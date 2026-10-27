//! Temporary, capability-invited timeline sharing between independent players.
//!
//! This endpoint has no sync, pairing, package or remote-player protocol. Nothing
//! is persisted. Each participant resolves its own media and keeps all source
//! URLs, credentials, audio/subtitle selection and preferences local.
mod engine;
mod service;
mod ticket;
mod wire;

use nova_playback_api::MediaIdentity;
use serde::{Deserialize, Serialize};
pub use service::PartyService;
use std::sync::LazyLock;
use std::time::Instant;

pub const ALPN: &[u8] = b"nova/party/6";
pub const MAX_MEMBERS: usize = 8;
/// Opaque SHA-256 asset hint; contains no source URL, path, headers or provider token.
pub type SourceFingerprint = [u8; 32];
pub const BUFFER_READY_SECONDS: f64 = 2.0;
pub const MIN_TARGET_RATE: f64 = 0.5;
pub const MAX_TARGET_RATE: f64 = 2.0;
pub const TARGET_RATE_STEP: f64 = 0.05;
pub const CATCH_UP_MULTIPLIER: f64 = 1.03;
pub const START_LEAD_MS: u64 = 1_000;
pub const HOST_TIMEOUT_MS: u64 = 30_000;

static CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Shared process-monotonic timebase, unrelated to wall-clock changes.
pub fn local_now_ms() -> u64 {
    CLOCK.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartyMedia {
    pub identity: MediaIdentity,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub duration: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalReport {
    pub source_fingerprint: Option<SourceFingerprint>,
    pub generation: u64,
    /// Identity bound by the local adapter after resolving its own source.
    pub media: Option<MediaIdentity>,
    pub loaded: bool,
    pub ended: bool,
    pub seekable: bool,
    pub position: f64,
    pub duration: f64,
    pub buffered_ahead: f64,
    pub buffering: bool,
    pub ready: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyRole {
    Host,
    Guest,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub position: f64,
    /// Host-monotonic anchor; a future anchor schedules coordinated resume.
    pub anchor_ms: u64,
    pub paused: bool,
    /// Room-scoped base speed, independent of transient local drift correction.
    #[serde(default = "default_target_rate")]
    pub target_rate: f64,
}

fn default_target_rate() -> f64 {
    1.0
}

impl Timeline {
    pub fn position_at(&self, host_now_ms: f64) -> f64 {
        self.position
            + if self.paused {
                0.0
            } else {
                ((host_now_ms - self.anchor_ms as f64) / 1_000.0).max(0.0) * self.target_rate
            }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    /// Bound by the host to the snapshot's current media generation.
    pub source_fingerprint: Option<SourceFingerprint>,
    pub id: String,
    pub name: String,
    pub is_host: bool,
    pub connected: bool,
    pub ready: bool,
    pub buffering: bool,
    pub position: f64,
    pub buffered_ahead: f64,
    pub reason: Option<String>,
}

/// The creator alone can change this live room-wide policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlPolicy {
    #[default]
    HostOnly,
    Everyone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlRejectionReason {
    PermissionDenied,
    StaleGeneration,
    StaleControlRevision,
    Duplicate,
    RateLimited,
    InvalidIntent,
    NoMedia,
    Ended,
}

/// A business rejection refreshes room state and keeps membership alive.
#[derive(Clone, Debug)]
pub struct ControlRejection {
    pub reason: ControlRejectionReason,
    pub snapshot: Box<RoomSnapshot>,
}

impl std::fmt::Display for ControlRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.reason {
            ControlRejectionReason::PermissionDenied => {
                "only the party creator can control playback"
            }
            ControlRejectionReason::StaleGeneration => "party media changed",
            ControlRejectionReason::StaleControlRevision => "party control permissions changed",
            ControlRejectionReason::Duplicate => "party command already handled",
            ControlRejectionReason::RateLimited => "party commands too frequent",
            ControlRejectionReason::InvalidIntent => "invalid party command",
            ControlRejectionReason::NoMedia => "choose party media first",
            ControlRejectionReason::Ended => "party has ended",
        })
    }
}

impl std::error::Error for ControlRejection {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub role: PartyRole,
    pub room_id: String,
    pub host_id: String,
    pub member_id: String,
    /// No shared selection while the host is choosing what to watch.
    pub media: Option<PartyMedia>,
    /// Episode announced while everyone independently chooses a source.
    /// Duration is zero until the host loads and validates its stream.
    pub pending_media: Option<PartyMedia>,
    pub generation: u64,
    pub revision: u64,
    #[serde(default)]
    pub control_policy: ControlPolicy,
    /// Changes only with policy, independently of clock/report ticks.
    #[serde(default)]
    pub control_revision: u64,
    pub timeline: Timeline,
    pub participants: Vec<Participant>,
    pub waiting: Vec<String>,
    pub wait_for_guests: bool,
    pub connected: bool,
    pub ended: bool,
    pub error: Option<String>,
    /// host clock minus local clock, estimated at the RTT midpoint.
    pub clock_offset_ms: f64,
}

impl RoomSnapshot {
    pub fn selected_media(&self) -> Option<&PartyMedia> {
        self.pending_media.as_ref().or(self.media.as_ref())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostCommand {
    SetPaused(bool),
    SeekAbsolute(f64),
    SeekRelative(f64),
    SetRate(f64),
    /// Exactly one 0.05 step in direction -1 or +1, resolved by the host.
    StepRate(i32),
    ResetRate,
}

/// Ephemeral player adjustments; callers must not persist `rate` as a preference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriftCorrection {
    pub paused: bool,
    pub seek_to: Option<f64>,
    pub rate: f64,
}

/// Compute all adjustments together so a seek never loses a simultaneous pause.
pub fn drift_correction(room: &RoomSnapshot, local: &LocalReport, now_ms: u64) -> DriftCorrection {
    let Some(media) = &room.media else {
        return DriftCorrection {
            paused: true,
            seek_to: None,
            rate: room.timeline.target_rate,
        };
    };
    let host_now = now_ms as f64 + room.clock_offset_ms;
    let matching = engine::media_matches(&media.identity, local.media.as_ref());
    let compatible = matching
        && local.generation == room.generation
        && engine::duration_matches(media.duration, local.duration);
    let paused = room.ended
        || !room.connected
        || !compatible
        || !engine::ready_for_media(media, room.generation, room.timeline.target_rate, local)
        || room.timeline.paused
        || host_now < room.timeline.anchor_ms as f64;
    let target = room
        .timeline
        .position_at(host_now)
        .clamp(0.0, media.duration);
    let delta = target - local.position;
    let base = room.timeline.target_rate;
    // Playing tolerances describe wall time at the agreed base speed. Using
    // the temporary corrected native speed would make classification oscillate.
    let gap = if paused {
        delta.abs()
    } else {
        delta.abs() / base
    };
    // Subtracting two large positions can put an exact boundary a few ULPs
    // beyond the limit. This only absorbs subtraction/normalization roundoff.
    let rounding_tolerance = 4.0 * f64::EPSILON * (target.abs() + local.position.abs()).max(1.0)
        / if paused { 1.0 } else { base };
    let seek_threshold = if paused { 0.15 } else { 1.25 };
    let seek_to = (compatible
        && local.loaded
        && !local.ended
        && local.seekable
        && gap > seek_threshold + rounding_tolerance)
        .then_some(target);
    let rate = if paused || seek_to.is_some() || gap <= 0.15 + rounding_tolerance {
        base
    } else if delta > 0.0 {
        base * CATCH_UP_MULTIPLIER
    } else {
        base * 0.97
    };
    DriftCorrection {
        paused,
        seek_to,
        rate,
    }
}

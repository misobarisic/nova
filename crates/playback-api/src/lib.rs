//! Ephemeral paired-device playback messages. Sources, credentials, paths and
//! provider settings stay on the receiving device; offers expose opaque IDs.
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId {
    /// Process boot epoch; never reuse sessions after a receiver restart.
    pub epoch: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaIdentity {
    pub media_type: String,
    pub title_id: String,
    pub episode_id: String,
    pub title: String,
    pub episode: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlaybackAction {
    TogglePause,
    SetPaused(bool),
    SeekRelative(f64),
    SeekAbsolute(f64),
    SetVolume(f64),
    VolumeStep(f64),
    SystemVolumeStep(i32),
    SetMuted(bool),
    ToggleMute,
    SetSpeed(f64),
    SpeedStep(i32),
    BeginSpeedPreview,
    EndSpeedPreview,
    SelectAudioTrack(i32),
    SelectSubtitleTrack(i32),
    ResetAudioTrack,
    ResetSubtitleTrack,
    ResetSpeed,
    ToggleFullscreen,
    SetFullscreen(bool),
    Screenshot,
    Stop,
    Resume(bool),
    CancelResume,
    NextEpisode { token: String },
    DismissNextEpisode { token: String },
    SetDecoder(i32),
    SetBrightness(f64),
    BrightnessStep(f64),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Snapshot,
    Lookup(MediaIdentity),
    Start {
        offer_id: String,
        expected_session: Option<SessionId>,
    },
    Command {
        session: SessionId,
        action: PlaybackAction,
    },
    /// Also dispatched locally when a controller connection is lost.
    ReleasePreview,
}

// Keep the public callback contract ergonomic; the 256 KiB transport bound
// limits payload size independently of this small inline snapshot.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Snapshot(PlaybackSnapshot),
    Offers(Vec<PlaybackOffer>),
    Accepted,
    Error(PlaybackError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackError {
    Unavailable,
    Unauthorized,
    StaleSession,
    InvalidRequest,
    NotFound,
    Busy,
    Timeout,
    Unsupported,
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackRole {
    #[default]
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackPhase {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaybackCapabilities {
    pub pause: bool,
    pub seek: bool,
    pub volume: bool,
    pub mute: bool,
    pub speed: bool,
    pub tracks: bool,
    pub fullscreen: bool,
    pub screenshot: bool,
    pub stop: bool,
    pub resume: bool,
    pub next_episode: bool,
    pub decoder: bool,
    pub brightness: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackTrack {
    pub id: i32,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaybackSnapshot {
    /// Headless sync jobs and apps without a registered UI adapter are unavailable.
    pub ready: bool,
    pub role: PlaybackRole,
    pub session: Option<SessionId>,
    pub title: String,
    pub episode: String,
    pub phase: PlaybackPhase,
    pub paused: bool,
    pub time: f64,
    pub duration: f64,
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub fullscreen: bool,
    pub audio_tracks: Vec<PlaybackTrack>,
    pub subtitle_tracks: Vec<PlaybackTrack>,
    pub audio_track: i32,
    pub subtitle_track: i32,
    pub resume_prompt: bool,
    pub next_episode_prompt: bool,
    pub next_episode_token: String,
    pub next_episode_media: Option<MediaIdentity>,
    pub screenshot_busy: bool,
    pub screenshot_saved: bool,
    pub screenshot_failed: bool,
    pub capabilities: PlaybackCapabilities,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackOffer {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Receiver-formatted display size, bounded to 32 bytes; empty if unknown.
    pub size: String,
}

/// Authenticated connection identity, never supplied by the remote message.
#[derive(Clone)]
pub struct PeerContext {
    pub peer_id: String,
    pub request_id: u64,
    pub requester_epoch: u64,
    /// Receiver-local connection identity; zero denotes membership cleanup.
    pub connection_id: u64,
    authorization: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl PeerContext {
    pub fn new(
        peer_id: String,
        request_id: u64,
        requester_epoch: u64,
        authorization: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            peer_id,
            request_id,
            requester_epoch,
            connection_id: 0,
            authorization,
        }
    }
    pub fn with_connection_id(mut self, connection_id: u64) -> Self {
        self.connection_id = connection_id;
        self
    }
    /// Recheck on the UI thread and after asynchronous work: pairing can be
    /// revoked after the transport accepted the original request.
    pub fn is_authorized(&self) -> bool {
        (self.authorization)()
    }
}

pub type Completion = Box<dyn FnOnce(Response) + Send>;
/// The adapter schedules work on its owning UI thread and completes later.
/// It must return promptly, without blocking the transport runtime.
pub type PlaybackHandler = Arc<dyn Fn(PeerContext, Request, Completion) + Send + Sync>;

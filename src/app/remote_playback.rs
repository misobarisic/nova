//! Live playback is a receiver-owned service, separate from persistent sync.
//! The UI thread serializes both roles and commands; workers only carry messages.
use super::*;
use nova_playback_api::{
    Completion, MediaIdentity, PeerContext, PlaybackAction, PlaybackCapabilities, PlaybackError,
    PlaybackOffer, PlaybackPhase, PlaybackRole, PlaybackSnapshot, PlaybackTrack, Request, Response,
    SessionId,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const REMOTE_OFFER_LIMIT: usize = 256;

/// Addon labels are untrusted presentation data and sometimes fall back to the
/// source URL. Keep only display text, with a byte budget for the wire frame.
fn remote_display_text(value: &str, max_bytes: usize) -> String {
    let mut output = String::new();
    for line in value.lines() {
        let lower = line.to_ascii_lowercase();
        if [
            "authorization",
            "bearer ",
            "token=",
            "api_key",
            "apikey",
            "password=",
            "secret=",
        ]
        .iter()
        .any(|key| lower.contains(key))
        {
            continue;
        }
        let words = line
            .split_whitespace()
            .filter(|word| {
                let word =
                    word.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '"' | '\''));
                !word.contains("://")
                    && !word.to_ascii_lowercase().starts_with("magnet:")
                    && !word.starts_with('/')
                    && !word.starts_with('\\')
                    && !word.contains(":\\")
                    && !word.starts_with("./")
                    && !word.starts_with("../")
            })
            .collect::<Vec<_>>()
            .join(" ");
        if words.is_empty() {
            continue;
        }
        if !output.is_empty() && output.len() < max_bytes {
            output.push('\n');
        }
        for character in words.chars() {
            if output.len() + character.len_utf8() > max_bytes {
                return output;
            }
            output.push(character);
        }
    }
    output
}

/// Native IDs take precedence over aliases claimed by other episodes. A
/// confirmed alias is usable only when exactly one receiver episode claims it;
/// metadata order must never decide which episode the controller plays.
fn remote_episode<'a>(videos: &'a [Video], requested: &str) -> Option<&'a Video> {
    if let Some(video) = videos.iter().find(|video| video.id == requested) {
        return Some(video);
    }
    let mut matches = videos.iter().filter(|video| {
        confirmed_episode_stream_ids(video)
            .iter()
            .any(|id| id == requested)
    });
    let video = matches.next()?;
    matches.next().is_none().then_some(video)
}

static FOREGROUND: AtomicBool = AtomicBool::new(true);

pub(super) fn is_foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}
static BOOT: LazyLock<u64> = LazyLock::new(|| {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
});

#[derive(Default, Debug, PartialEq)]
enum Role {
    #[default]
    Idle,
    Local,
    Remote,
}

type RemoteLookupRows = Vec<(StreamUi, u64)>;
struct RemoteLookupBatch {
    remaining: usize,
    rows: Vec<Option<RemoteLookupRows>>,
    done: Option<Completion>,
}
impl RemoteLookupBatch {
    fn push(
        &mut self,
        index: usize,
        rows: RemoteLookupRows,
    ) -> Option<(RemoteLookupRows, Completion)> {
        if self.done.is_none() || self.rows.get(index)?.is_some() {
            return None;
        }
        self.rows[index] = Some(rows);
        self.remaining -= 1;
        if self.remaining != 0 {
            return None;
        }
        let rows = self
            .rows
            .iter_mut()
            .flat_map(|row| row.take().unwrap_or_default())
            .collect();
        Some((rows, self.done.take()?))
    }
}

struct Offer {
    peer: String,
    media: MediaIdentity,
    stream: StreamUi,
    created: Instant,
    settings_generation: u64,
    addon_generation: u64,
}

struct CachedRemoteLookup {
    media: MediaIdentity,
    offers: Vec<PlaybackOffer>,
    started: Instant,
}

struct PendingReplacement {
    token: String,
    offer: String,
    session: Option<SessionId>,
}

/// Discovery and control share retry policy, but keep independent attempt clocks.
/// A live controller must not speed up a whole paired-device fan-out.
#[derive(Default)]
struct PeerPoll {
    attempted: Option<Instant>,
    failures: u8,
    snapshot: Option<PlaybackSnapshot>,
}

impl PeerPoll {
    fn retry_delay(&self) -> Duration {
        Duration::from_secs(match self.failures {
            0 => 0,
            1 => 5,
            2 => 15,
            3 => 30,
            _ => 60,
        })
    }

    fn ready(&self, now: Instant) -> bool {
        self.attempted
            .is_none_or(|at| now.saturating_duration_since(at) >= self.retry_delay())
    }

    fn complete(&mut self, snapshot: Option<PlaybackSnapshot>) {
        self.failures = if snapshot.as_ref().is_some_and(|snapshot| snapshot.ready) {
            0
        } else {
            self.failures.saturating_add(1)
        };
        self.snapshot = snapshot;
    }
}

fn poll_due(at: Option<Instant>, now: Instant, interval: Duration) -> bool {
    at.is_none_or(|at| now.saturating_duration_since(at) >= interval)
}

fn control_poll_interval(paused: bool, interacting: bool) -> Duration {
    if paused && !interacting {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(250)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum DiscoveryView {
    #[default]
    Hidden,
    Home,
    Picker,
}

impl DiscoveryView {
    fn interval(self, playing: bool) -> Option<Duration> {
        match self {
            Self::Hidden => None,
            Self::Home if !playing => Some(Duration::from_secs(30)),
            _ => Some(Duration::from_secs(5)),
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    role: Role,
    pub(super) receiver_videos: Vec<Video>,
    pub(super) screenshot_saved: bool,
    pub(super) screenshot_failed: bool,
    sequence: u64,
    session: Option<SessionId>,
    pub(super) start_context: Option<MediaIdentity>,
    offers: HashMap<String, Offer>,
    offer_sequence: u64,
    target: String,
    generation: u64,
    snapshot: Option<PlaybackSnapshot>,
    remote_offers: Vec<PlaybackOffer>,
    selector_lookups: HashMap<String, CachedRemoteLookup>,
    selector_engine: Option<std::sync::Weak<nova_sync::SyncEngine>>,
    visible_offers: Vec<PlaybackOffer>,
    remote_filter: usize,
    remote_page: usize,
    queued: VecDeque<Request>,
    in_flight: bool,
    lookup_in_flight: bool,
    discovery_running: bool,
    discovery_at: Option<Instant>,
    discovery_view: DiscoveryView,
    discovery_forced: bool,
    discovery_token: u64,
    discovered: HashMap<String, PeerPoll>,
    polling_engine: Option<std::sync::Weak<nova_sync::SyncEngine>>,
    control_poll: PeerPoll,
    interacting_until: Option<Instant>,
    replacement: Option<PendingReplacement>,
    preview_owner: Option<String>,
    preview_epoch: u64,
    preview_connection: u64,
    preview_seen: Option<Instant>,
}

impl State {
    fn control_request_due(&self, now: Instant) -> bool {
        let interacting = self.interacting_until.is_some_and(|until| now < until);
        let interval = control_poll_interval(
            self.snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.paused),
            interacting,
        );
        !self.queued.is_empty()
            || (self.control_poll.ready(now)
                && poll_due(self.control_poll.attempted, now, interval))
    }

    fn reconcile_polling_engine(&mut self, engine: &Arc<nova_sync::SyncEngine>) -> bool {
        if self
            .polling_engine
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|previous| Arc::ptr_eq(&previous, engine))
        {
            return false;
        }
        self.polling_engine = Some(Arc::downgrade(engine));
        self.invalidate_polling_engine();
        true
    }

    fn invalidate_polling_engine(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.queued.clear();
        self.in_flight = false;
        self.lookup_in_flight = false;
        self.snapshot = None;
        self.remote_offers.clear();
        self.visible_offers.clear();
        self.replacement = None;
        self.control_poll = PeerPoll::default();
        self.discovered.clear();
        self.discovery_at = None;
        // Keep the running guard until the old fan-out completes. Its engine
        // check rejects the result without allowing overlapping fan-outs.
    }

    fn discovery_peers(&mut self, peers: &[String], now: Instant, force: bool) -> Vec<String> {
        self.discovered.retain(|peer, _| peers.contains(peer));
        let mut due = peers
            .iter()
            .filter(|peer| {
                force
                    || self
                        .discovered
                        .get(*peer)
                        .is_none_or(|poll| poll.ready(now))
            })
            .cloned()
            .collect::<Vec<_>>();
        // Oldest attempts first keeps paired devices beyond the fan-out cap
        // from starving, while retaining the latest status for skipped peers.
        due.sort_by_key(|peer| self.discovered.get(peer).and_then(|poll| poll.attempted));
        due.truncate(32);
        for peer in &due {
            self.discovered.entry(peer.clone()).or_default().attempted = Some(now);
        }
        due
    }

    fn cached_selector_offers(
        &self,
        target: &str,
        media: &MediaIdentity,
    ) -> Option<Vec<PlaybackOffer>> {
        let cached = self.selector_lookups.get(target)?;
        (cached.media == *media && cached.started.elapsed() < Duration::from_secs(300))
            .then(|| cached.offers.clone())
    }

    fn reconcile_selector_cache(&mut self, engine: &Arc<nova_sync::SyncEngine>) {
        let same_engine = self
            .selector_engine
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|previous| Arc::ptr_eq(&previous, engine));
        if !same_engine {
            self.selector_lookups.clear();
            self.selector_engine = Some(Arc::downgrade(engine));
        }
        let peers = engine.peers();
        self.selector_lookups.retain(|peer, cached| {
            peers.contains(peer) && cached.started.elapsed() < Duration::from_secs(300)
        });
    }

    pub(super) fn can_play_locally(&self) -> bool {
        self.role != Role::Remote
    }
    pub(super) fn begin(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        self.session = Some(SessionId {
            epoch: *BOOT,
            sequence: self.sequence,
        });
        self.role = Role::Local;
    }
    pub(super) fn ended(&mut self) {
        if self.role == Role::Local {
            self.role = Role::Idle;
        }
        self.session = None;
        self.preview_owner = None;
        self.preview_seen = None;
        self.receiver_videos.clear();
        self.screenshot_saved = false;
        self.screenshot_failed = false;
    }
}

#[cfg(any(target_os = "android", test))]
pub(crate) fn playback_foreground(active: bool) {
    FOREGROUND.store(active, Ordering::Release);
}

fn session_text(session: Option<SessionId>) -> String {
    session
        .map(|s| format!("{}:{}", s.epoch, s.sequence))
        .unwrap_or_default()
}

fn error_text(error: &PlaybackError) -> String {
    // Do not display provider errors which could include source URLs or secrets.
    text::tr(match error {
        PlaybackError::Unavailable => "Open Nova on the playing device to connect.",
        PlaybackError::Unauthorized => "This device is no longer paired.",
        PlaybackError::StaleSession => "Playback changed. Refresh the controls and try again.",
        PlaybackError::Busy => "Leave remote control before starting playback on this device.",
        PlaybackError::NotFound => "This selection expired. Reload streams.",
        PlaybackError::Unsupported => "This control is unavailable on the playing device.",
        _ => "Could not contact the playing device. Try again.",
    })
    .to_string()
}

fn remote_phase_text(snapshot: &PlaybackSnapshot) -> &'static str {
    text::tr(if snapshot.screenshot_busy {
        "Capturing screenshot…"
    } else if snapshot.screenshot_saved {
        "Screenshot saved on the playing device"
    } else if snapshot.screenshot_failed {
        "Screenshot failed on the playing device"
    } else {
        match snapshot.phase {
            PlaybackPhase::Loading => "Loading stream…",
            PlaybackPhase::Playing => "Playing",
            PlaybackPhase::Paused => "Paused",
            PlaybackPhase::Failed => "Playback failed",
            _ => "No active playback",
        }
    })
}

fn remote_prompt_text(snapshot: &PlaybackSnapshot) -> &'static str {
    text::tr(if snapshot.resume_prompt {
        "Resume playback?"
    } else if snapshot.next_episode_prompt {
        "Choose streams for the next episode?"
    } else {
        ""
    })
}

impl Bridge {
    fn reconcile_remote_polling_engine(&self, engine: &Arc<nova_sync::SyncEngine>) {
        let changed = self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .reconcile_polling_engine(engine);
        if !changed {
            return;
        }
        let Some(app) = self.app() else { return };
        app.set_remote_connected(false);
        app.set_remote_has_playing_devices(false);
        app.set_remote_has_ready_devices(false);
        app.set_remote_devices(Default::default());
        app.set_remote_replacement_token(SharedString::default());
        app.set_remote_offers(Default::default());
        if app.get_stream_selector_open() && self.remote_output_selected() {
            // The old lookup completion is deliberately rejected. Release its
            // loading state so Reload can start a lookup against this engine.
            app.set_streams_searching(false);
            app.set_streams(Default::default());
            app.set_stream_total(0);
            app.set_streams_hint(error_text(&PlaybackError::Unavailable).into());
        }
        if app.get_remote_devices_open() || app.get_remote_controls_open() {
            app.recover_back_focus();
        }
    }

    fn remote_control_enabled(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .cache_settings
            .remote_control_enabled
    }

    pub(super) fn wire_remote_playback(&self) {
        let Some(app) = self.app() else { return };
        let b = self.clone();
        app.on_remote_device_refresh(move || b.refresh_playback_devices());
        let b = self.clone();
        app.on_remote_device_open(move |id| b.open_remote_controls(id.as_str()));
        let b = self.clone();
        app.on_remote_device_selected(move |id| b.select_playback_output(id.as_str()));
        let b = self.clone();
        app.on_remote_device_close(move || b.remote_disconnect());
        let b = self.clone();
        app.on_remote_action(move |action, value, token| {
            b.remote_ui_action(action.as_str(), value, token.as_str())
        });
        let b = self.clone();
        app.on_remote_stream_picked(move |index| {
            b.remote_stream_selected(index.max(0) as usize);
        });
        let b = self.clone();
        app.on_remote_replacement_confirmed(move |token, accept| {
            b.confirm_remote_replacement(token.as_str(), accept)
        });
    }

    pub(super) fn attach_playback_service(&self, engine: &Arc<nova_sync::SyncEngine>) {
        if !self.remote_control_enabled() {
            engine.set_playback_handler(None);
            return;
        }
        let bridge = self.clone();
        engine.set_playback_handler(Some(Arc::new(move |peer, request, done| {
            let bridge = bridge.clone();
            // Completion drops if the app event loop has shut down; the transport
            // times out. A headless job never installs this adapter.
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge.receive_playback_request(peer, request, done)
            });
        })));
    }

    /// Revoke queued receiver work immediately without stopping local playback
    /// or the independent sync service. Reattaching advances the adapter epoch,
    /// so work accepted before an opt-out cannot become valid after re-enabling.
    pub(super) fn remote_control_policy_changed(&self, enabled: bool) {
        if let Some(engine) = nova_sync::engine() {
            if enabled {
                self.attach_playback_service(&engine);
            } else {
                engine.set_playback_handler(None);
            }
        }
        if !enabled {
            let release = {
                let mut state = self.shared.lock().unwrap();
                let remote = &mut state.remote_playback;
                remote.offers.clear();
                let release = remote
                    .preview_owner
                    .as_deref()
                    .is_some_and(|owner| owner != "local");
                if release {
                    remote.preview_owner = None;
                    remote.preview_seen = None;
                }
                release
            };
            if release {
                let _ = self.execute_playback_action(PlaybackAction::EndSpeedPreview);
            }
        }
    }

    /// A held preview has one input owner. Local release cannot cancel a
    /// remote hold, and a disconnected old connection cannot cancel a new one.
    pub(super) fn prepare_speed_preview(
        &self,
        action: &PlaybackAction,
        peer: Option<&PeerContext>,
    ) -> Result<bool, String> {
        if !matches!(
            action,
            PlaybackAction::BeginSpeedPreview | PlaybackAction::EndSpeedPreview
        ) {
            return Ok(true);
        }
        let owner = peer.map(|p| p.peer_id.as_str()).unwrap_or("local");
        let epoch = peer.map(|p| p.requester_epoch).unwrap_or(0);
        let connection = peer.map(|p| p.connection_id).unwrap_or(0);
        let mut state = self.shared.lock().unwrap();
        let r = &mut state.remote_playback;
        let matches = r.preview_owner.as_deref() == Some(owner)
            && r.preview_epoch == epoch
            && r.preview_connection == connection;
        if *action == PlaybackAction::BeginSpeedPreview {
            // A current authenticated connection from the same paired owner
            // can reclaim its hold before the old connection's cleanup runs.
            // End and cleanup still require the exact original lease.
            if r.preview_owner
                .as_deref()
                .is_some_and(|current| current != owner)
            {
                return Err("Speed preview belongs to another input".into());
            }
            if !self.app().is_some_and(|app| app.get_playback_started()) {
                return Err("Playback has not started".into());
            }
            r.preview_owner = Some(owner.into());
            r.preview_epoch = epoch;
            r.preview_connection = connection;
            r.preview_seen = peer.map(|_| Instant::now());
            Ok(true)
        } else if matches || (peer.is_none() && r.preview_owner.is_none()) {
            r.preview_owner = None;
            r.preview_seen = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[cfg(target_os = "android")]
    pub(super) fn remote_suspend(&self) {
        let _ = self.execute_playback_action(PlaybackAction::EndSpeedPreview);
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.preview_owner = None;
            state.remote_playback.preview_seen = None;
        }
        self.remote_disconnect();
    }

    pub(super) fn playback_closed(&self) {
        self.shared.lock().unwrap().remote_playback.ended();
    }

    /// Party ownership supersedes any old local or paired-controller hold.
    pub(super) fn release_speed_preview_for_party(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.preview_owner = None;
            state.remote_playback.preview_seen = None;
        }
        let _ = self.execute_playback_action(PlaybackAction::EndSpeedPreview);
    }

    fn playback_snapshot(&self) -> PlaybackSnapshot {
        let Some(app) = self.app() else {
            return PlaybackSnapshot::default();
        };
        let state = self.shared.lock().unwrap();
        // Wait for a local profile choice before exposing its playback state.
        if !state.cache_settings.remote_control_enabled || app.get_profiles_require_selection() {
            return PlaybackSnapshot::default();
        }
        let remote = &state.remote_playback;
        let started = app.get_playback_started();
        let active = remote.session.is_some();
        let ready = FOREGROUND.load(Ordering::Acquire)
            && remote.role != Role::Remote
            && !state.party.active();
        let tracks = |model: slint::ModelRc<crate::TrackRow>| {
            model
                .iter()
                .take(64)
                .map(|row| PlaybackTrack {
                    id: row.index,
                    label: remote_display_text(&format!("{} {}", row.title, row.lang), 256),
                })
                .collect()
        };
        #[cfg(target_os = "android")]
        let volume = android_player::player_volume() as f64;
        #[cfg(not(target_os = "android"))]
        let volume = app.get_volume() as f64;
        PlaybackSnapshot {
            ready,
            role: if remote.role == Role::Remote {
                PlaybackRole::Remote
            } else {
                PlaybackRole::Local
            },
            session: remote.session,
            title: remote_display_text(app.get_player_show_title().as_str(), 512),
            episode: remote_display_text(app.get_player_episode_title().as_str(), 512),
            phase: if !active {
                PlaybackPhase::Idle
            } else if self.player.playback_failed() {
                PlaybackPhase::Failed
            } else if !started {
                PlaybackPhase::Loading
            } else if app.get_is_paused() {
                PlaybackPhase::Paused
            } else {
                PlaybackPhase::Playing
            },
            paused: app.get_is_paused(),
            time: app.get_position() as f64,
            duration: app.get_duration() as f64,
            volume,
            muted: app.get_muted(),
            speed: self.player.effective_playback_speed() as f64,
            fullscreen: app.get_is_fullscreen(),
            audio_tracks: tracks(app.get_audio_tracks()),
            subtitle_tracks: tracks(app.get_sub_tracks()),
            audio_track: self.player.selected_track_id(true),
            subtitle_track: self.player.selected_track_id(false),
            resume_prompt: app.get_resume_prompt_visible(),
            next_episode_prompt: app.get_next_episode_visible(),
            next_episode_token: app.get_next_episode_token().to_string(),
            next_episode_media: state.playback.as_ref().and_then(|p| {
                if !app.get_next_episode_visible()
                    || p.series_id.len() > 1024
                    || p.next_episode_id.len() > 1024
                {
                    return None;
                }
                let video = state
                    .modal_item
                    .as_ref()
                    .filter(|m| m.id == p.series_id)
                    .map(|m| &m.videos)
                    .unwrap_or(&remote.receiver_videos)
                    .iter()
                    .find(|v| v.id == p.next_episode_id)?;
                Some(MediaIdentity {
                    media_type: "series".into(),
                    title_id: p.series_id.clone(),
                    episode_id: video.id.clone(),
                    title: remote_display_text(app.get_player_show_title().as_str(), 512),
                    episode: remote_display_text(&stream_episode_context_label(video), 512),
                })
            }),
            screenshot_busy: app.get_screenshot_busy(),
            screenshot_saved: remote.screenshot_saved,
            screenshot_failed: remote.screenshot_failed,
            capabilities: PlaybackCapabilities {
                pause: started,
                seek: started,
                volume: started,
                mute: started,
                speed: started,
                tracks: started,
                fullscreen: active && !cfg!(target_os = "android"),
                screenshot: started && !app.get_screenshot_busy(),
                stop: active,
                resume: app.get_resume_prompt_visible(),
                next_episode: app.get_next_episode_visible(),
                decoder: started && cfg!(target_os = "android"),
                brightness: started && cfg!(target_os = "android"),
            },
            revision: remote.sequence,
        }
    }

    fn receive_playback_request(&self, peer: PeerContext, request: Request, done: Completion) {
        // Revocation and disconnection cleanup only releases this controller's
        // held preview, and must still run after its authority has expired.
        if matches!(request, Request::ReleasePreview) {
            let owned = {
                let mut state = self.shared.lock().unwrap();
                if state.remote_playback.preview_owner.as_deref() == Some(&peer.peer_id)
                    && (peer.requester_epoch == 0
                        || state.remote_playback.preview_epoch == peer.requester_epoch)
                    && (peer.connection_id == 0
                        || state.remote_playback.preview_connection == peer.connection_id)
                {
                    state.remote_playback.preview_owner = None;
                    state.remote_playback.preview_seen = None;
                    true
                } else {
                    false
                }
            };
            if owned {
                let _ = self.execute_playback_action(PlaybackAction::EndSpeedPreview);
            }
            done(Response::Accepted);
            return;
        }
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        if !FOREGROUND.load(Ordering::Acquire)
            || self
                .app()
                .is_none_or(|app| app.get_profiles_require_selection())
        {
            done(Response::Error(PlaybackError::Unavailable));
            return;
        }
        if !matches!(request, Request::Snapshot) && self.party_active() {
            done(Response::Error(PlaybackError::Busy));
            return;
        }
        match request {
            Request::Snapshot => {
                let mut state = self.shared.lock().unwrap();
                if state.remote_playback.preview_owner.as_deref() == Some(&peer.peer_id)
                    && state.remote_playback.preview_epoch == peer.requester_epoch
                    && state.remote_playback.preview_connection == peer.connection_id
                {
                    state.remote_playback.preview_seen = Some(Instant::now());
                }
                drop(state);
                done(Response::Snapshot(self.playback_snapshot()));
            }
            Request::Lookup(media) => self.lookup_remote_streams(peer, media, done),
            Request::Start {
                offer_id,
                expected_session,
            } => done(self.start_remote_offer(&peer, &offer_id, expected_session)),
            Request::Command { session, action } => {
                if self.shared.lock().unwrap().remote_playback.session != Some(session) {
                    done(Response::Error(PlaybackError::StaleSession));
                    return;
                }
                if !self.playback_snapshot().ready {
                    done(Response::Error(PlaybackError::Busy));
                    return;
                }
                let caps = self.playback_snapshot().capabilities;
                let supported = match &action {
                    PlaybackAction::Stop => caps.stop,
                    PlaybackAction::Resume(_) | PlaybackAction::CancelResume => caps.resume,
                    PlaybackAction::NextEpisode { token }
                    | PlaybackAction::DismissNextEpisode { token } => {
                        caps.next_episode
                            && self
                                .app()
                                .is_some_and(|a| a.get_next_episode_token().as_str() == token)
                    }
                    PlaybackAction::TogglePause | PlaybackAction::SetPaused(_) => caps.pause,
                    PlaybackAction::SeekRelative(_) | PlaybackAction::SeekAbsolute(_) => caps.seek,
                    PlaybackAction::SetVolume(_)
                    | PlaybackAction::VolumeStep(_)
                    | PlaybackAction::SystemVolumeStep(_) => caps.volume,
                    PlaybackAction::SetMuted(_) | PlaybackAction::ToggleMute => caps.mute,
                    PlaybackAction::SetSpeed(_)
                    | PlaybackAction::SpeedStep(_)
                    | PlaybackAction::BeginSpeedPreview
                    | PlaybackAction::ResetSpeed => caps.speed,
                    PlaybackAction::EndSpeedPreview => true,
                    PlaybackAction::SelectAudioTrack(_)
                    | PlaybackAction::SelectSubtitleTrack(_)
                    | PlaybackAction::ResetAudioTrack
                    | PlaybackAction::ResetSubtitleTrack => caps.tracks,
                    PlaybackAction::ToggleFullscreen | PlaybackAction::SetFullscreen(_) => {
                        caps.fullscreen
                    }
                    PlaybackAction::Screenshot => caps.screenshot,
                    PlaybackAction::SetDecoder(_) => caps.decoder,
                    PlaybackAction::SetBrightness(_) | PlaybackAction::BrightnessStep(_) => {
                        caps.brightness
                    }
                };
                if !supported {
                    done(Response::Error(PlaybackError::Unsupported));
                    return;
                }
                // Track and prompt identifiers are meaningful only within this session.
                if let PlaybackAction::SelectAudioTrack(id)
                | PlaybackAction::SelectSubtitleTrack(id) = &action
                {
                    let snapshot = self.playback_snapshot();
                    let tracks = if matches!(action, PlaybackAction::SelectAudioTrack(_)) {
                        snapshot.audio_tracks
                    } else {
                        snapshot.subtitle_tracks
                    };
                    if *id != -1 && !tracks.iter().any(|track| track.id == *id) {
                        done(Response::Error(PlaybackError::InvalidRequest));
                        return;
                    }
                }
                done(
                    match self.dispatch_playback_action_from(action, Some(&peer)) {
                        Ok(()) => Response::Accepted,
                        Err(_) => Response::Error(PlaybackError::Unsupported),
                    },
                );
            }
            Request::ReleasePreview => unreachable!("handled before authority check"),
        }
    }

    fn lookup_remote_streams(&self, peer: PeerContext, mut media: MediaIdentity, done: Completion) {
        if media.title_id.is_empty()
            || media.title_id.len() > 1024
            || media.episode_id.len() > 1024
            || media.title.len() > 512
            || media.episode.len() > 512
            || !matches!(media.media_type.as_str(), "movie" | "series" | "anime")
        {
            done(Response::Error(PlaybackError::InvalidRequest));
            return;
        }
        if self.shared.lock().unwrap().remote_playback.role == Role::Remote {
            done(Response::Error(PlaybackError::Busy));
            return;
        }
        let cached =
            read_episodes_cache_for(&media.media_type, &media.title_id).unwrap_or_default();
        let header = read_meta_header_for(&media.media_type, &media.title_id);
        let urls = {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .filter(|a| {
                    a.enabled
                        && a.available
                        && a.manifest
                            .accepts("meta", &media.media_type, &media.title_id)
                })
                .filter_map(|a| Addon::new(&a.url).ok())
                .map(|a| a.meta_url(&media.media_type, &media.title_id))
                .collect::<Vec<_>>()
        };
        let cached_episode = remote_episode(&cached, &media.episode_id);
        if media.media_type == "movie" || cached_episode.is_some() {
            if let Some(video) = cached_episode {
                media.episode_id = video.id.clone();
            }
            self.lookup_remote_context(
                peer,
                media,
                cached,
                header.map(|h| h.year).unwrap_or_default(),
                done,
            );
        } else {
            self.lookup_remote_metadata(peer, media, urls.into(), done);
        }
    }

    fn lookup_remote_metadata(
        &self,
        peer: PeerContext,
        media: MediaIdentity,
        mut urls: VecDeque<String>,
        done: Completion,
    ) {
        let Some(url) = urls.pop_front() else {
            done(Response::Error(PlaybackError::NotFound));
            return;
        };
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        let b = self.clone();
        net::fetch_bytes(url, move |result| {
            let item = result
                .ok()
                .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                .filter(|item| meta_matches_request(item, &media.media_type, &media.title_id));
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                b.apply_remote_metadata(peer, media, urls, item, done);
            });
        });
    }

    fn apply_remote_metadata(
        &self,
        peer: PeerContext,
        mut media: MediaIdentity,
        urls: VecDeque<String>,
        item: Option<MetaItem>,
        done: Completion,
    ) {
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        if let Some(item) = item {
            let videos = write_episode_meta_cache_for(&media.media_type, &media.title_id, &item);
            // Reconciliation can preserve receiver-native IDs from an older
            // cache. Resolve against its result, with the same rules as cache hits.
            if let Some(video) = remote_episode(&videos, &media.episode_id) {
                media.episode_id = video.id.clone();
                self.lookup_remote_context(
                    peer,
                    media,
                    videos,
                    item.preview
                        .release_info
                        .as_ref()
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    done,
                );
                return;
            }
        }
        self.lookup_remote_metadata(peer, media, urls, done);
    }

    fn lookup_remote_context(
        &self,
        peer: PeerContext,
        media: MediaIdentity,
        videos: Vec<Video>,
        year: String,
        done: Completion,
    ) {
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        let context = ModalItem {
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: media.title_id.clone(),
            type_: media.media_type.clone(),
            request_id: media.episode_id.clone(),
            videos,
            season_backdrops: HashMap::new(),
            seasons: vec![],
            season_index: 0,
            episode_page: 0,
            name: media.title.clone(),
            year,
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        };
        let urls = {
            let state = self.shared.lock().unwrap();
            detail::stream_search_urls_for_context(&state.installed, &context, &media.episode_id)
        };
        self.lookup_remote_sources(peer, media, urls, done);
    }

    fn lookup_remote_sources(
        &self,
        peer: PeerContext,
        media: MediaIdentity,
        urls: Vec<(String, String, String, u64)>,
        done: Completion,
    ) {
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        let settings = nova_providers::plugin_settings::settings_generation();
        if urls.is_empty() {
            self.finish_remote_lookup(peer, media, Vec::new(), settings, done);
            return;
        }
        // Fetch concurrently, then flatten in receiver addon order. Waiting for
        // one provider must not postpone starting every other provider.
        let batch = Arc::new(Mutex::new(RemoteLookupBatch {
            remaining: urls.len(),
            rows: vec![None; urls.len()],
            done: Some(done),
        }));
        for (index, (label, url, owner, generation)) in urls.into_iter().enumerate() {
            let batch = batch.clone();
            let peer = peer.clone();
            let media = media.clone();
            let bridge = self.clone();
            net::fetch_bytes(url, move |result| {
                let streams = result
                    .ok()
                    .and_then(|bytes| Addon::parse_streams(&bytes).ok())
                    .unwrap_or_default();
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    if !peer.is_authorized() || !bridge.remote_control_enabled() {
                        if let Some(done) = batch.lock().unwrap().done.take() {
                            done(Response::Error(PlaybackError::Unauthorized));
                        }
                        return;
                    }
                    let current = {
                        let state = bridge.shared.lock().unwrap();
                        settings == nova_providers::plugin_settings::settings_generation()
                            && state.installed.iter().any(|a| {
                                a.url == owner
                                    && a.enabled
                                    && a.available
                                    && a.generation == generation
                            })
                    };
                    let rows = if current {
                        streams
                            .into_iter()
                            .take(80)
                            .filter_map(|stream| {
                                let source =
                                    stream_source_for_addon(&stream, &owner, generation, settings);
                                if matches!(source, StreamSource::Unsupported) {
                                    return None;
                                }
                                Some((
                                    StreamUi {
                                        id: String::new(),
                                        choice_id: Some(stream_choice_identity(
                                            &owner,
                                            &media.media_type,
                                            &media.episode_id,
                                            &stream,
                                        )),
                                        owner_url: owner.clone(),
                                        display: stream_display(&stream),
                                        card: stream_card_text(&stream),
                                        source,
                                        addon: label.clone(),
                                        download: None,
                                    },
                                    generation,
                                ))
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let completed = batch.lock().unwrap().push(index, rows);
                    if let Some((rows, done)) = completed {
                        bridge.finish_remote_lookup(peer, media, rows, settings, done);
                    }
                });
            });
        }
    }

    fn finish_remote_lookup(
        &self,
        peer: PeerContext,
        media: MediaIdentity,
        rows: Vec<(StreamUi, u64)>,
        settings_generation: u64,
        done: Completion,
    ) {
        if !peer.is_authorized() || !self.remote_control_enabled() {
            done(Response::Error(PlaybackError::Unauthorized));
            return;
        }
        // Earlier addon replies may already be buffered when settings change.
        // Never relabel their old account-dependent sources with the new epoch.
        if settings_generation != nova_providers::plugin_settings::settings_generation() {
            done(Response::Error(PlaybackError::NotFound));
            return;
        }
        let mut offers = Vec::new();
        // Receiver-owned completed artifacts stay ahead of addon streams.
        for job in self
            .downloads
            .jobs()
            .into_iter()
            .filter(|job| {
                job.media_id == media.title_id
                    && job.request_id == media.episode_id
                    && job.phase == crate::download::DownloadPhase::Completed
            })
            .take(REMOTE_OFFER_LIMIT)
        {
            if let Some(path) = job.artifact_path.clone().filter(|path| path.is_file()) {
                let stream = StreamUi {
                    id: String::new(),
                    choice_id: None,
                    owner_url: String::new(),
                    display: job.title.clone(),
                    card: downloaded_stream_card(&job),
                    source: StreamSource::Downloaded {
                        job_id: job.id,
                        path,
                    },
                    addon: text::tr("Downloaded").into(),
                    download: None,
                };
                offers.push(self.retain_remote_offer(
                    &peer.peer_id,
                    &media,
                    stream,
                    0,
                    settings_generation,
                ));
            }
        }
        for (stream, generation) in rows
            .into_iter()
            .take(REMOTE_OFFER_LIMIT.saturating_sub(offers.len()))
        {
            offers.push(self.retain_remote_offer(
                &peer.peer_id,
                &media,
                stream,
                generation,
                settings_generation,
            ));
        }
        done(Response::Offers(offers));
    }

    fn retain_remote_offer(
        &self,
        peer: &str,
        media: &MediaIdentity,
        stream: StreamUi,
        addon_generation: u64,
        settings_generation: u64,
    ) -> PlaybackOffer {
        let mut state = self.shared.lock().unwrap();
        let remote = &mut state.remote_playback;
        remote
            .offers
            .retain(|_, o| o.created.elapsed() < Duration::from_secs(300));
        if remote.offers.len() >= 2048
            && let Some(oldest) = remote
                .offers
                .iter()
                .min_by_key(|(_, o)| o.created)
                .map(|(id, _)| id.clone())
        {
            remote.offers.remove(&oldest);
        }
        remote.offer_sequence = remote.offer_sequence.wrapping_add(1);
        let id = format!("{}:{}", *BOOT, remote.offer_sequence);
        let offer = PlaybackOffer {
            id: id.clone(),
            name: remote_display_text(&stream.addon, 128),
            description: remote_display_text(&stream.display, 512),
            size: remote_display_text(&stream.card.size, 32),
        };
        remote.offers.insert(
            id,
            Offer {
                peer: peer.into(),
                media: media.clone(),
                stream,
                created: Instant::now(),
                settings_generation,
                addon_generation,
            },
        );
        offer
    }

    fn start_remote_offer(
        &self,
        peer: &PeerContext,
        id: &str,
        expected: Option<SessionId>,
    ) -> Response {
        let offer = {
            let mut state = self.shared.lock().unwrap();
            let r = &state.remote_playback;
            if r.role == Role::Remote {
                return Response::Error(PlaybackError::Busy);
            }
            if r.session != expected {
                return Response::Error(PlaybackError::StaleSession);
            }
            let Some(offer) = r.offers.get(id) else {
                return Response::Error(PlaybackError::NotFound);
            };
            if offer.peer != peer.peer_id
                || offer.created.elapsed() > Duration::from_secs(300)
                || offer.settings_generation
                    != nova_providers::plugin_settings::settings_generation()
                || (!offer.stream.owner_url.is_empty()
                    && !state.installed.iter().any(|a| {
                        a.url == offer.stream.owner_url
                            && a.enabled
                            && a.available
                            && a.generation == offer.addon_generation
                    }))
            {
                return Response::Error(PlaybackError::NotFound);
            }
            state.remote_playback.offers.remove(id).unwrap()
        };
        // Starting over is explicit compare-and-swap against the observed session.
        if expected.is_some() {
            let _ = self.dispatch_playback_action(PlaybackAction::Stop);
        }
        if let Some(identity) = offer.stream.choice_id {
            self.last_stream.remember(identity, now_secs());
        }
        self.shared.lock().unwrap().remote_playback.start_context = Some(offer.media);
        self.play_remote_source(offer.stream.source, &offer.stream.display);
        let snapshot = self.playback_snapshot();
        if snapshot.session.is_none() {
            Response::Error(PlaybackError::Failed(String::new()))
        } else {
            Response::Snapshot(snapshot)
        }
    }

    fn play_remote_source(&self, source: StreamSource, display: &str) {
        match source {
            StreamSource::Url(url) => {
                self.open_player(url);
            }
            StreamSource::UrlWithOptions {
                url,
                headers,
                subtitles,
            } => {
                self.open_player_with_options(url, headers, subtitles);
            }
            StreamSource::Resolver {
                provider_url,
                token,
                settings_generation,
                addon_generation,
            } => self.play_provider_resolver(
                provider_url,
                token,
                settings_generation,
                addon_generation,
            ),
            StreamSource::Torrent {
                info_hash,
                file_idx,
            } => self.play_torrent(display, info_hash, file_idx),
            StreamSource::Downloaded { job_id, .. } => self.play_download(&job_id),
            StreamSource::Unsupported => {}
        }
        self.shared.lock().unwrap().remote_playback.start_context = None;
    }

    pub(super) fn remote_disconnect(&self) {
        self.detach_remote_playback(false);
    }

    fn detach_remote_playback(&self, keep_selector_lookups: bool) {
        let target = {
            let mut state = self.shared.lock().unwrap();
            let r = &mut state.remote_playback;
            if !keep_selector_lookups {
                r.selector_lookups.clear();
                r.selector_engine = None;
            }
            r.generation = r.generation.wrapping_add(1);
            r.role = if r.session.is_some() {
                Role::Local
            } else {
                Role::Idle
            };
            r.snapshot = None;
            r.queued.clear();
            r.in_flight = false;
            r.lookup_in_flight = false;
            r.control_poll = PeerPoll::default();
            r.interacting_until = None;
            r.remote_offers.clear();
            r.visible_offers.clear();
            r.replacement = None;
            std::mem::take(&mut r.target)
        };
        if let Some(app) = self.app() {
            app.set_remote_controls_open(false);
            app.set_remote_connected(false);
            app.set_remote_loading(false);
            app.set_remote_target_id(SharedString::default());
            app.set_remote_session(SharedString::default());
            app.set_remote_replacement_token(SharedString::default());
        }
        if !target.is_empty()
            && let Some(app) = self.app()
        {
            app.set_streams(Rc::new(VecModel::from(Vec::<StreamRow>::new())).into());
            app.set_remote_offers(
                Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
            );
            app.set_stream_total(0);
            if app.get_stream_selector_open() {
                let request = self
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .map(|m| m.request_id.clone());
                if let Some(request) = request {
                    self.start_stream_search(request);
                }
            }
        }
        if !target.is_empty()
            && let Some(engine) = nova_sync::engine()
        {
            engine.disconnect_playback(&target);
        }
    }

    fn open_remote_controls(&self, target: &str) {
        if self.party_active() {
            return;
        }
        let Some(engine) = nova_sync::engine() else {
            return;
        };
        if !engine.peers().iter().any(|id| id == target) {
            return;
        }
        {
            let state = self.shared.lock().unwrap();
            if state.remote_playback.role == Role::Local
                || state
                    .playback_intent
                    .as_ref()
                    .is_some_and(|intent| intent.pending || intent.in_app)
                || self.app().is_some_and(|a| a.get_player_open())
            {
                return;
            }
        }
        self.remote_disconnect();
        self.reconcile_remote_polling_engine(&engine);
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.role = Role::Remote;
            state.remote_playback.target = target.into();
        }
        if let Some(app) = self.app() {
            let name = engine
                .peer_names()
                .get(target)
                .cloned()
                .unwrap_or_else(|| target.to_string());
            app.set_remote_target_id(target.into());
            app.set_remote_target_name(name.into());
            app.set_remote_devices_open(false);
            app.set_remote_error(SharedString::default());
            app.set_remote_loading(true);
            app.set_remote_controls_open(true);
            app.set_remote_offers(
                Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
            );
        }
        self.remote_tick();
    }

    pub(super) fn remote_output_selected(&self) -> bool {
        !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .target
            .is_empty()
    }
    pub(super) fn remote_reload_streams(&self) -> bool {
        let target = self.shared.lock().unwrap().remote_playback.target.clone();
        if target.is_empty() {
            return false;
        }
        self.shared
            .lock()
            .unwrap()
            .remote_playback
            .selector_lookups
            .remove(&target);
        self.select_playback_output(&target);
        true
    }

    fn select_playback_output(&self, target: &str) {
        if !target.is_empty() && self.party_active() {
            return;
        }
        if target.is_empty() {
            self.detach_remote_playback(true);
            let id = self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .map(|m| m.request_id.clone());
            if let Some(id) = id {
                self.start_stream_search(id);
            }
            return;
        }
        let Some(engine) = nova_sync::engine() else {
            return;
        };
        if !engine.peers().iter().any(|id| id == target) {
            return;
        }
        if self.shared.lock().unwrap().remote_playback.role == Role::Local {
            return;
        }
        self.detach_remote_playback(true);
        self.reconcile_remote_polling_engine(&engine);
        self.shared
            .lock()
            .unwrap()
            .remote_playback
            .reconcile_selector_cache(&engine);
        let (media, generation) = {
            let mut state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            let media = MediaIdentity {
                media_type: m.type_.clone(),
                title_id: m.id.clone(),
                episode_id: m.request_id.clone(),
                title: m.name.clone(),
                episode: self
                    .app()
                    .map(|a| a.get_episode_context().to_string())
                    .unwrap_or_default(),
            };
            state.remote_playback.target = target.into();
            state.remote_playback.role = Role::Remote;
            (media, state.remote_playback.generation)
        };
        if let Some(app) = self.app() {
            let name = engine
                .peer_names()
                .get(target)
                .cloned()
                .unwrap_or_else(|| target.to_string());
            app.set_remote_target_id(target.into());
            app.set_remote_target_name(name.into());
            app.set_stream_action_open(false);
            app.set_streams_searching(true);
            app.set_streams(Rc::new(VecModel::from(Vec::<StreamRow>::new())).into());
            app.set_stream_addons(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
            app.set_streams_hint(text::tr("Loading streams from the playing device…").into());
        }
        if self.restore_selector_lookup(target, &media) {
            return;
        }
        let b = self.clone();
        let target = target.to_string();
        let started = Instant::now();
        let request = match engine.prepare_playback_request(&target, Request::Lookup(media.clone()))
        {
            Ok(request) => request,
            Err(_) => {
                if let Some(app) = self.app() {
                    app.set_streams_searching(false);
                    app.set_streams_hint(error_text(&PlaybackError::Unavailable).into());
                }
                return;
            }
        };
        thread::spawn(move || {
            let response = engine.request_prepared_playback(request);
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if b.shared.lock().unwrap().remote_playback.generation != generation {
                    return;
                }
                if !nova_sync::engine().is_some_and(|current| Arc::ptr_eq(&current, &engine)) {
                    if let Some(current) = nova_sync::engine() {
                        b.reconcile_remote_polling_engine(&current);
                    } else if let Some(app) = b.app() {
                        app.set_streams_searching(false);
                        app.set_streams_hint(error_text(&PlaybackError::Unavailable).into());
                    }
                    return;
                }
                let Some(app) = b.app() else { return };
                app.set_streams_searching(false);
                match response {
                    Ok(Response::Offers(offers)) => {
                        {
                            let mut state = b.shared.lock().unwrap();
                            // Generation checks above exclude replies from a target
                            // abandoned while its lookup was still running.
                            if state.remote_playback.selector_lookups.len() < 32
                                || state.remote_playback.selector_lookups.contains_key(&target)
                            {
                                state.remote_playback.selector_lookups.insert(
                                    target.clone(),
                                    CachedRemoteLookup {
                                        media,
                                        offers: offers.clone(),
                                        started,
                                    },
                                );
                            }
                            state.remote_playback.remote_offers = offers;
                            state.remote_playback.remote_filter = 0;
                            state.remote_playback.remote_page = 0;
                        }
                        b.render_remote_streams();
                    }
                    Ok(Response::Error(error)) => app.set_streams_hint(error_text(&error).into()),
                    _ => app.set_streams_hint(error_text(&PlaybackError::Unavailable).into()),
                }
            });
        });
    }

    fn restore_selector_lookup(&self, target: &str, media: &MediaIdentity) -> bool {
        let Some(app) = self.app().filter(|app| app.get_stream_selector_open()) else {
            return false;
        };
        let mut state = self.shared.lock().unwrap();
        let Some(offers) = state.remote_playback.cached_selector_offers(target, media) else {
            return false;
        };
        state.remote_playback.remote_offers = offers;
        state.remote_playback.remote_filter = 0;
        state.remote_playback.remote_page = 0;
        drop(state);
        app.set_streams_searching(false);
        self.render_remote_streams();
        true
    }

    fn render_remote_streams(&self) {
        let Some(app) = self.app() else { return };
        let (rows, names, total, filter, page) = {
            let mut state = self.shared.lock().unwrap();
            let r = &mut state.remote_playback;
            let mut names = Vec::<String>::new();
            for offer in &r.remote_offers {
                if !names.contains(&offer.name) {
                    names.push(offer.name.clone());
                }
            }
            if r.remote_filter > names.len() {
                r.remote_filter = 0;
            }
            let filtered = r
                .remote_offers
                .iter()
                .filter(|o| r.remote_filter == 0 || names.get(r.remote_filter - 1) == Some(&o.name))
                .cloned()
                .collect::<Vec<_>>();
            let total = filtered.len();
            let pages = total.div_ceil(25).max(1);
            r.remote_page = r.remote_page.min(pages - 1);
            r.visible_offers = filtered
                .into_iter()
                .skip(r.remote_page * 25)
                .take(25)
                .collect();
            let rows = r
                .visible_offers
                .iter()
                .map(|o| StreamRow {
                    id: o.id.clone().into(),
                    metadata: o.name.clone().into(),
                    text: o.description.clone().into(),
                    title: o.description.clone().into(),
                    size: o.size.clone().into(),
                    lines: Bridge::estimate_stream_lines(&o.description),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            (rows, names, total, r.remote_filter, r.remote_page)
        };
        app.set_streams(Rc::new(VecModel::from(rows)).into());
        app.set_stream_addons(
            Rc::new(VecModel::from(
                names
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_stream_loading(Rc::new(VecModel::from(Vec::<bool>::new())).into());
        app.set_stream_filter(filter as i32);
        app.set_stream_page(page as i32);
        app.set_stream_page_start((page * 25) as i32);
        app.set_stream_page_count(total.div_ceil(25).max(1) as i32);
        app.set_stream_total(total as i32);
        app.set_detail_kb_s((page * 25) as i32);
        app.set_streams_hint(SharedString::default());
    }

    pub(super) fn remote_pick_filter(&self, index: usize) -> bool {
        if !self.remote_output_selected() {
            return false;
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.remote_filter = index;
            state.remote_playback.remote_page = 0;
        }
        self.render_remote_streams();
        true
    }

    pub(super) fn remote_pick_page(&self, page: usize) -> bool {
        if !self.remote_output_selected() {
            return false;
        }
        self.shared.lock().unwrap().remote_playback.remote_page = page;
        self.render_remote_streams();
        true
    }

    pub(super) fn remote_stream_selected(&self, index: usize) -> bool {
        let (target, offer, generation) = {
            let state = self.shared.lock().unwrap();
            let r = &state.remote_playback;
            if r.target.is_empty() {
                return false;
            }
            let offers = if self.app().is_some_and(|a| a.get_remote_controls_open()) {
                &r.remote_offers
            } else {
                &r.visible_offers
            };
            let row = if self.app().is_some_and(|a| a.get_remote_controls_open()) {
                Some(index)
            } else {
                index.checked_sub(r.remote_page * 25)
            };
            let Some(offer) = row.and_then(|i| offers.get(i)) else {
                return true;
            };
            (r.target.clone(), offer.id.clone(), r.generation)
        };
        let Some(engine) = nova_sync::engine() else {
            return true;
        };
        let b = self.clone();
        let request = match engine.prepare_playback_request(&target, Request::Snapshot) {
            Ok(request) => request,
            Err(_) => return true,
        };
        thread::spawn(move || {
            let response = engine.request_prepared_playback(request);
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if b.shared.lock().unwrap().remote_playback.generation != generation {
                    return;
                }
                match response {
                    Ok(Response::Snapshot(snapshot)) if snapshot.ready => {
                        if snapshot.session.is_some() {
                            let token = format!("{}:{generation}:{offer}", *BOOT);
                            b.shared.lock().unwrap().remote_playback.replacement =
                                Some(PendingReplacement {
                                    token: token.clone(),
                                    offer,
                                    session: snapshot.session,
                                });
                            if let Some(app) = b.app() {
                                app.set_remote_replacement_title(snapshot.title.into());
                                app.set_remote_replacement_token(token.into());
                            }
                        } else {
                            b.send_remote_start(offer, None);
                        }
                    }
                    _ => {
                        if let Some(app) = b.app() {
                            app.set_streams_hint(error_text(&PlaybackError::Unavailable).into());
                        }
                    }
                }
            });
        });
        true
    }

    fn confirm_remote_replacement(&self, token: &str, accept: bool) {
        let pending = {
            let mut state = self.shared.lock().unwrap();
            if !state
                .remote_playback
                .replacement
                .as_ref()
                .is_some_and(|p| p.token == token)
            {
                return;
            }
            state.remote_playback.replacement.take().unwrap()
        };
        if let Some(app) = self.app() {
            app.set_remote_replacement_token(SharedString::default());
        }
        if accept {
            self.send_remote_start(pending.offer, pending.session);
        }
    }

    fn send_remote_start(&self, offer: String, expected: Option<SessionId>) {
        let target = self.shared.lock().unwrap().remote_playback.target.clone();
        // Reserve the controller role before network I/O, including start prompts.
        if self.shared.lock().unwrap().remote_playback.role == Role::Local {
            return;
        }
        // Retain selection offers across opening the controller.
        self.open_remote_controls(&target);
        self.shared
            .lock()
            .unwrap()
            .remote_playback
            .queued
            .push_back(Request::Start {
                offer_id: offer,
                expected_session: expected,
            });
        self.remote_tick();
    }

    fn remote_ui_action(&self, name: &str, value: f32, token: &str) {
        if !FOREGROUND.load(Ordering::Acquire) {
            return;
        }
        let snapshot = self.shared.lock().unwrap().remote_playback.snapshot.clone();
        let Some(snapshot) = snapshot else { return };
        let Some(session) = snapshot.session else {
            return;
        };
        let expected = session_text(Some(session));
        let prompt_token = if snapshot.resume_prompt {
            expected.clone()
        } else {
            snapshot.next_episode_token.clone()
        };
        if !value.is_finite()
            || (token != expected && (!name.starts_with("prompt_") || token != prompt_token))
        {
            return;
        }
        let action = match name {
            "pause" => PlaybackAction::TogglePause,
            "seek" => PlaybackAction::SeekAbsolute(value as f64),
            "seek_relative" => PlaybackAction::SeekRelative(value as f64),
            "volume" => PlaybackAction::SetVolume(value as f64),
            "volume_relative" => PlaybackAction::VolumeStep(value as f64),
            "speed_preview" => PlaybackAction::BeginSpeedPreview,
            "speed_preview_end" => PlaybackAction::EndSpeedPreview,
            "mute" => PlaybackAction::ToggleMute,
            "speed" => PlaybackAction::SetSpeed(value as f64),
            "reset_audio" => PlaybackAction::ResetAudioTrack,
            "reset_subtitle" => PlaybackAction::ResetSubtitleTrack,
            "reset_speed" => PlaybackAction::ResetSpeed,
            "audio" => PlaybackAction::SelectAudioTrack(value as i32),
            "subtitle" => PlaybackAction::SelectSubtitleTrack(value as i32),
            "fullscreen" => PlaybackAction::ToggleFullscreen,
            "screenshot" => PlaybackAction::Screenshot,
            "stop" => PlaybackAction::Stop,
            "prompt_cancel" if snapshot.resume_prompt => PlaybackAction::CancelResume,
            "prompt_accept" if snapshot.resume_prompt => PlaybackAction::Resume(true),
            "prompt_reject" if snapshot.resume_prompt => PlaybackAction::Resume(false),
            "prompt_accept" if snapshot.next_episode_prompt => {
                if let Some(app) = self.app() {
                    app.set_remote_offers(
                        Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
                    );
                }
                self.shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .remote_offers
                    .clear();
                let lookup_pending = {
                    let state = self.shared.lock().unwrap();
                    state.remote_playback.lookup_in_flight
                        || state
                            .remote_playback
                            .queued
                            .iter()
                            .any(|r| matches!(r, Request::Lookup(_)))
                };
                if lookup_pending {
                    return;
                }
                if let Some(media) = snapshot.next_episode_media {
                    self.shared
                        .lock()
                        .unwrap()
                        .remote_playback
                        .queued
                        .push_back(Request::Lookup(media));
                }
                self.remote_tick();
                return;
            }
            "prompt_reject" if snapshot.next_episode_prompt => PlaybackAction::DismissNextEpisode {
                token: token.into(),
            },
            _ => return,
        };
        let mut state = self.shared.lock().unwrap();
        // A bounded command queue preserves order without replay on reconnect.
        let r = &mut state.remote_playback;
        r.interacting_until = Some(Instant::now() + Duration::from_secs(1));
        // Only consecutive slider updates commute. Searching further back
        // could move a new absolute seek before an intervening relative seek,
        // or a new speed before ResetSpeed, reversing the user's final intent.
        if matches!(
            action,
            PlaybackAction::SeekAbsolute(_)
                | PlaybackAction::SetVolume(_)
                | PlaybackAction::SetSpeed(_)
        ) && let Some(old) = r.queued.back_mut()
            && matches!(old, Request::Command { session: old_session, action: old_action } if *old_session == session && std::mem::discriminant(old_action) == std::mem::discriminant(&action))
        {
            *old = Request::Command { session, action };
            drop(state);
            self.remote_tick();
            return;
        }
        if action == PlaybackAction::EndSpeedPreview {
            // Releasing before Begin has left the queue cancels that Begin.
            // Sending End first and then Begin would leave a permanent hold.
            r.queued.retain(|request| !matches!(request, Request::Command { session: held_session, action: PlaybackAction::BeginSpeedPreview } if *held_session == session));
        }
        let critical = matches!(
            action,
            PlaybackAction::Stop | PlaybackAction::EndSpeedPreview | PlaybackAction::CancelResume
        );
        if critical {
            if action == PlaybackAction::Stop {
                r.queued.clear();
            }
            if r.queued.len() >= 32 {
                r.queued.pop_back();
            }
            r.queued.push_front(Request::Command { session, action });
        } else if r.queued.len() < 32 {
            r.queued.push_back(Request::Command { session, action });
        }
        drop(state);
        self.remote_tick();
    }

    /// Refresh presentation only: a locale change must not reconnect or mutate
    /// receiver state, capabilities, prompts, or queued actions.
    pub(super) fn refresh_remote_language_text(&self) {
        let Some(app) = self.app() else { return };
        let rows = app
            .get_remote_devices()
            .iter()
            .map(|mut row| {
                row.status = text::tr(if row.ready {
                    "Available"
                } else {
                    "Open Nova to connect"
                })
                .into();
                row
            })
            .collect::<Vec<_>>();
        app.set_remote_devices(Rc::new(VecModel::from(rows)).into());
        let snapshot = self.shared.lock().unwrap().remote_playback.snapshot.clone();
        if let Some(snapshot) = snapshot {
            app.set_remote_phase(remote_phase_text(&snapshot).into());
            app.set_remote_prompt(remote_prompt_text(&snapshot).into());
        }
        if app.get_remote_devices_open() || app.get_remote_controls_open() {
            app.recover_back_focus();
        }
    }

    fn apply_remote_snapshot(&self, snapshot: PlaybackSnapshot) {
        let Some(app) = self.app() else { return };
        app.set_remote_loading(false);
        app.set_remote_connected(snapshot.ready);
        app.set_remote_error(SharedString::default());
        app.set_remote_session(session_text(snapshot.session).into());
        app.set_remote_title(snapshot.title.clone().into());
        app.set_remote_episode(snapshot.episode.clone().into());
        app.set_remote_position(snapshot.time as f32);
        app.set_remote_duration(snapshot.duration as f32);
        app.set_remote_volume(snapshot.volume as f32);
        app.set_remote_speed(snapshot.speed as f32);
        app.set_remote_paused(snapshot.paused);
        app.set_remote_muted(snapshot.muted);
        app.set_remote_fullscreen(snapshot.fullscreen);
        app.set_remote_can_pause(snapshot.capabilities.pause);
        app.set_remote_can_mute(snapshot.capabilities.mute);
        app.set_remote_can_stop(snapshot.capabilities.stop);
        app.set_remote_can_seek(snapshot.capabilities.seek);
        app.set_remote_can_volume(snapshot.capabilities.volume);
        app.set_remote_can_speed(snapshot.capabilities.speed);
        app.set_remote_can_tracks(snapshot.capabilities.tracks);
        app.set_remote_can_fullscreen(snapshot.capabilities.fullscreen);
        app.set_remote_can_screenshot(snapshot.capabilities.screenshot);
        app.set_remote_phase(remote_phase_text(&snapshot).into());
        app.set_remote_time_text(
            format!(
                "{} / {}",
                crate::player::format_time(snapshot.time),
                crate::player::format_time(snapshot.duration)
            )
            .into(),
        );
        let tracks = |rows: &[PlaybackTrack], selected| {
            Rc::new(VecModel::from(
                rows.iter()
                    .map(|track| crate::TrackRow {
                        index: track.id,
                        title: track.label.clone().into(),
                        selected: track.id == selected,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ))
        };
        app.set_remote_audio_tracks(tracks(&snapshot.audio_tracks, snapshot.audio_track).into());
        app.set_remote_sub_tracks(
            tracks(&snapshot.subtitle_tracks, snapshot.subtitle_track).into(),
        );
        app.set_remote_resume_prompt(snapshot.resume_prompt);
        app.set_remote_prompt(remote_prompt_text(&snapshot).into());
        app.set_remote_prompt_token(if snapshot.resume_prompt {
            session_text(snapshot.session).into()
        } else {
            snapshot.next_episode_token.clone().into()
        });
        let mut state = self.shared.lock().unwrap();
        if state
            .remote_playback
            .snapshot
            .as_ref()
            .is_some_and(|old| old.session != snapshot.session)
        {
            state
                .remote_playback
                .queued
                .retain(|r| !matches!(r, Request::Command { .. } | Request::Lookup(_)));
            state.remote_playback.remote_offers.clear();
            app.set_remote_offers(
                Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
            );
        }
        state.remote_playback.snapshot = Some(snapshot);
        drop(state);
        if app.get_remote_controls_open() {
            app.recover_back_focus();
        }
    }

    pub(super) fn remote_tick(&self) {
        // Slint timers can still tick around Android lifecycle transitions.
        // Backgrounding must never launch discovery or controller polling.
        if !FOREGROUND.load(Ordering::Acquire) {
            self.shared.lock().unwrap().remote_playback.queued.clear();
            return;
        }
        let Some(app) = self.app() else { return };
        if let Some(engine) = nova_sync::engine() {
            self.reconcile_remote_polling_engine(&engine);
        }
        let mut devices_changed = false;
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(engine) = nova_sync::engine() {
                let peers = engine.peers();
                state
                    .remote_playback
                    .discovered
                    .retain(|peer, _| peers.contains(peer));
                if app
                    .get_remote_devices()
                    .iter()
                    .any(|row| !peers.iter().any(|peer| peer == row.id.as_str()))
                {
                    let rows = app
                        .get_remote_devices()
                        .iter()
                        .filter(|row| peers.iter().any(|peer| peer == row.id.as_str()))
                        .collect::<Vec<_>>();
                    app.set_remote_has_playing_devices(rows.iter().any(|row| row.has_session));
                    app.set_remote_has_ready_devices(rows.iter().any(|row| row.ready));
                    app.set_remote_devices(Rc::new(VecModel::from(rows)).into());
                    devices_changed = true;
                }
            }
            if !app.get_stream_selector_open() {
                state.remote_playback.selector_lookups.clear();
                state.remote_playback.selector_engine = None;
            } else if let Some(engine) = nova_sync::engine() {
                state.remote_playback.reconcile_selector_cache(&engine);
            } else {
                state.remote_playback.selector_lookups.clear();
                state.remote_playback.selector_engine = None;
            }
        }
        // Peer removal may have destroyed the device row holding native focus.
        // Instantiate only after releasing SharedState: page callbacks can
        // re-enter the bridge while the replacement trees are published.
        if devices_changed && app.get_remote_devices_open() {
            app.recover_back_focus();
        }
        if !app.get_stream_selector_open()
            && !app.get_remote_controls_open()
            && self.remote_output_selected()
        {
            self.remote_disconnect();
        }
        let expired = {
            let mut state = self.shared.lock().unwrap();
            let r = &mut state.remote_playback;
            let expired = r
                .preview_seen
                .is_some_and(|seen| seen.elapsed() > Duration::from_secs(3));
            if expired {
                r.preview_owner = None;
                r.preview_seen = None;
            }
            r.offers
                .retain(|_, o| o.created.elapsed() < Duration::from_secs(300));
            expired
        };
        if expired {
            let _ = self.execute_playback_action(PlaybackAction::EndSpeedPreview);
        }
        if app.get_remote_controls_open() {
            let Some(engine) = nova_sync::engine() else {
                app.set_remote_connected(false);
                return;
            };
            let (target, generation, request, is_lookup, lookup_session) = {
                let mut state = self.shared.lock().unwrap();
                let r = &mut state.remote_playback;
                if r.in_flight || r.target.is_empty() {
                    return;
                }
                if !engine.peers().contains(&r.target) {
                    drop(state);
                    self.remote_disconnect();
                    return;
                }
                let now = Instant::now();
                if !r.control_request_due(now) {
                    return;
                }
                if r.lookup_in_flight && matches!(r.queued.front(), Some(Request::Lookup(_))) {
                    return;
                }
                let request = r.queued.pop_front().unwrap_or(Request::Snapshot);
                r.control_poll.attempted = Some(now);
                let is_lookup = matches!(request, Request::Lookup(_));
                if matches!(request, Request::Start { .. }) {
                    r.selector_lookups.remove(&r.target);
                }
                if is_lookup {
                    if r.lookup_in_flight {
                        return;
                    }
                    r.lookup_in_flight = true;
                } else {
                    r.in_flight = true;
                }
                (
                    r.target.clone(),
                    r.generation,
                    request,
                    is_lookup,
                    r.snapshot.as_ref().and_then(|s| s.session),
                )
            };
            let request = match engine.prepare_playback_request(&target, request) {
                Ok(request) => request,
                Err(_) => {
                    let mut state = self.shared.lock().unwrap();
                    if is_lookup {
                        state.remote_playback.lookup_in_flight = false;
                    } else {
                        state.remote_playback.in_flight = false;
                    }
                    state.remote_playback.control_poll.complete(None);
                    state.remote_playback.queued.clear();
                    drop(state);
                    app.set_remote_error(error_text(&PlaybackError::Unavailable).into());
                    return;
                }
            };
            let b = self.clone();
            thread::spawn(move || {
                // Lifecycle can change between preparing and starting the worker.
                let response = if FOREGROUND.load(Ordering::Acquire) {
                    engine.request_prepared_playback(request)
                } else {
                    Err(std::io::Error::other("playback controller backgrounded").into())
                };
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    {
                        let mut state = b.shared.lock().unwrap();
                        if state.remote_playback.generation != generation {
                            return;
                        }
                        if !nova_sync::engine()
                            .is_some_and(|current| Arc::ptr_eq(&current, &engine))
                        {
                            state.remote_playback.in_flight = false;
                            state.remote_playback.lookup_in_flight = false;
                            state.remote_playback.queued.clear();
                            return;
                        }
                        if is_lookup {
                            state.remote_playback.lookup_in_flight = false;
                        } else {
                            state.remote_playback.in_flight = false;
                        }
                        if !FOREGROUND.load(Ordering::Acquire) {
                            state.remote_playback.queued.clear();
                            return;
                        }
                        let r = &mut state.remote_playback;
                        match &response {
                            Ok(Response::Snapshot(snapshot)) => {
                                r.control_poll.complete(Some(snapshot.clone()))
                            }
                            Ok(Response::Accepted | Response::Offers(_)) => {
                                r.control_poll.failures = 0;
                                // Commands which acknowledge without a snapshot
                                // need an immediate read to publish their result.
                                r.control_poll.attempted = None;
                            }
                            _ => r.control_poll.complete(None),
                        }
                        if is_lookup
                            && state
                                .remote_playback
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.session)
                                != lookup_session
                        {
                            return;
                        }
                    }
                    match response {
                        Ok(Response::Snapshot(snapshot)) => b.apply_remote_snapshot(snapshot),
                        Ok(Response::Offers(offers)) => {
                            if let Some(app) = b.app() {
                                app.set_remote_offers(
                                    Rc::new(VecModel::from(
                                        offers
                                            .iter()
                                            .map(|o| crate::RemoteOfferRow {
                                                id: o.id.clone().into(),
                                                title: o.name.clone().into(),
                                                description: o.description.clone().into(),
                                            })
                                            .collect::<Vec<_>>(),
                                    ))
                                    .into(),
                                );
                            }
                            b.shared.lock().unwrap().remote_playback.remote_offers = offers;
                        }
                        Ok(Response::Accepted) => {}
                        Ok(Response::Error(error)) => {
                            if let Some(app) = b.app() {
                                app.set_remote_error(error_text(&error).into());
                            }
                            if !is_lookup {
                                b.shared.lock().unwrap().remote_playback.queued.clear();
                            }
                        }
                        _ => {
                            if let Some(app) = b.app() {
                                app.set_remote_connected(false);
                                app.set_remote_loading(false);
                                app.set_remote_error(
                                    error_text(&PlaybackError::Unavailable).into(),
                                );
                            }
                            b.shared.lock().unwrap().remote_playback.queued.clear();
                        }
                    }
                    // Preserve queue order while letting commands run as soon
                    // as the previous operation completes, without timer delay.
                    b.remote_tick();
                });
            });
        } else {
            let home_visible = app.get_show_home()
                && !app.get_party_page_open()
                && !app.get_player_open()
                && !app.get_modal_visible()
                && !app.get_show_library()
                && !app.get_show_settings()
                && !app.get_show_about()
                && !app.get_more_open()
                && !app.get_show_calendar()
                && !app.get_show_downloads();
            let due = {
                let mut state = self.shared.lock().unwrap();
                let home_enabled = state.cache_settings.home_devices_enabled;
                let r = &mut state.remote_playback;
                let view = if app.get_remote_devices_open() || app.get_stream_selector_open() {
                    DiscoveryView::Picker
                } else if home_visible && home_enabled {
                    DiscoveryView::Home
                } else {
                    DiscoveryView::Hidden
                };
                let opened_picker = view == DiscoveryView::Picker && r.discovery_view != view;
                r.discovery_view = view;
                opened_picker
                    || view
                        .interval(app.get_remote_has_playing_devices())
                        .is_some_and(|interval| poll_due(r.discovery_at, Instant::now(), interval))
            };
            if due {
                self.poll_playback_devices(false);
            }
        }
    }

    fn refresh_playback_devices(&self) {
        self.poll_playback_devices(true);
    }

    fn poll_playback_devices(&self, force: bool) {
        if !FOREGROUND.load(Ordering::Acquire) {
            return;
        }
        let Some(engine) = nova_sync::engine() else {
            if let Some(app) = self.app() {
                app.set_remote_has_playing_devices(false);
                app.set_remote_has_ready_devices(false);
                app.set_remote_devices(Default::default());
                if app.get_remote_devices_open() {
                    app.recover_back_focus();
                }
            }
            return;
        };
        self.reconcile_remote_polling_engine(&engine);
        let now = Instant::now();
        let allowed = engine.peers();
        let (peers, token) = {
            let mut state = self.shared.lock().unwrap();
            let r = &mut state.remote_playback;
            r.discovered.retain(|peer, _| allowed.contains(peer));
            if r.discovery_running {
                r.discovery_forced |= force;
                return;
            }
            let pending_force = std::mem::take(&mut r.discovery_forced);
            let force = force || pending_force;
            let peers = r.discovery_peers(&allowed, now, force);
            r.discovery_at = Some(now);
            r.discovery_running = true;
            r.discovery_token = r.discovery_token.wrapping_add(1);
            (peers, r.discovery_token)
        };
        let b = self.clone();
        thread::spawn(move || {
            // Bounded discovery fan-out avoids an offline device delaying live peers.
            let results = peers
                .into_iter()
                .map(|peer| {
                    let engine = engine.clone();
                    thread::spawn(move || {
                        let snapshot = if FOREGROUND.load(Ordering::Acquire) {
                            match engine.request_playback(&peer, Request::Snapshot) {
                                Ok(Response::Snapshot(s)) => Some(s),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        (peer, snapshot)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|h| h.join().ok())
                .collect::<Vec<_>>();
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                {
                    let mut state = b.shared.lock().unwrap();
                    let r = &mut state.remote_playback;
                    if r.discovery_token != token {
                        return;
                    }
                    r.discovery_running = false;
                }
                let Some(current) = nova_sync::engine() else {
                    return;
                };
                if !Arc::ptr_eq(&current, &engine) || !FOREGROUND.load(Ordering::Acquire) {
                    return;
                }
                let Some(app) = b.app() else { return };
                let allowed = current.peers();
                let names = current.peer_names();
                let (rows, refresh_pending) = {
                    let mut state = b.shared.lock().unwrap();
                    let r = &mut state.remote_playback;
                    r.discovered.retain(|peer, _| allowed.contains(peer));
                    for (peer, snapshot) in results {
                        if allowed.contains(&peer) {
                            r.discovered.entry(peer).or_default().complete(snapshot);
                        }
                    }
                    let rows = allowed
                        .into_iter()
                        .map(|peer| {
                            let snapshot = r
                                .discovered
                                .get(&peer)
                                .and_then(|poll| poll.snapshot.as_ref());
                            let ready = snapshot.is_some_and(|s| s.ready);
                            crate::RemoteDeviceRow {
                                name: names
                                    .get(&peer)
                                    .cloned()
                                    .unwrap_or_else(|| peer.clone())
                                    .into(),
                                id: peer.into(),
                                ready,
                                has_session: snapshot
                                    .is_some_and(|s| s.ready && s.session.is_some()),
                                playing_title: snapshot
                                    .filter(|s| s.session.is_some())
                                    .map(|s| s.title.clone())
                                    .unwrap_or_default()
                                    .into(),
                                status: text::tr(if ready {
                                    "Available"
                                } else {
                                    "Open Nova to connect"
                                })
                                .into(),
                            }
                        })
                        .collect::<Vec<_>>();
                    (rows, r.discovery_forced)
                };
                app.set_remote_has_playing_devices(rows.iter().any(|row| row.has_session));
                app.set_remote_has_ready_devices(rows.iter().any(|row| row.ready));
                app.set_remote_devices(Rc::new(VecModel::from(rows)).into());
                if app.get_remote_devices_open() {
                    app.recover_back_focus();
                }
                if refresh_pending {
                    b.poll_playback_devices(false);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offer_display_omits_source_locations_and_credentials() {
        assert_eq!(
            remote_display_text(
                "1080p https://host/video?token=secret\nRelease /private/video.mkv\nBearer credential",
                128
            ),
            "Release"
        );
        assert_eq!(
            remote_display_text(
                "Release https://host/video.mkv C:\\private\\video.mkv magnet:?xt=secret",
                128
            ),
            "Release"
        );
        let text = remote_display_text(&"ž".repeat(200), 127);
        assert!(text.len() <= 127);
        assert!(text.is_char_boundary(text.len()));
    }
    #[test]
    fn concurrent_lookup_completion_preserves_addon_order_and_completes_once() {
        let row = |addon: &str| {
            (
                StreamUi {
                    id: String::new(),
                    choice_id: None,
                    owner_url: String::new(),
                    display: addon.into(),
                    card: StreamCardText::default(),
                    source: StreamSource::Unsupported,
                    addon: addon.into(),
                    download: None,
                },
                1,
            )
        };
        let mut batch = RemoteLookupBatch {
            remaining: 3,
            rows: vec![None; 3],
            done: Some(Box::new(|_| {})),
        };
        assert!(batch.push(2, vec![row("third")]).is_none());
        assert!(batch.push(2, vec![row("duplicate")]).is_none());
        assert!(batch.push(0, vec![row("first")]).is_none());
        let (rows, _) = batch.push(1, vec![row("second")]).unwrap();
        assert_eq!(
            rows.iter()
                .map(|(row, _)| row.addon.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        assert!(batch.push(1, vec![]).is_none());
    }

    #[test]
    fn roles_reserve_loading_and_release_after_close() {
        let mut state = State::default();
        assert!(state.can_play_locally());
        state.role = Role::Remote;
        assert!(!state.can_play_locally());
        state.role = Role::Idle;
        state.begin();
        assert_eq!(state.role, Role::Local);
        let first = state.session.unwrap();
        state.ended();
        state.begin();
        assert_ne!(state.session, Some(first));
    }
}

#[cfg(all(test, feature = "desktop"))]
#[path = "remote_playback_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[path = "remote_polling_tests.rs"]
mod polling_tests;

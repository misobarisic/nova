//! Live playback is a receiver-owned service, separate from persistent sync.
//! The UI thread serializes both roles and commands; workers only carry messages.
use super::*;
use nova_playback_api::{
    Completion, MAX_REMOTE_BOOKMARKS, MAX_REMOTE_CHAPTERS, MAX_REMOTE_LABEL_BYTES,
    MAX_REMOTE_NOTE_BYTES, MAX_REMOTE_TRACKS, MediaIdentity, PeerContext, PlaybackAction,
    PlaybackBookmark, PlaybackCapabilities, PlaybackChapter, PlaybackError, PlaybackOffer,
    PlaybackPhase, PlaybackRole, PlaybackSnapshot, PlaybackTrack, Request, Response, SessionId,
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
pub(super) fn remote_episode<'a>(videos: &'a [Video], requested: &str) -> Option<&'a Video> {
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

fn remote_entity_id(prefix: char, value: &str) -> String {
    // Tokens disclose neither addon URLs nor local scene keys. Membership is
    // rechecked against the receiver's current snapshot before every command.
    let hash = value.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{prefix}{hash:016x}")
}

fn bounded_tracks(mut rows: Vec<PlaybackTrack>, selected: i32) -> Vec<PlaybackTrack> {
    if rows.len() > MAX_REMOTE_TRACKS {
        let selected_row = rows.iter().find(|row| row.id == selected).cloned();
        rows.truncate(MAX_REMOTE_TRACKS);
        if let Some(row) = selected_row
            && !rows.iter().any(|r| r.id == selected)
        {
            rows[MAX_REMOTE_TRACKS - 1] = row;
        }
    }
    rows
}

/// Validate against the one current receiver snapshot. Numeric native track
/// IDs are intentionally local-only: a delayed controller choice must carry
/// the opaque membership token advertised for its original source/choice.
fn validate_remote_action(
    snapshot: &PlaybackSnapshot,
    action: PlaybackAction,
) -> Result<PlaybackAction, PlaybackError> {
    use PlaybackAction::*;
    if !action.valid_payload() {
        return Err(PlaybackError::InvalidRequest);
    }
    let caps = &snapshot.capabilities;
    let supported = match &action {
        SetSleepTimer(nova_playback_api::SleepTimerMode::Episodes(_)) => {
            caps.sleep_timer && caps.sleep_timer_episodes
        }
        SetSleepTimer(_) | ExtendSleepTimer | CancelSleepTimer => caps.sleep_timer,
        QueueSkip | QueueStop | QueuePlayNow => caps.queue,
        Stop => caps.stop,
        Resume(_) | CancelResume => caps.resume,
        NextEpisode { token } | DismissNextEpisode { token } => {
            caps.next_episode && *token == snapshot.next_episode_token
        }
        TogglePause | SetPaused(_) => caps.pause,
        SeekRelative(_) | SeekAbsolute(_) => caps.seek,
        SetVolume(_) | VolumeStep(_) | SystemVolumeStep(_) => caps.volume,
        SetMuted(_) | ToggleMute => caps.mute,
        SetSpeed(_) | SpeedStep(_) | BeginSpeedPreview | ResetSpeed => caps.speed,
        EndSpeedPreview => true,
        SelectAudioTrackToken(_)
        | SelectSubtitleTrackToken(_)
        | ResetAudioTrack
        | ResetSubtitleTrack => caps.tracks,
        SelectAudioTrack(_) | SelectSubtitleTrack(_) | SelectSecondarySubtitleTrack(_) => false,
        ToggleFullscreen | SetFullscreen(_) => caps.fullscreen,
        Screenshot => caps.screenshot,
        SetDecoder(_) => caps.decoder,
        SetBrightness(_) | BrightnessStep(_) => caps.brightness,
        SeekChapter { .. } => caps.chapters,
        SetSubtitleStyle { .. } | ResetSubtitleStyle => caps.subtitle_style,
        SelectSecondarySubtitleTrackToken(_) | SetSecondarySubtitlePosition(_) => {
            caps.secondary_subtitles
        }
        SetIntroSkipMode(_) => caps.intro_skip,
        SkipSegment { token } | DismissSkipSegment { token } => {
            caps.intro_skip && !snapshot.skip_token.is_empty() && *token == snapshot.skip_token
        }
        AddBookmark { .. } | EditBookmark { .. } | DeleteBookmark { .. } | SeekBookmark { .. } => {
            caps.bookmarks
        }
        TryAnotherStream => caps.source_recovery,
        RetrySubtitleDiscovery | RefreshSubtitleTracks | CancelSubtitleDownload { .. } => {
            caps.subtitle_discovery
        }
    };
    if !supported {
        return Err(PlaybackError::Unsupported);
    }
    let track = |rows: &[PlaybackTrack], token: &str| {
        rows.iter()
            .find(|row| row.token == token)
            .map(|row| row.id)
            .ok_or(PlaybackError::InvalidRequest)
    };
    match action {
        SelectAudioTrackToken(token) => {
            Ok(SelectAudioTrack(track(&snapshot.audio_tracks, &token)?))
        }
        SelectSubtitleTrackToken(token) if token == "off" => Ok(SelectSubtitleTrack(-1)),
        SelectSubtitleTrackToken(token) => Ok(SelectSubtitleTrack(track(
            &snapshot.subtitle_tracks,
            &token,
        )?)),
        SelectSecondarySubtitleTrackToken(token) if token == "off" => {
            Ok(SelectSecondarySubtitleTrack(-1))
        }
        SelectSecondarySubtitleTrackToken(token) => Ok(SelectSecondarySubtitleTrack(track(
            &snapshot.secondary_subtitle_tracks,
            &token,
        )?)),
        SeekChapter { ref id } if !snapshot.chapters.iter().any(|row| row.id == *id) => {
            Err(PlaybackError::NotFound)
        }
        EditBookmark { ref id, .. } | DeleteBookmark { ref id } | SeekBookmark { ref id }
            if !snapshot.bookmarks.iter().any(|row| row.id == *id) =>
        {
            Err(PlaybackError::NotFound)
        }
        action => Ok(action),
    }
}

fn is_lookup_request(request: &Request) -> bool {
    matches!(
        request,
        Request::Lookup(_)
            | Request::CurrentOffers { .. }
            | Request::QueueStart { .. }
            | Request::Command {
                action: PlaybackAction::QueueSkip,
                ..
            }
    )
}

fn semantic_request(request: &Request) -> Option<(&'static str, String, SessionId)> {
    let Request::Command { session, action } = request else {
        return None;
    };
    use PlaybackAction::*;
    let (name, id) = match action {
        SeekChapter { id } => ("chapter", id.clone()),
        SetSubtitleStyle { control, .. } => ("subtitle_style", control.to_string()),
        ResetSubtitleStyle => ("subtitle_style_reset", String::new()),
        SelectSecondarySubtitleTrackToken(_) => ("secondary_subtitle", String::new()),
        SetSecondarySubtitlePosition(_) => ("secondary_position", String::new()),
        SetIntroSkipMode(_) => ("intro_mode", String::new()),
        SkipSegment { token } => ("skip", token.clone()),
        DismissSkipSegment { token } => ("skip_dismiss", token.clone()),
        AddBookmark { .. } => ("bookmark_add", String::new()),
        EditBookmark { id, .. } => ("bookmark_edit", id.clone()),
        DeleteBookmark { id } => ("bookmark_delete", id.clone()),
        SeekBookmark { id } => ("bookmark_seek", id.clone()),
        TryAnotherStream => ("try_another", String::new()),
        RetrySubtitleDiscovery => ("subtitle_retry", String::new()),
        RefreshSubtitleTracks => ("subtitle_refresh", String::new()),
        CancelSubtitleDownload { .. } => ("subtitle_cancel", String::new()),
        SetDecoder(_) => ("decoder", String::new()),
        SetBrightness(_) => ("brightness", String::new()),
        _ => return None,
    };
    Some((name, id, *session))
}

static SESSION_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    retry_session: Option<(SessionId, u64)>,
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
    scene: Option<SceneStart>,
}

/// The opaque offer is content-bound by the receiver lookup, and only that
/// exact Start response may bind its newly allocated session. Display titles
/// in later snapshots are deliberately never used as content identifiers.
struct SceneStart {
    media: MediaIdentity,
    position: f64,
    offer: String,
    profile: profiles::Context,
}
struct RemoteSceneResume {
    start: SceneStart,
    session: Option<SessionId>,
    created: Instant,
    dismissed_resume: bool,
}
impl RemoteSceneResume {
    fn new(start: SceneStart) -> Self {
        Self {
            start,
            session: None,
            created: Instant::now(),
            dismissed_resume: false,
        }
    }
    fn accept_start(
        &mut self,
        offer: &str,
        expected: Option<SessionId>,
        snapshot: &PlaybackSnapshot,
    ) -> bool {
        if self.start.offer != offer
            || self.start.profile != profiles::active_context()
            || !profiles::is_current(&self.start.profile)
            || !snapshot.ready
            || snapshot.role != PlaybackRole::Local
            || snapshot.session.is_none()
            || snapshot.session == expected
            || matches!(
                snapshot.phase,
                PlaybackPhase::Idle | PlaybackPhase::Ended | PlaybackPhase::Failed
            )
        {
            return false;
        }
        self.session = snapshot.session;
        true
    }
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
    pub(super) start_retry: Option<playback::RemoteRetry>,
    offers: HashMap<String, Offer>,
    offer_sequence: u64,
    target: String,
    generation: u64,
    snapshot: Option<PlaybackSnapshot>,
    remote_offers: Vec<PlaybackOffer>,
    current_offers_session: Option<SessionId>,
    selector_lookups: HashMap<String, CachedRemoteLookup>,
    selector_engine: Option<std::sync::Weak<nova_sync::SyncEngine>>,
    visible_offers: Vec<PlaybackOffer>,
    visible_media: Option<MediaIdentity>,
    scene_resume: Option<RemoteSceneResume>,
    remote_filter: usize,
    remote_page: usize,
    queued: VecDeque<Request>,
    in_flight: bool,
    lookup_in_flight: bool,
    queue_revision: u64,
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
    fn queue_stop_request(&mut self) -> Option<Request> {
        let snapshot = self.snapshot.as_ref()?;
        let queue = snapshot.queue.as_ref()?;
        let request = Request::QueueStop {
            profile_id: queue.profile_id.clone(),
            run_id: queue.run_id,
            expected_session: snapshot.session,
        };
        self.queue_revision = self.queue_revision.wrapping_add(1);
        self.lookup_in_flight = false;
        self.queued.clear();
        self.queued.push_front(request.clone());
        Some(request)
    }
    fn scene_resume_command(&mut self, snapshot: &PlaybackSnapshot) -> Option<Request> {
        let mut pending = self.scene_resume.take()?;
        if pending.start.profile != profiles::active_context()
            || !profiles::is_current(&pending.start.profile)
            || pending.created.elapsed() > Duration::from_secs(120)
            || !snapshot.ready
            || snapshot.role != PlaybackRole::Local
        {
            return None;
        }
        let Some(session) = pending.session else {
            self.scene_resume = Some(pending);
            return None;
        };
        if snapshot.session != Some(session)
            || matches!(
                snapshot.phase,
                PlaybackPhase::Idle | PlaybackPhase::Ended | PlaybackPhase::Failed
            )
        {
            return None;
        }
        if snapshot.resume_prompt {
            let dismiss = !pending.dismissed_resume && snapshot.capabilities.resume;
            pending.dismissed_resume |= dismiss;
            self.scene_resume = Some(pending);
            return dismiss.then_some(Request::Command {
                session,
                action: PlaybackAction::Resume(false),
            });
        }
        if snapshot.capabilities.seek {
            return Some(Request::Command {
                session,
                action: PlaybackAction::SeekAbsolute(pending.start.position),
            });
        }
        self.scene_resume = Some(pending);
        None
    }

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
        self.current_offers_session = None;
        self.visible_offers.clear();
        self.visible_media = None;
        self.scene_resume = None;
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
        self.sequence = SESSION_SEQUENCE
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
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
    if !active {
        profile_policy::suspend();
    }
}

fn session_text(session: Option<SessionId>) -> String {
    session
        .map(|s| format!("{}:{}", s.epoch, s.sequence))
        .unwrap_or_default()
}

fn error_text(error: &PlaybackError) -> String {
    // Do not display provider errors which could include source URLs or secrets.
    text::tr(match error {
        PlaybackError::Failed(code) if code == "profile_locked" => "Unlock on TV.",
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
    fn remote_stop_queue(&self) {
        let mut state = self.shared.lock().unwrap();
        let remote = &mut state.remote_playback;
        if remote.queue_stop_request().is_none() {
            return;
        }
        drop(state);
        self.remote_tick();
    }

    pub(super) fn remote_start_queue(&self, item_key: Option<&str>) {
        let mut state = self.shared.lock().unwrap();
        let remote = &mut state.remote_playback;
        if remote.target.is_empty() || remote.queued.len() >= 32 {
            return;
        }
        let expected_session = remote.snapshot.as_ref().and_then(|s| s.session);
        remote.queued.push_back(Request::QueueStart {
            profile_id: profiles::current_id(),
            item_key: item_key.map(str::to_owned),
            expected_session,
        });
        drop(state);
        if let Some(app) = self.app() {
            app.global::<crate::QueueBridge>().set_open(false);
            app.set_remote_controls_open(true);
        }
        self.remote_tick();
    }

    pub(super) fn remote_queue_action(&self, action: PlaybackAction) {
        if matches!(action, PlaybackAction::QueueStop) {
            self.remote_stop_queue();
            return;
        }
        let session = self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .snapshot
            .as_ref()
            .and_then(|s| s.session);
        if let Some(session) = session {
            self.queue_remote_action(session, action);
        }
    }

    fn receive_queue_start(
        &self,
        peer: PeerContext,
        key: Option<&str>,
        expected: Option<SessionId>,
        continuation: bool,
        done: Completion,
    ) {
        if (if continuation {
            key.map_or_else(
                || Err("Missing queue item".to_owned()),
                |key| self.continue_queue_from(key),
            )
        } else {
            self.start_queue(key)
        })
        .is_err()
        {
            done(Response::Error(PlaybackError::NotFound));
            return;
        }
        let Some((run_id, entry)) = self.queue_run_entry() else {
            done(Response::Error(PlaybackError::NotFound));
            return;
        };
        let video = read_episodes_cache_for(&entry.type_, &entry.series_id)
            .unwrap_or_default()
            .into_iter()
            .find(|v| v.id == entry.episode_id)
            .unwrap_or_else(|| entry.video());
        if entry.type_ != "movie"
            && (if episode_has_air_date(&video) {
                !episode_is_out(&video, today_days())
            } else {
                entry.source_preference.is_none()
            })
        {
            self.pause_queue();
            done(Response::Error(PlaybackError::NotFound));
            return;
        }
        let profile = profiles::capture();
        let bridge = self.clone();
        let owner = peer.clone();
        self.lookup_remote_streams(
            peer,
            entry.identity(),
            Box::new(move |response| {
                if !profiles::is_current(&profile)
                    || !owner.is_authorized()
                    || bridge
                        .queue_run_entry()
                        .is_none_or(|(id, current)| id != run_id || current.key() != entry.key())
                    || bridge.playback_snapshot().session != expected
                {
                    done(Response::Error(PlaybackError::StaleSession));
                    return;
                }
                let Response::Offers(offers) = response else {
                    bridge.pause_queue();
                    done(response);
                    return;
                };
                let rows: Vec<_> = {
                    let state = bridge.shared.lock().unwrap();
                    offers
                        .iter()
                        .filter_map(|offer| {
                            state
                                .remote_playback
                                .offers
                                .get(&offer.id)
                                .map(|stored| (offer.id.clone(), stored.stream.clone()))
                        })
                        .collect()
                };
                let preference = entry.source_preference.clone().or_else(|| {
                    stream_series_scope(&entry.type_, &entry.series_id)
                        .and_then(|id| bridge.last_stream.series_preference(&id, now_secs()))
                });
                let candidate = playback_end::automatic_choice_with_rules(
                    &rows.iter().map(|(_, row)| row.clone()).collect::<Vec<_>>(),
                    preference.as_ref(),
                    active_torrent_settings().enabled,
                    &bridge.effective_stream_filter_rules(),
                );
                let id = candidate.and_then(|candidate| {
                    rows.iter()
                        .find(|(_, row)| row.id == candidate.id)
                        .map(|(id, _)| id.clone())
                });
                let Some(id) = id else {
                    bridge.pause_queue();
                    done(Response::Error(PlaybackError::NotFound));
                    return;
                };
                if !profile_policy::allows_title(
                    &profiles::ProfileStore::capture(),
                    &entry.type_,
                    &entry.series_id,
                ) || bridge.sleep_timer_blocks_successor(&entry.identity())
                {
                    done(Response::Error(PlaybackError::Unavailable));
                    return;
                }
                bridge.sleep_timer_note_successor(entry.identity(), run_id);
                bridge.preserve_queue_on_close(true);
                let result = bridge.start_remote_offer(&owner, &id, expected);
                bridge.preserve_queue_on_close(false);
                done(result);
            }),
        );
    }
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
        app.on_remote_semantic_action(move |name, value, id, text, session| {
            b.remote_semantic_action(
                name.as_str(),
                value,
                id.as_str(),
                text.as_str(),
                session.as_str(),
            );
        });
        let b = self.clone();
        app.on_remote_controls_visual_closed(move || b.remote_controls_visual_closed());
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
        self.suspend_profile_management();
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
        let skip_token = self.intro_skip_offer_token().unwrap_or_default();
        let source = self.player.screenshot_session();
        let source_offers = self.current_playback_identity().is_some();
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
        let tracks = |model: slint::ModelRc<crate::TrackRow>, audio, selected| {
            let rows = model
                .iter()
                .enumerate()
                .filter(|(index, row)| *index < MAX_REMOTE_TRACKS || row.index == selected)
                .map(|(_, row)| {
                    let online = self
                        .player
                        .online_subtitle(row.index)
                        .map(|s| s.url)
                        .unwrap_or_default();
                    PlaybackTrack {
                        id: row.index,
                        token: remote_entity_id(
                            't',
                            &format!(
                                "{source}:{audio}:{}:{}:{}:{online}",
                                row.index, row.title, row.lang
                            ),
                        ),
                        label: remote_display_text(
                            &format!("{} {}", row.title, row.lang),
                            MAX_REMOTE_LABEL_BYTES,
                        )
                        .replace('\n', " "),
                    }
                })
                .collect();
            bounded_tracks(rows, selected)
        };
        let audio_track = self.player.selected_track_id(true);
        let subtitle_track = self.player.selected_track_id(false);
        let secondary_track = app
            .get_secondary_sub_tracks()
            .iter()
            .find(|row| row.selected)
            .map_or(-1, |row| row.index);
        let duration = (app.get_duration() as f64).max(0.0);
        let valid_time = |position: f64| {
            position.is_finite() && position >= 0.0 && (duration <= 0.0 || position <= duration)
        };
        let chapters = app
            .get_player_chapters()
            .iter()
            .filter(|row| valid_time(f64::from(row.position)))
            .take(MAX_REMOTE_CHAPTERS + 1)
            .map(|row| PlaybackChapter {
                id: format!("c{:016x}", f64::from(row.position).to_bits()),
                label: remote_display_text(row.title.as_str(), MAX_REMOTE_LABEL_BYTES)
                    .replace('\n', " "),
                time: f64::from(row.position),
            })
            .collect::<Vec<_>>();
        let bookmarks = app
            .get_scene_bookmarks()
            .iter()
            .filter(|row| valid_time(f64::from(row.position)))
            .take(MAX_REMOTE_BOOKMARKS + 1)
            .map(|row| PlaybackBookmark {
                id: remote_entity_id('b', row.id.as_str()),
                note: remote_display_text(row.note.as_str(), MAX_REMOTE_NOTE_BYTES)
                    .replace('\n', " "),
                time: f64::from(row.position),
            })
            .collect::<Vec<_>>();
        #[cfg(target_os = "android")]
        let volume = android_player::player_volume() as f64;
        #[cfg(not(target_os = "android"))]
        let volume = app.get_volume() as f64;
        PlaybackSnapshot {
            sleep_timer: state
                .sleep_timer
                .snapshot(crate::player::elapsed_realtime_ms()),
            ready,
            role: if remote.role == Role::Remote {
                PlaybackRole::Remote
            } else {
                PlaybackRole::Local
            },
            session: remote.session,
            title: remote_display_text(app.get_player_show_title().as_str(), 512)
                .replace('\n', " "),
            episode: remote_display_text(app.get_player_episode_title().as_str(), 512)
                .replace('\n', " "),
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
            audio_tracks: tracks(app.get_audio_tracks(), true, audio_track),
            subtitle_tracks: tracks(app.get_sub_tracks(), false, subtitle_track),
            audio_track,
            subtitle_track,
            secondary_subtitle_tracks: tracks(
                app.get_secondary_sub_tracks(),
                false,
                secondary_track,
            ),
            secondary_subtitle_track: secondary_track,
            secondary_subtitle_position: app.get_secondary_subtitle_position_index(),
            subtitle_style: std::array::from_fn(|i| {
                app.get_effective_subtitle_style()
                    .row_data(i)
                    .unwrap_or_default() as f64
            }),
            chapters_truncated: chapters.len() > MAX_REMOTE_CHAPTERS,
            bookmarks_truncated: bookmarks.len() > MAX_REMOTE_BOOKMARKS,
            chapters: chapters.into_iter().take(MAX_REMOTE_CHAPTERS).collect(),
            bookmarks: bookmarks.into_iter().take(MAX_REMOTE_BOOKMARKS).collect(),
            bookmark_notice: remote_display_text(app.get_scene_bookmark_notice().as_str(), 512)
                .replace('\n', " "),
            intro_skip_mode: app.get_intro_skip_override_index(),
            skip_segment_kind: app.get_skip_segment_kind(),
            skip_token,
            subtitle_addon_status: app.get_subtitle_addon_status(),
            decoder: app.get_decoder_selected().clamp(0, 2),
            brightness: app.get_player_brightness() as f64,
            automatic_recovery: app.get_automatic_stream_recovery_visible(),
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
                    title: remote_display_text(app.get_player_show_title().as_str(), 512)
                        .replace('\n', " "),
                    episode: remote_display_text(&stream_episode_context_label(video), 512),
                })
            }),
            screenshot_busy: app.get_screenshot_busy(),
            screenshot_saved: remote.screenshot_saved,
            screenshot_failed: remote.screenshot_failed,
            capabilities: PlaybackCapabilities {
                queue: state.watch_queue.active(),
                sleep_timer: active && app.get_player_open(),
                sleep_timer_episodes: active
                    && state
                        .playback
                        .as_ref()
                        .and_then(|p| p.history.as_ref())
                        .is_some_and(|history| history.type_ == "series"),
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
                chapters: started && app.get_player_chapters().row_count() > 0,
                subtitle_style: started,
                secondary_subtitles: started && source_offers,
                intro_skip: started,
                bookmarks: started && source_offers,
                source_recovery: active && app.get_try_another_stream_available(),
                source_offers: active && source_offers,
                subtitle_discovery: started,
                decoder: started
                    && cfg!(any(
                        target_os = "android",
                        target_os = "windows",
                        target_os = "linux"
                    )),
                brightness: started && cfg!(target_os = "android"),
            },
            queue: state.watch_queue.status(
                &profiles::current_id(),
                if app.get_auto_next_preview() {
                    app.get_auto_next_seconds().max(0) as u32
                } else {
                    0
                },
            ),
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
        if !FOREGROUND.load(Ordering::Acquire) || self.app().is_none() {
            done(Response::Error(PlaybackError::Unavailable));
            return;
        }
        if self.app().is_some_and(|app| {
            app.get_profiles_require_selection() || app.global::<crate::MoreBridge>().get_pin_open()
        }) {
            let locked = profile_policy::entry_blocked()
                || self
                    .app()
                    .is_some_and(|app| app.global::<crate::MoreBridge>().get_pin_open());
            done(Response::Error(if locked {
                PlaybackError::Failed("profile_locked".into())
            } else {
                PlaybackError::Unavailable
            }));
            return;
        }
        if !matches!(request, Request::Snapshot)
            && (self.party_active() || self.shared.lock().unwrap().casting.reserved())
        {
            done(Response::Error(PlaybackError::Busy));
            return;
        }
        if !request.valid_payload() {
            done(Response::Error(PlaybackError::InvalidRequest));
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
            Request::QueueStop {
                profile_id,
                run_id,
                expected_session,
            } => {
                let snapshot = self.playback_snapshot();
                if profile_id != profiles::current_id() {
                    done(Response::Error(PlaybackError::Unavailable));
                    return;
                }
                if snapshot.session != expected_session
                    || snapshot
                        .queue
                        .as_ref()
                        .is_none_or(|queue| queue.run_id != run_id)
                {
                    done(Response::Error(PlaybackError::StaleSession));
                    return;
                }
                self.stop_queue();
                done(Response::Snapshot(self.playback_snapshot()));
            }
            Request::QueueStart {
                profile_id,
                item_key,
                expected_session,
            } => {
                if profile_id != profiles::current_id() {
                    done(Response::Error(PlaybackError::Unavailable));
                    return;
                }
                if self.playback_snapshot().session != expected_session {
                    done(Response::Error(PlaybackError::StaleSession));
                    return;
                }
                self.receive_queue_start(peer, item_key.as_deref(), expected_session, false, done);
            }
            Request::Lookup(media) => self.lookup_remote_streams(peer, media, done),
            Request::Start {
                offer_id,
                expected_session,
            } => done(self.start_remote_offer(&peer, &offer_id, expected_session)),
            Request::CurrentOffers { session } => {
                self.lookup_current_remote_streams(peer, session, done)
            }
            Request::Command { session, action } => {
                let snapshot = self.playback_snapshot();
                if snapshot.session != Some(session) {
                    done(Response::Error(PlaybackError::StaleSession));
                    return;
                }
                if !snapshot.ready {
                    done(Response::Error(PlaybackError::Busy));
                    return;
                }
                let action = match self.resolve_remote_action(&snapshot, action) {
                    Ok(action) => action,
                    Err(error) => {
                        done(Response::Error(error));
                        return;
                    }
                };
                if matches!(action, PlaybackAction::QueueSkip) {
                    if let Some(next) = self.queue_successor() {
                        self.queue_skip_cursor(&next.key());
                        self.receive_queue_start(
                            peer,
                            Some(&next.key()),
                            Some(session),
                            true,
                            done,
                        );
                    } else {
                        self.stop_queue();
                        done(Response::Snapshot(self.playback_snapshot()));
                    }
                    return;
                }
                done(
                    match self.dispatch_playback_action_from(action, Some(&peer)) {
                        Ok(()) => Response::Accepted,
                        Err(_) => Response::Error(PlaybackError::Failed(String::new())),
                    },
                );
            }
            Request::ReleasePreview => unreachable!("handled before authority check"),
        }
    }

    fn resolve_remote_action(
        &self,
        snapshot: &PlaybackSnapshot,
        action: PlaybackAction,
    ) -> Result<PlaybackAction, PlaybackError> {
        let action = validate_remote_action(snapshot, action)?;
        // Scene IDs contain local media keys; only the opaque token crosses
        // the connection. Resolve it again at acceptance, never by row index.
        let resolve = |token: &str| {
            self.app().and_then(|app| {
                app.get_scene_bookmarks()
                    .iter()
                    .find(|row| remote_entity_id('b', row.id.as_str()) == token)
                    .map(|row| row.id.to_string())
            })
        };
        Ok(match action {
            PlaybackAction::EditBookmark { id, note } => PlaybackAction::EditBookmark {
                id: resolve(&id).ok_or(PlaybackError::NotFound)?,
                note,
            },
            PlaybackAction::DeleteBookmark { id } => PlaybackAction::DeleteBookmark {
                id: resolve(&id).ok_or(PlaybackError::NotFound)?,
            },
            PlaybackAction::SeekBookmark { id } => PlaybackAction::SeekBookmark {
                id: resolve(&id).ok_or(PlaybackError::NotFound)?,
            },
            action => action,
        })
    }

    fn lookup_current_remote_streams(
        &self,
        peer: PeerContext,
        session: SessionId,
        done: Completion,
    ) {
        let snapshot = self.playback_snapshot();
        if snapshot.session != Some(session) {
            done(Response::Error(PlaybackError::StaleSession));
            return;
        }
        if !snapshot.ready || !snapshot.capabilities.source_offers {
            done(Response::Error(PlaybackError::Unsupported));
            return;
        }
        let Some(media) = self.current_playback_identity() else {
            done(Response::Error(PlaybackError::NotFound));
            return;
        };
        let source = self.player.screenshot_session();
        let b = self.clone();
        let owner = peer.clone();
        self.lookup_remote_streams(
            peer,
            media,
            Box::new(move |response| {
                done(b.finish_current_remote_lookup(&owner, session, source, response))
            }),
        );
    }

    /// The lookup can finish after its controller/source has gone away. Only
    /// these newly allocated offers are retired; unrelated lookups remain valid.
    fn finish_current_remote_lookup(
        &self,
        owner: &PeerContext,
        session: SessionId,
        source: u64,
        response: Response,
    ) -> Response {
        let mut state = self.shared.lock().unwrap();
        let current = owner.is_authorized()
            && FOREGROUND.load(Ordering::Acquire)
            && state.cache_settings.remote_control_enabled
            && !state.party.active()
            && state.remote_playback.session == Some(session)
            && self.player.screenshot_session() == source;
        match response {
            Response::Offers(offers) => {
                for offer in &offers {
                    if current {
                        if let Some(row) = state.remote_playback.offers.get_mut(&offer.id) {
                            row.retry_session = Some((session, source));
                        }
                    } else {
                        state.remote_playback.offers.remove(&offer.id);
                    }
                }
                if current {
                    Response::Offers(offers)
                } else {
                    Response::Error(PlaybackError::StaleSession)
                }
            }
            response => response,
        }
    }

    fn lookup_remote_streams(&self, peer: PeerContext, mut media: MediaIdentity, done: Completion) {
        if !profile_policy::allows_title(
            &profiles::ProfileStore::capture(),
            &media.media_type,
            &media.title_id,
        ) {
            done(Response::Error(PlaybackError::Unavailable));
            return;
        }
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
                                if matches!(
                                    source,
                                    StreamSource::Unsupported | StreamSource::ExternalUrl(_)
                                ) {
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
        let policy_owner = profiles::ProfileStore::capture();
        if !profile_policy::allows_title(&policy_owner, &media.media_type, &media.title_id) {
            done(Response::Error(PlaybackError::Unavailable));
            return;
        }
        // Earlier addon replies may already be buffered when settings change.
        // Never relabel their old account-dependent sources with the new epoch.
        if settings_generation != nova_providers::plugin_settings::settings_generation() {
            done(Response::Error(PlaybackError::NotFound));
            return;
        }
        // Offers belong to this receiver profile. Controller-side caps never
        // remove sources that the target device can use.
        let rules = self.effective_stream_filter_rules();
        let mut rows = rows
            .into_iter()
            .filter(|(row, _)| {
                row.download.as_ref().map_or_else(
                    || profile_policy::allows_addon(&policy_owner, &row.owner_url),
                    |job| smart_offline::allows_download(&policy_owner, job),
                )
            })
            .filter(|(row, _)| {
                matches!(row.source, StreamSource::Downloaded { .. })
                    || rules.accepts(&row.card.filter_facts)
            })
            .collect::<Vec<_>>();
        if rules.enabled {
            rows.sort_by(|(a, _), (b, _)| {
                usize::from(!matches!(a.source, StreamSource::Downloaded { .. }))
                    .cmp(&usize::from(!matches!(
                        b.source,
                        StreamSource::Downloaded { .. }
                    )))
                    .then_with(|| rules.compare(&a.card.filter_facts, &b.card.filter_facts))
            });
        }
        let mut offers = Vec::new();
        // Receiver-owned completed artifacts stay ahead of addon streams.
        for job in self
            .downloads
            .jobs()
            .into_iter()
            .filter(|job| {
                job.media_type == media.media_type
                    && job.media_id == media.title_id
                    && job.request_id == media.episode_id
                    && job.phase == crate::download::DownloadPhase::Completed
                    && smart_offline::allows_download(&policy_owner, job)
            })
            .take(REMOTE_OFFER_LIMIT)
        {
            if let Some(path) = job.artifact_path.clone().filter(|path| path.is_file()) {
                let stream = StreamUi {
                    id: format!("download:{}", job.id),
                    choice_id: None,
                    owner_url: String::new(),
                    display: job.title.clone(),
                    card: downloaded_stream_card(&job),
                    source: StreamSource::Downloaded {
                        job_id: job.id.clone(),
                        path,
                    },
                    addon: text::tr("Downloaded").into(),
                    download: Some(job),
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
                retry_session: None,
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
        let stream = match self.accept_remote_offer(peer, id, expected) {
            Ok(stream) => stream,
            Err(error) => return Response::Error(error),
        };
        self.launch_remote_offer(stream)
    }

    fn launch_remote_offer(&self, stream: StreamUi) -> Response {
        self.note_selected_playback_stream(&stream);
        self.play_remote_source(stream.source, &stream.display);
        let snapshot = self.playback_snapshot();
        if snapshot.session.is_none() {
            Response::Error(PlaybackError::Failed(String::new()))
        } else {
            Response::Snapshot(snapshot)
        }
    }

    /// Accept while the old source still owns its timeline/preferences. Native
    /// launch is a separate step and may fail synchronously on this device.
    fn accept_remote_offer(
        &self,
        peer: &PeerContext,
        id: &str,
        expected: Option<SessionId>,
    ) -> Result<StreamUi, PlaybackError> {
        let offer = {
            let mut state = self.shared.lock().unwrap();
            let r = &state.remote_playback;
            if r.role == Role::Remote {
                return Err(PlaybackError::Busy);
            }
            if r.session != expected {
                return Err(PlaybackError::StaleSession);
            }
            let Some(offer) = r.offers.get(id) else {
                return Err(PlaybackError::NotFound);
            };
            if offer.retry_session.is_some_and(|(session, source)| {
                expected != Some(session) || self.player.screenshot_session() != source
            }) {
                return Err(PlaybackError::StaleSession);
            }
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &offer.media.media_type,
                &offer.media.title_id,
            ) || !offer.stream.download.as_ref().map_or_else(
                || {
                    profile_policy::allows_addon(
                        &profiles::ProfileStore::capture(),
                        &offer.stream.owner_url,
                    )
                },
                |j| smart_offline::allows_download(&profiles::ProfileStore::capture(), j),
            ) || offer.peer != peer.peer_id
                || offer.created.elapsed() > Duration::from_secs(300)
                || offer.settings_generation
                    != nova_providers::plugin_settings::settings_generation()
                || (!matches!(offer.stream.source, StreamSource::Downloaded { .. })
                    && !stream_filters::effective_rules(&state.cache_settings)
                        .accepts(&offer.stream.card.filter_facts))
                || (!offer.stream.owner_url.is_empty()
                    && !state.installed.iter().any(|a| {
                        a.url == offer.stream.owner_url
                            && a.enabled
                            && a.available
                            && a.generation == offer.addon_generation
                    }))
            {
                return Err(PlaybackError::NotFound);
            }
            state.remote_playback.offers.remove(id).unwrap()
        };
        // Capture at acceptance, after lookup latency, while the old source
        // still owns its playhead and semantic track preferences.
        let retry = if offer.retry_session.is_some() {
            let Some(retry) = self.capture_remote_retry() else {
                return Err(PlaybackError::StaleSession);
            };
            Some(retry)
        } else {
            None
        };
        if expected.is_some() {
            if retry.is_some() {
                self.player.with_preserved_fullscreen_on_close(|| {
                    let _ = self.dispatch_playback_action(PlaybackAction::Stop);
                });
            } else {
                let _ = self.dispatch_playback_action(PlaybackAction::Stop);
            }
        }
        self.queue_note_stream_for(
            &offer.media.media_type,
            &offer.media.title_id,
            &offer.media.episode_id,
            &offer.stream,
        );
        if let Some(identity) = offer.stream.choice_id.clone() {
            self.last_stream.remember(identity, now_secs());
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.start_context = Some(offer.media);
            state.remote_playback.start_retry = retry;
        }
        Ok(offer.stream)
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
            StreamSource::Unsupported | StreamSource::ExternalUrl(_) => {}
        }
        let mut state = self.shared.lock().unwrap();
        state.remote_playback.start_context = None;
        state.remote_playback.start_retry = None;
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
            r.current_offers_session = None;
            r.visible_offers.clear();
            r.visible_media = None;
            r.scene_resume = None;
            r.replacement = None;
            std::mem::take(&mut r.target)
        };
        if let Some(app) = self.app() {
            app.set_remote_controls_open(false);
            app.set_remote_connected(false);
            app.set_remote_loading(false);
            app.set_remote_target_id(SharedString::default());
            app.set_remote_replacement_token(SharedString::default());
            if !app.get_remote_controls_visual_retained() {
                self.clear_remote_controls_visual();
            }
        }
        if !target.is_empty()
            && let Some(app) = self.app()
        {
            app.set_streams(Rc::new(VecModel::from(Vec::<StreamRow>::new())).into());
            if !app.get_remote_controls_visual_retained() {
                app.set_remote_offers(
                    Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
                );
            }
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

    fn remote_controls_visual_closed(&self) {
        let Some(app) = self.app() else { return };
        let detached = self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .target
            .is_empty();
        if !app.get_remote_controls_open() && detached {
            self.clear_remote_controls_visual();
        }
    }

    fn clear_remote_controls_visual(&self) {
        let Some(app) = self.app() else { return };
        app.set_remote_session(SharedString::default());
        app.set_remote_title(SharedString::default());
        app.set_remote_episode(SharedString::default());
        app.set_remote_prompt(SharedString::default());
        app.set_remote_skip_token(SharedString::default());
        app.set_remote_audio_tracks(Rc::new(VecModel::from(Vec::<crate::TrackRow>::new())).into());
        app.set_remote_sub_tracks(Rc::new(VecModel::from(Vec::<crate::TrackRow>::new())).into());
        app.set_remote_secondary_sub_tracks(
            Rc::new(VecModel::from(Vec::<crate::TrackRow>::new())).into(),
        );
        app.set_remote_chapters(
            Rc::new(VecModel::from(Vec::<crate::RemoteChapterRow>::new())).into(),
        );
        app.set_remote_bookmarks(
            Rc::new(VecModel::from(Vec::<crate::RemoteBookmarkRow>::new())).into(),
        );
        app.set_remote_offers(Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into());
    }

    fn open_remote_controls(&self, target: &str) {
        if self.shared.lock().unwrap().casting.reserved() {
            if let Some(app) = self.app() {
                app.set_remote_error(
                    text::tr("Stop casting before joining a watch party or paired Nova session.")
                        .into(),
                );
            }
            return;
        }
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
                                        media: media.clone(),
                                        offers: offers.clone(),
                                        started,
                                    },
                                );
                            }
                            state.remote_playback.visible_media = Some(media);
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
        state.remote_playback.visible_media = Some(media.clone());
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
        let (target, offer, generation, media, retry_session) = {
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
            (
                r.target.clone(),
                offer.id.clone(),
                r.generation,
                (!self.app().is_some_and(|a| a.get_remote_controls_open()))
                    .then(|| r.visible_media.clone())
                    .flatten(),
                r.current_offers_session
                    .filter(|_| self.app().is_some_and(|a| a.get_remote_controls_open())),
            )
        };
        let scene = media.and_then(|media| {
            let position = self.take_remote_bookmark_resume(&media)?;
            Some(SceneStart {
                media,
                position,
                offer: offer.clone(),
                profile: profiles::capture(),
            })
        });
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
                if b.shared.lock().unwrap().remote_playback.generation != generation
                    || !nova_sync::engine().is_some_and(|current| Arc::ptr_eq(&current, &engine))
                {
                    return;
                }
                match response {
                    Ok(Response::Snapshot(snapshot))
                        if snapshot.ready && snapshot.valid_payload() =>
                    {
                        if let Some(session) = retry_session {
                            if snapshot.session == Some(session) {
                                b.send_remote_start(offer, Some(session), None);
                            } else if let Some(app) = b.app() {
                                app.set_remote_error(
                                    error_text(&PlaybackError::StaleSession).into(),
                                );
                            }
                            return;
                        }
                        if snapshot.session.is_some() {
                            let token = format!("{}:{generation}:{offer}", *BOOT);
                            b.shared.lock().unwrap().remote_playback.replacement =
                                Some(PendingReplacement {
                                    token: token.clone(),
                                    offer,
                                    session: snapshot.session,
                                    scene,
                                });
                            if let Some(app) = b.app() {
                                app.set_remote_replacement_title(snapshot.title.into());
                                app.set_remote_replacement_token(token.into());
                            }
                        } else {
                            b.send_remote_start(offer, None, scene);
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
            self.send_remote_start(pending.offer, pending.session, pending.scene);
        }
    }

    fn send_remote_start(
        &self,
        offer: String,
        expected: Option<SessionId>,
        scene: Option<SceneStart>,
    ) {
        if scene.as_ref().is_some_and(|scene| {
            scene.profile != profiles::active_context() || !profiles::is_current(&scene.profile)
        }) {
            return;
        }
        let target = self.shared.lock().unwrap().remote_playback.target.clone();
        // Reserve the controller role before network I/O, including start prompts.
        if self.shared.lock().unwrap().remote_playback.role == Role::Local {
            return;
        }
        // Retain selection offers across opening the controller.
        self.open_remote_controls(&target);
        {
            let mut state = self.shared.lock().unwrap();
            if state.remote_playback.target != target || state.remote_playback.role != Role::Remote
            {
                return;
            }
            state.remote_playback.scene_resume = scene
                .filter(|scene| {
                    scene.offer == offer
                        && scene.profile == profiles::active_context()
                        && profiles::is_current(&scene.profile)
                        && !scene.media.media_type.is_empty()
                        && !scene.media.title_id.is_empty()
                        && !scene.media.episode_id.is_empty()
                })
                .map(RemoteSceneResume::new);
        }
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
        if name == "queue_continue" {
            self.remote_start_queue(None);
            return;
        }
        if name == "queue_stop" {
            self.remote_stop_queue();
            return;
        }
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
            "queue_skip" => PlaybackAction::QueueSkip,
            "queue_stop" => PlaybackAction::QueueStop,
            "queue_now" => PlaybackAction::QueuePlayNow,
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
            "audio" => {
                let Some(row) = snapshot
                    .audio_tracks
                    .iter()
                    .find(|row| row.id == value as i32)
                else {
                    return;
                };
                PlaybackAction::SelectAudioTrackToken(row.token.clone())
            }
            "subtitle" if value == -1.0 => PlaybackAction::SelectSubtitleTrackToken("off".into()),
            "subtitle" => {
                let Some(row) = snapshot
                    .subtitle_tracks
                    .iter()
                    .find(|row| row.id == value as i32)
                else {
                    return;
                };
                PlaybackAction::SelectSubtitleTrackToken(row.token.clone())
            }
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
                        || state.remote_playback.queued.iter().any(is_lookup_request)
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
        self.queue_remote_action(session, action);
    }

    fn remote_semantic_action(&self, name: &str, value: f32, id: &str, note: &str, token: &str) {
        if !FOREGROUND.load(Ordering::Acquire) || !value.is_finite() {
            return;
        }
        let Some(snapshot) = self.shared.lock().unwrap().remote_playback.snapshot.clone() else {
            return;
        };
        let Some(session) = snapshot.session else {
            return;
        };
        if token != session_text(Some(session)) {
            return;
        }
        let integer = value.fract() == 0.0;
        let action = match name {
            "sleep_minutes" if integer && (1.0..=240.0).contains(&value) => {
                PlaybackAction::SetSleepTimer(nova_playback_api::SleepTimerMode::Minutes(
                    value as u16,
                ))
            }
            "sleep_episodes" if integer && (1.0..=10.0).contains(&value) => {
                PlaybackAction::SetSleepTimer(nova_playback_api::SleepTimerMode::Episodes(
                    value as u8,
                ))
            }
            "sleep_end" => {
                PlaybackAction::SetSleepTimer(nova_playback_api::SleepTimerMode::EndOfItem)
            }
            "sleep_cancel" => PlaybackAction::CancelSleepTimer,
            "sleep_extend" => PlaybackAction::ExtendSleepTimer,
            "chapter" => PlaybackAction::SeekChapter { id: id.into() },
            "subtitle_style" => {
                let Ok(control) = id.parse::<i32>() else {
                    return;
                };
                PlaybackAction::SetSubtitleStyle {
                    control,
                    value: value as f64,
                }
            }
            "subtitle_style_reset" => PlaybackAction::ResetSubtitleStyle,
            "secondary_subtitle" if value == -1.0 => {
                PlaybackAction::SelectSecondarySubtitleTrackToken("off".into())
            }
            "secondary_subtitle" if integer => {
                let Some(row) = snapshot
                    .secondary_subtitle_tracks
                    .iter()
                    .find(|row| row.id == value as i32)
                else {
                    return;
                };
                PlaybackAction::SelectSecondarySubtitleTrackToken(row.token.clone())
            }
            "secondary_position" if integer => {
                PlaybackAction::SetSecondarySubtitlePosition(value as i32)
            }
            "intro_mode" if integer => PlaybackAction::SetIntroSkipMode(value as i32),
            "skip" => PlaybackAction::SkipSegment { token: id.into() },
            "skip_dismiss" => PlaybackAction::DismissSkipSegment { token: id.into() },
            "bookmark_add" => PlaybackAction::AddBookmark { note: note.into() },
            "bookmark_edit" => PlaybackAction::EditBookmark {
                id: id.into(),
                note: note.into(),
            },
            "bookmark_delete" => PlaybackAction::DeleteBookmark { id: id.into() },
            "bookmark_seek" => PlaybackAction::SeekBookmark { id: id.into() },
            "try_another" => PlaybackAction::TryAnotherStream,
            "subtitle_retry" => PlaybackAction::RetrySubtitleDiscovery,
            "subtitle_refresh" => PlaybackAction::RefreshSubtitleTracks,
            "subtitle_cancel" if integer && (0.0..=1.0).contains(&value) => {
                PlaybackAction::CancelSubtitleDownload {
                    secondary: value == 1.0,
                }
            }
            "decoder" if integer => PlaybackAction::SetDecoder(value as i32),
            "brightness" => PlaybackAction::SetBrightness(value as f64),
            "source_offers" if snapshot.capabilities.source_offers => {
                let mut state = self.shared.lock().unwrap();
                let r = &mut state.remote_playback;
                if r.lookup_in_flight
                    || r.queued.len() >= 32
                    || r.queued.iter().any(is_lookup_request)
                {
                    return;
                }
                r.remote_offers.clear();
                r.current_offers_session = None;
                r.queued.push_back(Request::CurrentOffers { session });
                drop(state);
                if let Some(app) = self.app() {
                    app.set_remote_offers(
                        Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
                    );
                }
                self.remote_tick();
                return;
            }
            _ => return,
        };
        if validate_remote_action(&snapshot, action.clone()).is_err() {
            self.publish_remote_semantic_ack(name, id, session, false);
            return;
        }
        self.queue_remote_action(session, action);
    }

    fn publish_remote_semantic_ack(&self, name: &str, id: &str, session: SessionId, success: bool) {
        let Some(app) = self.app() else { return };
        app.set_remote_semantic_name(name.into());
        app.set_remote_semantic_id(id.into());
        app.set_remote_semantic_session(session_text(Some(session)).into());
        app.set_remote_semantic_success(success);
        app.set_remote_semantic_notice(if success {
            SharedString::default()
        } else {
            error_text(&PlaybackError::Failed(String::new())).into()
        });
        app.set_remote_semantic_revision(app.get_remote_semantic_revision().wrapping_add(1));
    }

    fn queue_remote_action(&self, session: SessionId, action: PlaybackAction) {
        if !action.valid_payload() {
            return;
        }
        let mut state = self.shared.lock().unwrap();
        // A bounded command queue preserves order without replay on reconnect.
        let r = &mut state.remote_playback;
        if r.role != Role::Remote
            || r.target.is_empty()
            || r.snapshot.as_ref().and_then(|s| s.session) != Some(session)
        {
            return;
        }
        r.interacting_until = Some(Instant::now() + Duration::from_secs(1));
        if matches!(
            action,
            PlaybackAction::SeekAbsolute(_)
                | PlaybackAction::SeekRelative(_)
                | PlaybackAction::Stop
                | PlaybackAction::Resume(_)
                | PlaybackAction::CancelResume
        ) {
            r.scene_resume = None;
        }
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
        } else {
            let metadata = semantic_request(&Request::Command { session, action });
            drop(state);
            if let Some((name, id, session)) = metadata {
                self.publish_remote_semantic_ack(name, &id, session, false);
            }
            return;
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
        let changed = {
            let state = self.shared.lock().unwrap();
            let old = state
                .remote_playback
                .snapshot
                .as_ref()
                .filter(|old| old.session == snapshot.session);
            (
                old.is_none_or(|old| {
                    old.audio_tracks != snapshot.audio_tracks
                        || old.audio_track != snapshot.audio_track
                }),
                old.is_none_or(|old| {
                    old.subtitle_tracks != snapshot.subtitle_tracks
                        || old.subtitle_track != snapshot.subtitle_track
                }),
                old.is_none_or(|old| {
                    old.secondary_subtitle_tracks != snapshot.secondary_subtitle_tracks
                        || old.secondary_subtitle_track != snapshot.secondary_subtitle_track
                }),
                old.is_none_or(|old| old.subtitle_style != snapshot.subtitle_style),
                old.is_none_or(|old| old.chapters != snapshot.chapters),
                old.is_none_or(|old| old.bookmarks != snapshot.bookmarks),
            )
        };
        app.global::<crate::RemoteBridge>()
            .set_queue_active(snapshot.queue.is_some());
        app.global::<crate::RemoteBridge>()
            .set_queue_countdown(snapshot.queue.as_ref().map_or(0, |q| q.countdown as i32));
        app.global::<crate::RemoteBridge>().set_queue_phase(
            snapshot
                .queue
                .as_ref()
                .map(|q| {
                    text::tr(match q.phase.as_str() {
                        "finished" => "Finished",
                        "paused" => "Queue paused",
                        "countdown" => "Next queued item",
                        "ready" => "Choose a stream",
                        _ => "Queue playing",
                    })
                })
                .unwrap_or("")
                .into(),
        );
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
        app.set_remote_sleep_timer_available(snapshot.capabilities.sleep_timer);
        app.set_remote_sleep_timer_episodes_available(snapshot.capabilities.sleep_timer_episodes);
        app.set_remote_sleep_timer_duration(snapshot.sleep_timer.as_ref().is_some_and(|timer| {
            matches!(timer.mode, nova_playback_api::SleepTimerMode::Minutes(_))
        }));
        app.set_remote_sleep_timer_active(snapshot.sleep_timer.is_some());
        app.set_remote_sleep_timer_label(
            snapshot
                .sleep_timer
                .as_ref()
                .map(|timer| {
                    text::sleep_timer_status(
                        match timer.mode {
                            nova_playback_api::SleepTimerMode::Minutes(_) => 1,
                            nova_playback_api::SleepTimerMode::EndOfItem => 2,
                            _ => 3,
                        },
                        timer.remaining_seconds,
                        timer.remaining_episodes,
                    )
                })
                .unwrap_or_else(|| text::tr("Off").into())
                .into(),
        );
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
        if changed.0 {
            app.set_remote_audio_tracks(
                tracks(&snapshot.audio_tracks, snapshot.audio_track).into(),
            );
        }
        if changed.1 {
            app.set_remote_sub_tracks(
                tracks(&snapshot.subtitle_tracks, snapshot.subtitle_track).into(),
            );
        }
        if changed.2 {
            app.set_remote_secondary_sub_tracks(
                tracks(
                    &snapshot.secondary_subtitle_tracks,
                    snapshot.secondary_subtitle_track,
                )
                .into(),
            );
        }
        app.set_remote_secondary_subtitle_position(snapshot.secondary_subtitle_position);
        if changed.3 {
            app.set_remote_subtitle_style(
                Rc::new(VecModel::from(
                    snapshot.subtitle_style.map(|v| v as f32).to_vec(),
                ))
                .into(),
            );
        }
        if changed.4 {
            app.set_remote_chapters(
                Rc::new(VecModel::from(
                    snapshot
                        .chapters
                        .iter()
                        .map(|row| crate::RemoteChapterRow {
                            id: row.id.clone().into(),
                            title: row.label.clone().into(),
                            time: crate::player::format_time(row.time).into(),
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
        if changed.5 {
            app.set_remote_bookmarks(
                Rc::new(VecModel::from(
                    snapshot
                        .bookmarks
                        .iter()
                        .map(|row| crate::RemoteBookmarkRow {
                            id: row.id.clone().into(),
                            note: row.note.clone().into(),
                            time: crate::player::format_time(row.time).into(),
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
        app.set_remote_chapters_truncated(snapshot.chapters_truncated);
        app.set_remote_bookmarks_truncated(snapshot.bookmarks_truncated);
        app.set_remote_bookmark_notice(snapshot.bookmark_notice.clone().into());
        app.set_remote_intro_skip_mode(snapshot.intro_skip_mode);
        app.set_remote_skip_segment_kind(snapshot.skip_segment_kind);
        app.set_remote_skip_token(snapshot.skip_token.clone().into());
        app.set_remote_subtitle_addon_status(snapshot.subtitle_addon_status);
        app.set_remote_decoder(snapshot.decoder);
        app.set_remote_brightness(snapshot.brightness as f32);
        app.set_remote_automatic_recovery(snapshot.automatic_recovery);
        app.set_remote_can_queue(snapshot.capabilities.queue);
        app.set_remote_can_chapters(snapshot.capabilities.chapters);
        app.set_remote_can_subtitle_style(snapshot.capabilities.subtitle_style);
        app.set_remote_can_secondary_subtitles(snapshot.capabilities.secondary_subtitles);
        app.set_remote_can_intro_skip(snapshot.capabilities.intro_skip);
        app.set_remote_can_bookmarks(snapshot.capabilities.bookmarks);
        app.set_remote_can_source_recovery(snapshot.capabilities.source_recovery);
        app.set_remote_can_source_offers(snapshot.capabilities.source_offers);
        app.set_remote_can_subtitle_discovery(snapshot.capabilities.subtitle_discovery);
        app.set_remote_can_decoder(snapshot.capabilities.decoder);
        app.set_remote_can_brightness(snapshot.capabilities.brightness);
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
            state.remote_playback.queued.retain(|r| {
                !matches!(
                    r,
                    Request::Command { .. } | Request::Lookup(_) | Request::CurrentOffers { .. }
                )
            });
            state.remote_playback.remote_offers.clear();
            app.set_remote_offers(
                Rc::new(VecModel::from(Vec::<crate::RemoteOfferRow>::new())).into(),
            );
        }
        if let Some(command) = state.remote_playback.scene_resume_command(&snapshot) {
            state.remote_playback.queued.push_front(command);
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
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.queued.clear();
            state.remote_playback.scene_resume = None;
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
                self.shared.lock().unwrap().remote_playback.scene_resume = None;
                app.set_remote_connected(false);
                return;
            };
            let (target, generation, request, is_lookup, lookup_session, queue_revision) = {
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
                if r.lookup_in_flight && r.queued.front().is_some_and(is_lookup_request) {
                    return;
                }
                let request = r.queued.pop_front().unwrap_or(Request::Snapshot);
                r.control_poll.attempted = Some(now);
                let is_lookup = is_lookup_request(&request);
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
                    r.queue_revision,
                )
            };
            let queue_request = matches!(&request, Request::QueueStart { .. });
            let queue_lookup = queue_request
                || matches!(
                    &request,
                    Request::Command {
                        action: PlaybackAction::QueueSkip,
                        ..
                    }
                );
            let current_offers = matches!(&request, Request::CurrentOffers { .. });
            let semantic = semantic_request(&request);
            let started_offer = match &request {
                Request::Start {
                    offer_id,
                    expected_session,
                } => Some((offer_id.clone(), *expected_session)),
                _ => None,
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
                    state.remote_playback.scene_resume = None;
                    drop(state);
                    app.set_remote_error(error_text(&PlaybackError::Unavailable).into());
                    if let Some((name, id, session)) = semantic {
                        self.publish_remote_semantic_ack(name, &id, session, false);
                    }
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
                    let response = match response {
                        Ok(Response::Snapshot(snapshot)) if !snapshot.valid_payload() => {
                            Ok(Response::Error(PlaybackError::InvalidRequest))
                        }
                        Ok(Response::Offers(offers))
                            if offers.len() > REMOTE_OFFER_LIMIT
                                || offers.iter().any(|o| {
                                    o.id.is_empty()
                                        || o.id.len() > 64
                                        || o.name.len() > 128
                                        || o.description.len() > 512
                                        || o.size.len() > 32
                                }) =>
                        {
                            Ok(Response::Error(PlaybackError::InvalidRequest))
                        }
                        response => response,
                    };
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
                            state.remote_playback.scene_resume = None;
                            return;
                        }
                        if queue_lookup && state.remote_playback.queue_revision != queue_revision {
                            return;
                        }
                        if is_lookup {
                            state.remote_playback.lookup_in_flight = false;
                        } else {
                            state.remote_playback.in_flight = false;
                        }
                        if !FOREGROUND.load(Ordering::Acquire) {
                            state.remote_playback.queued.clear();
                            state.remote_playback.scene_resume = None;
                            return;
                        }
                        let r = &mut state.remote_playback;
                        if let Some((offer, expected)) = &started_offer {
                            let accepted = match (&mut r.scene_resume, &response) {
                                (Some(pending), Ok(Response::Snapshot(snapshot))) => {
                                    pending.accept_start(offer, *expected, snapshot)
                                }
                                _ => false,
                            };
                            if !accepted {
                                r.scene_resume = None;
                            }
                        } else if !matches!(
                            &response,
                            Ok(Response::Snapshot(_) | Response::Accepted | Response::Offers(_))
                        ) {
                            r.scene_resume = None;
                        }
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
                    if let Some((name, id, session)) = semantic {
                        b.publish_remote_semantic_ack(
                            name,
                            &id,
                            session,
                            matches!(&response, Ok(Response::Accepted | Response::Snapshot(_))),
                        );
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
                            let mut state = b.shared.lock().unwrap();
                            state.remote_playback.remote_offers = offers;
                            state.remote_playback.current_offers_session =
                                if current_offers { lookup_session } else { None };
                        }
                        Ok(Response::Accepted) => {}
                        Ok(Response::Error(error)) => {
                            if let Some(app) = b.app() {
                                app.set_remote_error(if queue_request && error == PlaybackError::NotFound { text::tr("Queue item is unavailable on this device. Sync, then retry or choose a source on the TV.").to_owned() } else if queue_request && error == PlaybackError::Unavailable { text::tr("Select and unlock the same profile on the playing device.").to_owned() } else { error_text(&error) }.into());
                            }
                            if !is_lookup
                                && matches!(
                                    error,
                                    PlaybackError::Unauthorized
                                        | PlaybackError::StaleSession
                                        | PlaybackError::Unavailable
                                )
                            {
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
    fn pending_queue_stop_uses_control_lane_without_a_native_session() {
        let mut remote = State {
            lookup_in_flight: true,
            snapshot: Some(PlaybackSnapshot {
                ready: true,
                queue: Some(nova_playback_api::QueueStatus {
                    profile_id: "default".into(),
                    run_id: 7,
                    item_key: "item".into(),
                    phase: "ready".into(),
                    countdown: 0,
                    autoplay: true,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        remote.queued.push_back(Request::QueueStart {
            profile_id: "default".into(),
            item_key: None,
            expected_session: None,
        });
        let old_revision = remote.queue_revision;
        let request = remote.queue_stop_request().unwrap();
        assert!(
            !is_lookup_request(&request),
            "stop must pass an in-flight source lookup"
        );
        assert!(is_lookup_request(&Request::QueueStart {
            profile_id: "default".into(),
            item_key: None,
            expected_session: None
        }));
        assert_eq!(
            request,
            Request::QueueStop {
                profile_id: "default".into(),
                run_id: 7,
                expected_session: None
            }
        );
        assert!(!remote.lookup_in_flight);
        assert_eq!(
            remote.queued.len(),
            1,
            "queued starts cannot run after cancellation"
        );
        assert_ne!(
            remote.queue_revision, old_revision,
            "late lookup snapshots are rejected"
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn pending_queue_stop_cancels_intent_and_rejects_previous_run() {
        const ROOT: &str = "NOVA_PENDING_QUEUE_STOP_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-pending-queue-stop-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root).args(["--exact","app::remote_playback::tests::pending_queue_stop_cancels_intent_and_rejects_previous_run","--nocapture"]).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let key = r#"["movie","movie-id","movie-id"]"#;
        storage::try_set_str(watch_queue::KEY,&serde_json::json!({"version":2,"entries":[{"type_":"movie","series_id":"movie-id","episode_id":"movie-id","title":"Queued movie","poster_url":"","episode_title":"","season":null,"episode":null}],"order":[key]}).to_string()).unwrap();
        bridge.start_queue(Some(key)).unwrap();
        let (run_id, _) = bridge.queue_run_entry().unwrap();
        let snapshot = bridge.playback_snapshot();
        assert!(
            snapshot.session.is_none(),
            "source lookup has no native session"
        );
        let peer = PeerContext::new("paired".into(), 1, 1, Arc::new(|| true));
        let request = Request::QueueStop {
            profile_id: profiles::current_id(),
            run_id,
            expected_session: None,
        };
        let response = Arc::new(Mutex::new(None));
        let observed = response.clone();
        bridge.receive_playback_request(
            peer.clone(),
            request.clone(),
            Box::new(move |value| *observed.lock().unwrap() = Some(value)),
        );
        assert!(matches!(
            response.lock().unwrap().as_ref(),
            Some(Response::Snapshot(_))
        ));
        assert!(bridge.queue_run_entry().is_none());
        assert!(!app.get_player_open());
        bridge.start_queue(Some(key)).unwrap();
        let (resolving, _) = bridge.queue_run_entry().unwrap();
        let pending = bridge.begin_playback_request();
        assert!(pending.is_current());
        let response = Arc::new(Mutex::new(None));
        let observed = response.clone();
        bridge.receive_playback_request(
            peer.clone(),
            Request::QueueStop {
                profile_id: profiles::current_id(),
                run_id: resolving,
                expected_session: bridge.playback_snapshot().session,
            },
            Box::new(move |value| *observed.lock().unwrap() = Some(value)),
        );
        assert!(matches!(
            response.lock().unwrap().as_ref(),
            Some(Response::Snapshot(_))
        ));
        assert!(!pending.is_current());
        assert!(!bridge.playback_request_is_current(&pending));
        assert!(bridge.queue_run_entry().is_none());
        bridge.start_queue(Some(key)).unwrap();
        let (replacement, _) = bridge.queue_run_entry().unwrap();
        assert_ne!(replacement, run_id);
        let response = Arc::new(Mutex::new(None));
        let observed = response.clone();
        bridge.receive_playback_request(
            peer,
            request,
            Box::new(move |value| *observed.lock().unwrap() = Some(value)),
        );
        assert_eq!(
            response.lock().unwrap().as_ref(),
            Some(&Response::Error(PlaybackError::StaleSession))
        );
        assert_eq!(bridge.queue_run_entry().unwrap().0, replacement);
        bridge.open_preview(
            MetaPreview {
                id: "unrelated".into(),
                type_: "movie".into(),
                name: "Another movie".into(),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        let unrelated = bridge.begin_playback_request();
        assert!(unrelated.is_current());
        assert!(
            bridge.queue_run_entry().is_none(),
            "an unrelated accepted start cancels a pending queue rather than inheriting its EOF ownership"
        );
    }
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
    fn current_entities_are_required_even_when_numeric_ids_are_reused() {
        let mut snapshot = PlaybackSnapshot {
            capabilities: PlaybackCapabilities {
                tracks: true,
                secondary_subtitles: true,
                chapters: true,
                bookmarks: true,
                intro_skip: true,
                subtitle_style: true,
                ..Default::default()
            },
            subtitle_tracks: vec![PlaybackTrack {
                id: -2,
                token: "new-addon".into(),
                label: "English".into(),
            }],
            secondary_subtitle_tracks: vec![PlaybackTrack {
                id: 7,
                token: "new-native".into(),
                label: "Croatian".into(),
            }],
            chapters: vec![PlaybackChapter {
                id: "chapter-current".into(),
                label: "Scene".into(),
                time: 30.0,
            }],
            bookmarks: vec![PlaybackBookmark {
                id: "bookmark-current".into(),
                note: "Scene".into(),
                time: 31.0,
            }],
            skip_token: "skip-current".into(),
            ..Default::default()
        };
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SelectSubtitleTrackToken("new-addon".into())
            ),
            Ok(PlaybackAction::SelectSubtitleTrack(-2))
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SelectSubtitleTrackToken("old-addon".into())
            ),
            Err(PlaybackError::InvalidRequest)
        );
        assert_eq!(
            validate_remote_action(&snapshot, PlaybackAction::SelectSubtitleTrack(-2)),
            Err(PlaybackError::Unsupported)
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SelectSecondarySubtitleTrackToken("off".into())
            ),
            Ok(PlaybackAction::SelectSecondarySubtitleTrack(-1))
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SeekChapter {
                    id: "chapter-old".into()
                }
            ),
            Err(PlaybackError::NotFound)
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::EditBookmark {
                    id: "bookmark-old".into(),
                    note: "Keep draft".into()
                }
            ),
            Err(PlaybackError::NotFound)
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SkipSegment {
                    token: "skip-old".into()
                }
            ),
            Err(PlaybackError::Unsupported)
        );
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::SetSubtitleStyle {
                    control: 0,
                    value: f64::INFINITY
                }
            ),
            Err(PlaybackError::InvalidRequest)
        );
        snapshot.capabilities.bookmarks = false;
        assert_eq!(
            validate_remote_action(
                &snapshot,
                PlaybackAction::AddBookmark {
                    note: "Scene".into()
                }
            ),
            Err(PlaybackError::Unsupported)
        );
    }

    #[test]
    fn bounded_track_lists_retain_the_selected_choice_and_profile_sessions_never_alias() {
        let rows = (0..100)
            .map(|id| PlaybackTrack {
                id,
                token: format!("t{id}"),
                label: id.to_string(),
            })
            .collect();
        let rows = bounded_tracks(rows, 99);
        assert_eq!(rows.len(), MAX_REMOTE_TRACKS);
        assert_eq!(rows[0].id, 0);
        assert_eq!(rows.last().unwrap().id, 99);
        let mut first_profile = State::default();
        first_profile.begin();
        let mut second_profile = State::default();
        second_profile.begin();
        assert_ne!(first_profile.session, second_profile.session);
        first_profile.ended();
        first_profile.begin();
        assert_ne!(first_profile.session, second_profile.session);
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

    fn scene_start() -> SceneStart {
        SceneStart {
            media: MediaIdentity {
                media_type: "series".into(),
                title_id: "show".into(),
                episode_id: "show:1:2".into(),
                ..Default::default()
            },
            position: 137.25,
            offer: "receiver-opaque-offer".into(),
            profile: profiles::active_context(),
        }
    }
    fn scene_snapshot(sequence: u64) -> PlaybackSnapshot {
        PlaybackSnapshot {
            ready: true,
            session: Some(SessionId {
                epoch: 77,
                sequence,
            }),
            phase: PlaybackPhase::Loading,
            ..Default::default()
        }
    }

    #[test]
    fn saved_scene_binds_exact_start_and_seeks_once_after_dismissing_resume_prompt() {
        // Replacement confirmation retains both the content-bound offer and
        // exact scene, while its CAS session remains the old receiver session.
        let replacement = PendingReplacement {
            token: "confirmation".into(),
            offer: "receiver-opaque-offer".into(),
            session: scene_snapshot(1).session,
            scene: Some(scene_start()),
        };
        let mut waiting = State {
            scene_resume: Some(RemoteSceneResume::new(replacement.scene.unwrap())),
            ..Default::default()
        };
        let mut unrelated = scene_snapshot(90);
        unrelated.capabilities.seek = true;
        assert!(
            waiting.scene_resume_command(&unrelated).is_none(),
            "polling cannot bind a session before Start responds"
        );
        let mut scene = waiting.scene_resume.take().unwrap();
        let mut loading = scene_snapshot(2);
        assert!(!scene.accept_start("unrelated-offer", replacement.session, &loading));
        assert!(scene.session.is_none());
        assert!(!scene.accept_start(&replacement.offer, replacement.session, &scene_snapshot(1)));
        assert!(scene.accept_start(&replacement.offer, replacement.session, &loading));
        let mut state = State {
            scene_resume: Some(scene),
            ..Default::default()
        };
        assert!(state.scene_resume_command(&loading).is_none());
        loading.resume_prompt = true;
        loading.capabilities.resume = true;
        assert_eq!(
            state.scene_resume_command(&loading),
            Some(Request::Command {
                session: loading.session.unwrap(),
                action: PlaybackAction::Resume(false),
            })
        );
        assert!(
            state.scene_resume_command(&loading).is_none(),
            "do not repeat prompt commands"
        );
        loading.resume_prompt = false;
        loading.phase = PlaybackPhase::Playing;
        loading.capabilities.seek = true;
        assert_eq!(
            state.scene_resume_command(&loading),
            Some(Request::Command {
                session: loading.session.unwrap(),
                action: PlaybackAction::SeekAbsolute(137.25),
            })
        );
        assert!(
            state.scene_resume_command(&loading).is_none(),
            "never seek twice"
        );
    }

    #[test]
    fn saved_scene_never_seeks_replaced_session_or_survives_profile_and_engine_changes() {
        let accepted = scene_snapshot(2);
        let pending = || {
            let mut scene = RemoteSceneResume::new(scene_start());
            assert!(scene.accept_start("receiver-opaque-offer", None, &accepted));
            scene
        };
        let mut state = State {
            scene_resume: Some(pending()),
            ..Default::default()
        };
        let mut unrelated = scene_snapshot(3);
        unrelated.capabilities.seek = true;
        assert!(state.scene_resume_command(&unrelated).is_none());
        assert!(state.scene_resume.is_none());
        let mut stale = pending();
        stale.start.profile.epoch = stale.start.profile.epoch.wrapping_add(1);
        state.scene_resume = Some(stale);
        assert!(state.scene_resume_command(&accepted).is_none());
        assert!(state.scene_resume.is_none());
        state.scene_resume = Some(pending());
        let old_generation = state.generation;
        state.invalidate_polling_engine();
        assert_ne!(state.generation, old_generation);
        assert!(state.scene_resume.is_none());
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

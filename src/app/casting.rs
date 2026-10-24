//! External receiver ownership, delivery and background polling. This is separate from paired Nova.

// Deliberately no environment or user setting can enable this release.
const ENABLED: bool = false;

use super::*;
use nova_cast::{
    CastErrorKind, Device, MediaLoad, PlaybackState, ReceiverCommand, ReceiverSession,
    ReceiverSnapshot,
};
use nova_cast_media::{MediaGuard, SourceDescriptor};
use std::sync::Condvar;
use std::sync::mpsc;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct Source {
    descriptor: SourceDescriptor,
    url: String,
    headers: Vec<(String, String)>,
    subtitles: Vec<crate::player::SourceSubtitle>,
    request: crate::torrent::PlaybackRequest,
    title: String,
    _lease: Option<Arc<FileLease>>,
    torrent_lease: Option<crate::torrent::PlaybackLease>,
    selected_audio: Option<usize>,
    allow_unverified: bool,
    target: Option<PlaybackTarget>,
    intent: Option<playback::PlaybackIntent>,
    selected_text: Option<i64>,
    subtitle_warning: Option<&'static str>,
    observation_id: u64,
    timeline_offset: f64,
    duration: Option<f64>,
}
impl Source {
    fn identity(&self) -> Option<nova_playback_api::MediaIdentity> {
        let target = self.target.as_ref()?;
        Some(nova_playback_api::MediaIdentity {
            media_type: target
                .history
                .as_ref()
                .map(|h| h.type_.clone())
                .unwrap_or_default(),
            title_id: target.series_id.clone(),
            episode_id: target.episode_id.clone(),
            title: target
                .history
                .as_ref()
                .map(|h| h.title.clone())
                .unwrap_or_else(|| self.title.clone()),
            episode: target
                .history
                .as_ref()
                .map(|h| h.episode_label.clone())
                .unwrap_or_default(),
        })
    }
}
static FILE_LEASES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
struct FileLease {
    path: PathBuf,
}
impl FileLease {
    fn acquire(path: &str) -> Option<Arc<Self>> {
        let path = std::fs::canonicalize(path).ok()?;
        FILE_LEASES.lock().unwrap().push(path.clone());
        Some(Arc::new(Self { path }))
    }
}
impl Drop for FileLease {
    fn drop(&mut self) {
        let mut paths = FILE_LEASES.lock().unwrap();
        if let Some(index) = paths.iter().position(|p| p == &self.path) {
            paths.swap_remove(index);
        }
    }
}
pub(super) fn path_leased(path: &std::path::Path) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    FILE_LEASES
        .lock()
        .unwrap()
        .iter()
        .any(|p| p == &path || p.starts_with(&path))
}
#[derive(Default)]
pub(super) struct State {
    devices: Vec<Device>,
    generation: u64,
    discovery_generation: u64,
    discovery_cancel: Option<Arc<AtomicBool>>,
    cancel: Option<Arc<AtomicBool>>,
    attempt_cancel: Option<Arc<AtomicBool>>,
    worker: Option<Arc<Mailbox>>,
    worker_done: Option<Arc<(Mutex<bool>, Condvar)>>,
    pub(super) selected: bool,
    pub(super) active: bool,
    pub(super) loading: bool,
    pub(super) observation_id: u64,
    seekable: bool,
    timeline_offset: f64,
    recovery: Option<(Source, f64, bool, u64)>,
    source: Option<Source>,
    snapshot: Option<ReceiverSnapshot>,
    ui_snapshot: Option<(ReceiverSnapshot, u64, u64)>,
    ui_publish_pending: bool,
    pub(super) progress_dirty: bool,
    destination: Option<(crate::torrent::PlaybackRequest, u64)>,
    pending_source: Option<(Source, f64, bool)>,
}
struct WorkerDone(Arc<(Mutex<bool>, Condvar)>);
impl Drop for WorkerDone {
    fn drop(&mut self) {
        let (lock, wake) = &*self.0;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}
struct SetupHeartbeat(Arc<AtomicBool>);
impl SetupHeartbeat {
    fn start(
        name: String,
        lease: super::android_cast::CastServiceLease,
        lifetime: Arc<AtomicBool>,
        attempt: Arc<AtomicBool>,
        request: crate::torrent::PlaybackRequest,
    ) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let owner = profiles::capture();
        std::thread::spawn(move || {
            while !worker_done.load(Ordering::Acquire) {
                if lifetime.load(Ordering::Acquire)
                    || attempt.load(Ordering::Acquire)
                    || !request.is_current()
                    || !profiles::is_current(&owner)
                {
                    attempt.store(true, Ordering::Release);
                    break;
                }
                lease.update(&name, false);
                std::thread::park_timeout(Duration::from_secs(2));
            }
        });
        Self(done)
    }
}
impl Drop for SetupHeartbeat {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
enum Command {
    Load(Box<Source>, f64, bool),
    Replace(bool),
    Control(ReceiverCommand),
    Retry,
    Switch(Box<Device>),
    Stop(bool),
    Shutdown,
}
/// A bounded desired-state mailbox: slider scrubbing cannot queue stale requests ahead of Stop.
#[derive(Default)]
struct Mailbox {
    queue: Mutex<VecDeque<Command>>,
    wake: Condvar,
}
impl Mailbox {
    fn send(&self, command: Command) {
        let mut queue = self.queue.lock().unwrap();
        match &command {
            Command::Shutdown | Command::Stop(_) => {
                queue.clear();
                queue.push_front(command);
            }
            Command::Control(control) => {
                queue.retain(|queued| !matches!(queued,Command::Control(old) if std::mem::discriminant(old)==std::mem::discriminant(control)));
                if queue.len() < 16 {
                    queue.push_back(command);
                }
            }
            Command::Switch(..) => {
                queue.retain(|queued| !matches!(queued, Command::Switch(..)));
                if queue.len() < 16 {
                    queue.push_back(command);
                }
            }
            Command::Load(..) => {
                queue.retain(|queued| !matches!(queued, Command::Load(..)));
                if queue.len() < 16 {
                    queue.push_back(command);
                }
            }
            _ => {
                if queue.len() < 16 {
                    queue.push_back(command);
                }
            }
        }
        self.wake.notify_one();
    }
    fn receive(&self, timeout: Duration) -> Option<Command> {
        let mut queue = self.queue.lock().unwrap();
        if queue.is_empty() {
            queue = self.wake.wait_timeout(queue, timeout).unwrap().0;
        }
        queue.pop_front()
    }
}
impl State {
    pub(super) fn reserved(&self) -> bool {
        self.selected || self.active || self.loading || self.worker.is_some()
    }
    pub(super) fn clear_source(&mut self) {
        self.source = None;
        if !self.loading {
            self.pending_source = None;
        }
    }
    pub(super) fn capture_destination(&mut self, request: &crate::torrent::PlaybackRequest) {
        if self.loading
            && let Some(cancel) = &self.attempt_cancel
        {
            cancel.store(true, Ordering::Release);
        }
        self.destination = Some((request.clone(), self.generation));
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.attempt_cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(cancel) = &self.discovery_cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(worker) = &self.worker {
            worker.send(Command::Shutdown);
        }
    }
}
/// Linearizes each UI handoff against its worker deadline. Once a callback
/// claims a handoff, the worker must receive its result before deciding recovery.
struct HandoffTicket {
    state: AtomicUsize,
    deadline: Instant,
}
impl Default for HandoffTicket {
    fn default() -> Self {
        Self::new(Duration::from_secs(5))
    }
}
impl HandoffTicket {
    const PENDING: usize = 0;
    const COMMITTED: usize = 1;
    const CANCELLED: usize = 2;

    fn new(timeout: Duration) -> Self {
        Self {
            state: AtomicUsize::new(Self::PENDING),
            deadline: Instant::now() + timeout,
        }
    }

    fn commit(&self, current: bool) -> bool {
        if !current || Instant::now() >= self.deadline {
            self.cancel_pending();
            return false;
        }
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::COMMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// False means a UI callback already owns the handoff and its receipt.
    fn cancel_pending(&self) -> bool {
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            || self.state.load(Ordering::Acquire) == Self::CANCELLED
    }

    fn receive<T>(
        &self,
        receiver: &mpsc::Receiver<T>,
        timeout: Duration,
    ) -> Result<T, &'static str> {
        match receiver.recv_timeout(timeout) {
            Ok(value) => Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) if !self.cancel_pending() => {
                // The callback won before the deadline. Do not roll back its
                // side effects or tear down its receiver because delivery was
                // delayed; consume the already-running callback's receipt.
                // Existing synchronous libmpv calls cannot be preempted. While
                // this waits, attempt cancellation still stops relay delivery
                // and its foreground-service heartbeat independently.
                receiver.recv().map_err(|_| "Connection cancelled")
            }
            Err(_) => {
                self.cancel_pending();
                Err("Connection cancelled")
            }
        }
    }
}

fn post(bridge: &Bridge, generation: u64, f: impl FnOnce(Bridge, AppWindow) + Send + 'static) {
    let bridge = bridge.clone();
    let _ = profiles::invoke_from_event_loop(move || {
        if bridge.shared.lock().unwrap().casting.generation != generation {
            return;
        }
        if let Some(app) = bridge.app() {
            f(bridge, app);
        }
    });
}
fn source_snapshot(mut snapshot: ReceiverSnapshot, source: Option<&Source>) -> ReceiverSnapshot {
    if let Some(source) = source {
        snapshot.position = (snapshot.position + source.timeline_offset).max(0.);
        snapshot.duration = source.duration.or_else(|| {
            snapshot
                .duration
                .map(|duration| duration + source.timeline_offset)
        });
    }
    snapshot
}
fn same_origin(source: &str, target: &str) -> bool {
    url::Url::parse(source)
        .ok()
        .zip(url::Url::parse(target).ok())
        .is_some_and(|(source, target)| source.origin() == target.origin())
}
// Reject nonfinite or stale observations before they can authorize unloading mpv.
fn transfer_position_confirmed(snapshot: &ReceiverSnapshot, position: f64, offset: f64) -> bool {
    position.is_finite()
        && offset.is_finite()
        && snapshot.position.is_finite()
        && (snapshot.position + offset - position).abs() <= 3.
}

fn transfer_ready(snapshot: &ReceiverSnapshot) -> bool {
    snapshot.owned
        && snapshot.end_reason.is_none()
        && matches!(
            snapshot.state,
            PlaybackState::Playing | PlaybackState::Paused
        )
}
fn media_error_text(error: nova_cast_media::MediaError) -> &'static str {
    use nova_cast_media::MediaError;
    match error {
        MediaError::Cancelled => "Connection cancelled",
        MediaError::Network | MediaError::SourceUnavailable => {
            "Media source is unavailable. Refresh sources and try again."
        }
        MediaError::Bind => "Cannot serve media on this network. Check VPN and firewall settings.",
        MediaError::ResourceLimit => {
            "Casting reached its buffer limit. Increase Casting remux buffer in Player settings, choose another source or play here."
        }
        MediaError::UnverifiedFormat => {
            "Receiver format support is unknown. Try this stream, choose another source or play here."
        }
        MediaError::InvalidSource => "This source cannot be shared safely. Choose another source.",
        MediaError::UnsupportedCodec => {
            "This receiver does not support the source codecs. Choose another source or play here."
        }
        MediaError::UnsupportedContainer => {
            "This receiver cannot accept this streaming format. Choose another source or play here."
        }
        MediaError::UnsupportedHls => {
            "This HLS source is unsupported. Choose another source or play locally."
        }
        MediaError::InvalidTimestamps | MediaError::Native => {
            "Media could not be prepared safely. Choose another source or play locally."
        }
    }
}
fn error_text(kind: CastErrorKind) -> &'static str {
    match kind {
        CastErrorKind::Cancelled => "Connection cancelled",
        CastErrorKind::Timeout => {
            "Receiver did not respond. Check its power and Wi-Fi, then try again."
        }
        CastErrorKind::Network => "Cannot reach receiver. Check Wi-Fi, VPN and firewall settings.",
        CastErrorKind::Protocol => "Receiver returned an invalid response. Try reconnecting.",
        CastErrorKind::Unsupported => "This receiver does not support this operation.",
        CastErrorKind::Takeover => "Playback changed on another device. Reconnect to continue.",
        CastErrorKind::ReplacementRequired => "Receiver is already in use.",
        CastErrorKind::InvalidInput => "Enter a valid receiver IP address.",
    }
}
impl Bridge {
    fn finish_cast_source(&self, source: &Source) {
        if let Some(target) = &source.target {
            self.list_services_playback_observation(
                target,
                target.last_pos,
                target.last_dur,
                false,
                true,
                source.observation_id,
            );
        }
        let progress = self.shared.lock().unwrap().progress.clone();
        playback::write_progress_map(&progress);
        nova_sync::flush_local_sync();
        self.finish_viewing_journal();
    }
    pub(super) fn install_casting(&self) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        ui.set_available(ENABLED);
        // Retained for future work; casting is disabled in shipped builds.
        if !ENABLED {
            return;
        }
        let b = self.clone();
        ui.on_show(move || b.cast_show());
        let b = self.clone();
        ui.on_refresh(move || b.cast_discover(None));
        let b = self.clone();
        ui.on_manual(move |address| b.cast_discover(Some(address.to_string())));
        let b = self.clone();
        ui.on_select(move |id| b.cast_select(id.as_str()));
        let b = self.clone();
        ui.on_cancel(move || b.cast_cancel());
        let b = self.clone();
        ui.on_replace(move |yes| {
            b.cast_send(Command::Replace(yes));
        });
        let b = self.clone();
        ui.on_action(move |name, value| b.cast_action(name.as_str(), value));
    }
    fn cast_show(&self) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        ui.set_open(true);
        if self.party_active() || self.remote_receiver_playback() || app.get_remote_controls_open()
        {
            ui.set_error(
                text::tr("Leave the watch party or paired Nova session before casting.").into(),
            );
            return;
        }
        if nova_sync::read_settings().enable_local_discovery
            && self.shared.lock().unwrap().casting.devices.is_empty()
        {
            self.cast_discover(None);
        }
    }
    fn cast_send(&self, command: Command) {
        if let Some(worker) = &self.shared.lock().unwrap().casting.worker {
            worker.send(command);
        }
    }
    fn cast_discover(&self, manual: Option<String>) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        ui.set_error("".into());
        ui.set_searching(true);
        ui.set_status(text::tr("Searching for receivers…").into());
        let (generation, cancel) = {
            let mut state = self.shared.lock().unwrap();
            let s = &mut state.casting;
            if let Some(cancel) = s.discovery_cancel.take() {
                cancel.store(true, Ordering::Release);
            }
            s.discovery_generation += 1;
            let cancel = Arc::new(AtomicBool::new(false));
            s.discovery_cancel = Some(cancel.clone());
            (s.discovery_generation, cancel)
        };
        let bridge = self.clone();
        thread::spawn(move || {
            let result = (|| {
                let _lease = super::android_cast::discovery_lock()
                    .map_err(|_| error_text(CastErrorKind::Network))?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| error_text(CastErrorKind::Network))?;
                runtime
                    .block_on(async {
                        match manual {
                            Some(address) => {
                                nova_cast::discover_manual(&address, cancel.clone()).await
                            }
                            None => {
                                nova_cast::discover(Duration::from_secs(5), cancel.clone()).await
                            }
                        }
                    })
                    .map_err(|e| error_text(e.kind))
            })();
            let _ = profiles::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                let ui = app.global::<crate::CastBridge>();
                let mut state = bridge.shared.lock().unwrap();
                if state.casting.discovery_generation != generation
                    || cancel.load(Ordering::Acquire)
                {
                    return;
                }
                ui.set_searching(false);
                ui.set_status("".into());
                match result {
                    Ok(mut devices) => {
                        devices.sort_by(|a, b| {
                            a.name
                                .to_lowercase()
                                .cmp(&b.name.to_lowercase())
                                .then(a.id.cmp(&b.id))
                        });
                        devices.dedup_by(|a, b| a.id == b.id);
                        ui.set_devices(
                            Rc::new(VecModel::from(
                                devices
                                    .iter()
                                    .map(|d| crate::CastDeviceRow {
                                        id: d.id.clone().into(),
                                        name: d.name.clone().into(),
                                        protocol: match d.protocol {
                                            nova_cast::Protocol::GoogleCast => "Google Cast",
                                            nova_cast::Protocol::Dlna => "DLNA",
                                        }
                                        .into(),
                                        available: d.video,
                                    })
                                    .collect::<Vec<_>>(),
                            ))
                            .into(),
                        );
                        state.casting.devices = devices;
                    }
                    Err(message) => ui.set_error(text::tr(message).into()),
                }
            });
        });
    }
    fn cast_cancel(&self) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(cancel) = state.casting.discovery_cancel.take() {
                cancel.store(true, Ordering::Release);
            }
            state.casting.discovery_generation += 1;
        }
        ui.set_searching(false);
        if ui.get_connecting() {
            // Revoking the generation also invalidates preparation, pause handshakes and late replies.
            self.cast_disconnect();
        }
        ui.set_status(text::tr("Connection cancelled").into());
    }
    pub(super) fn cast_shutdown(&self) {
        let mut state = self.shared.lock().unwrap();
        let casting = &mut state.casting;
        if let Some(cancel) = &casting.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(cancel) = &casting.attempt_cancel {
            cancel.store(true, Ordering::Release);
        }
        casting.source = None;
        casting.pending_source = None;
        casting.recovery = None;
        if let Some(worker) = casting.worker.take() {
            worker.send(Command::Shutdown);
        }
    }
    pub(super) fn cast_wait_shutdown(&self) {
        let done = self.shared.lock().unwrap().casting.worker_done.clone();
        if let Some(done) = done {
            let (lock, wake) = &*done;
            let _ =
                wake.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(20), |done| {
                    !*done
                });
        }
    }
    fn cast_disconnect(&self) {
        let mut state = self.shared.lock().unwrap();
        let s = &mut state.casting;
        s.generation += 1;
        if let Some(worker) = s.worker.take() {
            worker.send(Command::Shutdown);
        }
        if let Some(cancel) = s.attempt_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        if let Some(cancel) = s.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        s.selected = false;
        s.loading = false;
        s.pending_source = None;
        s.recovery = None;
        s.ui_snapshot = None;
        s.ui_publish_pending = false;
        drop(state);
        if let Some(app) = self.app() {
            let ui = app.global::<crate::CastBridge>();
            ui.set_selected(false);
            ui.set_connecting(false);
            ui.set_target_id("".into());
            ui.set_target_name("".into());
            ui.set_replacement(false);
        }
    }
    fn cast_select(&self, id: &str) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        if self.party_active() || self.remote_receiver_playback() {
            ui.set_error(
                text::tr("Leave the watch party or paired Nova session before casting.").into(),
            );
            return;
        }
        if self.shared.lock().unwrap().casting.active {
            if ui.get_target_id() == id {
                return;
            }
            let device = self
                .shared
                .lock()
                .unwrap()
                .casting
                .devices
                .iter()
                .find(|device| device.id == id && device.video)
                .cloned();
            if let Some(device) = device {
                ui.set_connecting(true);
                ui.set_error("".into());
                ui.set_status(text::tr("Changing receiver…").into());
                self.cast_send(Command::Switch(Box::new(device)));
            }
            return;
        }
        if id.is_empty() {
            self.cast_disconnect();
            ui.set_open(false);
            return;
        }
        let device = self
            .shared
            .lock()
            .unwrap()
            .casting
            .devices
            .iter()
            .find(|d| d.id == id && d.video)
            .cloned();
        let Some(device) = device else { return };
        self.cast_disconnect();
        let receiver = Arc::new(Mailbox::default());
        let sender = receiver.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::new((Mutex::new(false), Condvar::new()));
        let generation = {
            let mut state = self.shared.lock().unwrap();
            let s = &mut state.casting;
            s.worker_done = Some(worker_done.clone());
            s.worker = Some(sender);
            s.loading = true;
            s.cancel = Some(cancel.clone());
            s.generation
        };
        ui.set_target_id(device.id.clone().into());
        ui.set_target_name(device.name.clone().into());
        ui.set_connecting(true);
        ui.set_error("".into());
        ui.set_status(text::tr("Connecting to receiver…").into());
        let bridge = self.clone();
        thread::spawn(move || {
            let _done = WorkerDone(worker_done);
            run_worker(bridge, generation, device, cancel, receiver)
        });
    }
    /// Called only after the existing resolver, policy and resume guards have accepted a source.
    pub(super) fn cast_source(
        &self,
        request: &crate::torrent::PlaybackRequest,
        url: &str,
        headers: &[(String, String)],
        subtitles: &[crate::player::SourceSubtitle],
        position: f64,
    ) -> bool {
        if !ENABLED {
            return false;
        }
        let descriptor = if std::path::Path::new(url).is_file() {
            let path = PathBuf::from(url);
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("m3u8"))
            {
                SourceDescriptor::LocalHlsBundle {
                    root: path
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .to_path_buf(),
                    playlist: path,
                }
            } else {
                SourceDescriptor::LocalFile { path }
            }
        } else if url.starts_with("http://127.0.0.1:") || url.starts_with("http://[::1]:") {
            SourceDescriptor::LoopbackTorrent { url: url.into() }
        } else {
            SourceDescriptor::Http {
                url: url.into(),
                headers: headers.to_vec(),
                private: true,
            }
        };
        // Struct-field temporaries live to the end of the initializer. Capture
        // both fields under one short lock instead of locking Shared twice.
        let (target, intent) = {
            let state = self.shared.lock().unwrap();
            (state.playback.clone(), state.playback_intent.clone())
        };
        let source = Source {
            descriptor: descriptor.clone(),
            url: url.into(),
            headers: headers.to_vec(),
            subtitles: subtitles.to_vec(),
            request: request.clone(),
            title: self
                .app()
                .map(|a| a.get_player_title().to_string())
                .unwrap_or_default(),
            _lease: FileLease::acquire(url),
            torrent_lease: if matches!(descriptor, SourceDescriptor::LoopbackTorrent { .. }) {
                crate::torrent::engine().and_then(|engine| engine.lease_playback(request, url).ok())
            } else {
                None
            },
            selected_audio: None,
            allow_unverified: false,
            target,
            intent,
            selected_text: None,
            subtitle_warning: None,
            observation_id: 0,
            timeline_offset: 0.,
            duration: None,
        };
        let mut state = self.shared.lock().unwrap();
        let s = &mut state.casting;
        s.source = Some(source.clone());
        if !s.selected {
            return false;
        }
        if s.destination.as_ref().is_some_and(|(token, generation)| {
            token.same_request(request) && *generation != s.generation
        }) {
            drop(state);
            if let Some(app) = self.app() {
                app.global::<crate::CastBridge>()
                    .set_error(text::tr("Receiver changed. Choose the source again.").into());
            }
            return true;
        }
        s.loading = true;
        if let Some(worker) = &s.worker {
            worker.send(Command::Load(Box::new(source), position, false));
        }
        drop(state);
        if let Some(app) = self.app() {
            let ui = app.global::<crate::CastBridge>();
            ui.set_open(true);
            ui.set_connecting(true);
            ui.set_error("".into());
            ui.set_artwork(app.get_player_poster());
            ui.set_status(text::tr("Preparing media for receiver…").into());
        }
        true
    }
    pub(super) fn cast_dispatch_action(
        &self,
        action: &nova_playback_api::PlaybackAction,
    ) -> Option<Result<(), String>> {
        use nova_playback_api::PlaybackAction;
        if !ENABLED {
            return None;
        }
        let state = self.shared.lock().unwrap();
        if !state.casting.active {
            return None;
        }
        let snapshot = state.casting.snapshot.clone();
        drop(state);
        let snapshot = snapshot.unwrap_or_default();
        let result = match action {
            PlaybackAction::TogglePause => {
                self.cast_action(
                    "pause",
                    if snapshot.state == PlaybackState::Paused {
                        0.
                    } else {
                        1.
                    },
                );
                Ok(())
            }
            PlaybackAction::SetPaused(paused) => {
                self.cast_action("pause", if *paused { 1. } else { 0. });
                Ok(())
            }
            PlaybackAction::SeekAbsolute(position) if position.is_finite() => {
                self.cast_action("seek", *position as f32);
                Ok(())
            }
            PlaybackAction::SeekRelative(delta) if delta.is_finite() => {
                self.cast_action("seek", (snapshot.position + delta).max(0.) as f32);
                Ok(())
            }
            PlaybackAction::SetVolume(volume) if volume.is_finite() => {
                self.cast_action("volume", (*volume / 100.) as f32);
                Ok(())
            }
            PlaybackAction::VolumeStep(delta) if delta.is_finite() => {
                self.cast_action(
                    "volume",
                    snapshot.volume.unwrap_or(0.) + (*delta / 100.) as f32,
                );
                Ok(())
            }
            PlaybackAction::SetMuted(muted) => {
                self.cast_action("mute", if *muted { 1. } else { 0. });
                Ok(())
            }
            PlaybackAction::ToggleMute => {
                self.cast_action(
                    "mute",
                    if snapshot.muted.unwrap_or(false) {
                        0.
                    } else {
                        1.
                    },
                );
                Ok(())
            }
            PlaybackAction::Stop => {
                self.cast_action("stop", 0.);
                Ok(())
            }
            PlaybackAction::SetSleepTimer(mode) => self.set_sleep_timer(mode.clone()),
            PlaybackAction::ExtendSleepTimer => self.extend_sleep_timer(),
            PlaybackAction::CancelSleepTimer => {
                self.cancel_sleep_timer();
                Ok(())
            }
            _ => Err(text::tr("This action is unavailable while casting.").into()),
        };
        Some(result)
    }
    fn cast_action(&self, action: &str, value: f32) {
        if !value.is_finite() {
            return;
        }
        match action {
            "pause" => self.cast_send(Command::Control(ReceiverCommand::SetPaused(value != 0.))),
            "seek" if !self.shared.lock().unwrap().casting.seekable => {}
            "seek" => {
                let offset = self.shared.lock().unwrap().casting.timeline_offset;
                self.cast_send(Command::Control(ReceiverCommand::Seek(
                    (f64::from(value) - offset).max(0.),
                )));
            }
            "volume" => self.cast_send(Command::Control(ReceiverCommand::SetVolume(
                value.clamp(0., 1.),
            ))),
            "text" => self.cast_send(Command::Control(ReceiverCommand::SetTextTrack(
                (value > 0.).then_some(value as i64),
            ))),
            "mute" => self.cast_send(Command::Control(ReceiverCommand::SetMuted(value != 0.))),
            "local_anyway" => {
                let recovery = self.shared.lock().unwrap().casting.recovery.take();
                if let Some((mut source, position, paused, native)) = recovery {
                    if let Some(intent) = source.intent.clone() {
                        source.request = self.restore_cast_playback(intent, source.target.clone());
                    }
                    {
                        let mut state = self.shared.lock().unwrap();
                        state.casting.source = Some(source.clone());
                        state.playback = source.target.clone();
                    }
                    if self.player.screenshot_session() == native
                        && self.app().is_some_and(|a| a.get_player_open())
                    {
                        self.player.set_paused(paused);
                    } else {
                        let preferences = self.prepare_playback_preferences();
                        if self
                            .player
                            .play_with_source_subtitles_and_load_policy(
                                &source.url,
                                position,
                                &source.headers,
                                &source.subtitles,
                                preferences,
                                crate::player::PlaybackLoadPolicy {
                                    allow_external: false,
                                    paused: false,
                                },
                            )
                            .is_ok()
                        {
                            self.player.set_paused(paused);
                            self.mark_playback_opened(true);
                        }
                    }
                    self.cast_disconnect();
                    if let Some(app) = self.app() {
                        let ui = app.global::<crate::CastBridge>();
                        ui.set_uncertain(false);
                        ui.set_open(false);
                    }
                }
            }
            "unverified" => {
                let pending = self.shared.lock().unwrap().casting.pending_source.clone();
                if let Some((mut source, position, paused)) = pending {
                    source.allow_unverified = true;
                    self.shared.lock().unwrap().casting.loading = true;
                    if let Some(app) = self.app() {
                        let ui = app.global::<crate::CastBridge>();
                        ui.set_unverified(false);
                        ui.set_connecting(true);
                        ui.set_error("".into());
                    }
                    self.cast_send(Command::Load(Box::new(source), position, paused));
                }
            }
            "retry" => self.cast_send(Command::Retry),
            "stop" => self.cast_send(Command::Stop(false)),
            "here" => self.cast_send(Command::Stop(true)),
            _ => {}
        }
    }
    fn cast_publish_latest(
        &self,
        generation: u64,
        snapshot: ReceiverSnapshot,
        load_id: u64,
        target_session: u64,
    ) {
        let mut state = self.shared.lock().unwrap();
        if state.casting.generation != generation {
            return;
        }
        state.casting.ui_snapshot = Some((snapshot, load_id, target_session));
        if state.casting.ui_publish_pending {
            return;
        }
        state.casting.ui_publish_pending = true;
        drop(state);
        post(self, generation, |b, _| {
            let (snapshot, dirty) = {
                let mut state = b.shared.lock().unwrap();
                state.casting.ui_publish_pending = false;
                (
                    state.casting.ui_snapshot.take(),
                    std::mem::take(&mut state.casting.progress_dirty),
                )
            };
            if let Some((snapshot, load_id, target_session)) = snapshot {
                let same_target = b
                    .shared
                    .lock()
                    .unwrap()
                    .playback
                    .as_ref()
                    .is_some_and(|target| target.session == target_session);
                if transfer_ready(&snapshot) && same_target {
                    b.observe_queue_playback(load_id, false);
                }
                b.cast_publish(snapshot);
            }
            if dirty {
                b.request_progress_projection();
            }
        });
    }
    fn cast_publish(&self, snapshot: ReceiverSnapshot) {
        let Some(app) = self.app() else { return };
        let ui = app.global::<crate::CastBridge>();
        if ui.get_stale() {
            ui.set_error("".into());
        }
        ui.set_stale(false);
        ui.set_paused(snapshot.state == PlaybackState::Paused);
        ui.set_position(snapshot.position as f32);
        ui.set_time_text(
            format!(
                "{} / {}",
                crate::player::format_time(snapshot.position),
                crate::player::format_time(snapshot.duration.unwrap_or(0.))
            )
            .into(),
        );
        ui.set_duration(snapshot.duration.unwrap_or(0.) as f32);
        ui.set_volume(snapshot.volume.unwrap_or(0.));
        ui.set_muted(snapshot.muted.unwrap_or(false));
        ui.set_can_pause(snapshot.owned && snapshot.capabilities.pause);
        ui.set_can_seek(
            snapshot.owned
                && snapshot.capabilities.seek
                && self.shared.lock().unwrap().casting.seekable,
        );
        ui.set_can_volume(snapshot.owned && snapshot.capabilities.volume);
        ui.set_can_mute(snapshot.owned && snapshot.capabilities.mute);
        ui.set_can_text_tracks(snapshot.owned && snapshot.capabilities.text_tracks);
        ui.set_status(
            text::tr(match snapshot.state {
                PlaybackState::Paused => "Paused on receiver",
                PlaybackState::Playing => "Playing on receiver",
                PlaybackState::Buffering => "Buffering on receiver",
                _ => "Connected to receiver",
            })
            .into(),
        );

        self.shared.lock().unwrap().casting.snapshot = Some(snapshot);
    }
}

fn run_worker(
    bridge: Bridge,
    generation: u64,
    mut device: Device,
    cancel: Arc<AtomicBool>,
    receiver: Arc<Mailbox>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return;
    };
    let mut session =
        match runtime.block_on(ReceiverSession::connect(device.clone(), cancel.clone())) {
            Ok(s) => s,
            Err(e) => {
                post(&bridge, generation, move |b, a| {
                    {
                        let mut state = b.shared.lock().unwrap();
                        state.casting.loading = false;
                        state.casting.worker = None;
                        state.casting.cancel = None;
                    }
                    let ui = a.global::<crate::CastBridge>();
                    ui.set_connecting(false);
                    ui.set_error(text::tr(error_text(e.kind)).into());
                });
                return;
            }
        };
    post(&bridge, generation, |b, a| {
        {
            let mut state = b.shared.lock().unwrap();
            state.casting.selected = true;
            state.casting.loading = false;
        }
        let ui = a.global::<crate::CastBridge>();
        ui.set_selected(true);
        ui.set_connecting(false);
        ui.set_status(text::tr("Connected. Choose something to watch.").into());
        if a.get_player_open() && a.get_playback_started() {
            let source = b.shared.lock().unwrap().casting.source.clone();
            if let Some(mut source) = source {
                source.selected_audio = b.player.selected_audio_stream_index();
                if let Some((external, url)) = b.player.selected_subtitle_source() {
                    if external && !url.is_empty() {
                        let index = source
                            .subtitles
                            .iter()
                            .position(|s| s.url == url)
                            .unwrap_or_else(|| {
                                source.subtitles.push(url.into());
                                source.subtitles.len() - 1
                            });
                        source.selected_text = Some(index as i64 + 1);
                    } else {
                        source.subtitle_warning = Some(
                            "Selected embedded subtitles cannot be cast. Use external text subtitles or play locally.",
                        );
                    }
                }
                b.shared.lock().unwrap().casting.loading = true;
                ui.set_connecting(true);
                ui.set_artwork(a.get_player_poster());
                b.cast_send(Command::Load(
                    Box::new(source),
                    f64::from(a.get_position()),
                    a.get_is_paused(),
                ));
            } else {
                ui.set_error(text::tr("Choose a source again to transfer this playback.").into());
            }
        }
    });
    // This loop owns transport, HTTP delivery and the Android foreground service independently of Slint timers.
    let mut media: Option<MediaGuard> = None;
    let mut service: Option<super::android_cast::CastServiceLease> = None;
    let mut pending: Option<(Source, f64, bool)> = None;
    let mut active_source: Option<Source> = None;
    let mut last_poll = Instant::now();
    let mut failures = 0u8;
    let mut last_snapshot = ReceiverSnapshot::default();
    let mut load_id = generation.wrapping_mul(1_000_000);
    let mut commands = VecDeque::new();
    let mut poll_enabled = true;
    let mut idle_observations = 0u8;
    loop {
        if cancel.load(Ordering::Acquire) || !profiles::is_current(&profiles::capture()) {
            break;
        }
        if let Some(command) = receiver.receive(Duration::from_millis(200)) {
            commands.push_back(command);
        }
        for action in service
            .as_ref()
            .map_or_else(Vec::new, |lease| lease.take_commands())
        {
            match action {
                super::android_cast::CastServiceCommand::Play => {
                    commands.push_back(Command::Control(ReceiverCommand::SetPaused(false)))
                }
                super::android_cast::CastServiceCommand::Pause => {
                    commands.push_back(Command::Control(ReceiverCommand::SetPaused(true)))
                }
                super::android_cast::CastServiceCommand::Stop
                | super::android_cast::CastServiceCommand::ServiceStopped => {
                    commands.clear();
                    commands.push_front(Command::Stop(false));
                }
                super::android_cast::CastServiceCommand::ShowControls => {
                    post(&bridge, generation, |_, a| {
                        a.global::<crate::CastBridge>().set_open(true)
                    })
                }
                super::android_cast::CastServiceCommand::NetworkChanged => {
                    commands.push_back(Command::Retry)
                }
            }
        }
        if commands
            .iter()
            .any(|command| matches!(command, Command::Shutdown | Command::Stop(_)))
        {
            let index = commands
                .iter()
                .position(|command| matches!(command, Command::Shutdown | Command::Stop(_)))
                .unwrap();
            let urgent = commands.remove(index).unwrap();
            commands.clear();
            commands.push_front(urgent);
        }
        let mut command = commands.pop_front();
        let timer_stop = media.is_some() && bridge.cast_sleep_tick(None);
        if timer_stop {
            command = Some(Command::Stop(false));
        }
        match command {
            Some(Command::Shutdown) => break,
            Some(Command::Switch(next_device)) => {
                let Some(mut source) = active_source.clone() else {
                    continue;
                };
                match runtime.block_on(session.snapshot()) {
                    Ok(snapshot) if snapshot.owned => {
                        let snapshot = source_snapshot(snapshot, Some(&source));
                        last_snapshot = snapshot.clone();
                        if let Some(target) = source.target.as_mut() {
                            bridge.note_cast_progress(
                                target,
                                snapshot.position,
                                snapshot.duration.unwrap_or(0.),
                                snapshot.state != PlaybackState::Playing,
                            );
                        }
                    }
                    _ => {
                        post(&bridge, generation, |_, a| {
                            let ui = a.global::<crate::CastBridge>();
                            ui.set_connecting(false);
                            ui.set_error(text::tr("Could not confirm receiver ownership. Retry the connection before changing device.").into());
                        });
                        continue;
                    }
                }
                if let Err(error) = runtime.block_on(session.stop_owned_media()) {
                    post(&bridge, generation, move |_, a| {
                        let ui = a.global::<crate::CastBridge>();
                        ui.set_connecting(false);
                        ui.set_error(text::tr(error_text(error.kind)).into());
                    });
                    continue;
                }
                bridge.finish_cast_source(&source);
                media = None;
                active_source = None;
                if let Some(service) = service.take() {
                    service.stop();
                }
                runtime.block_on(session.close_transport());
                let position = last_snapshot.position;
                let paused = last_snapshot.state == PlaybackState::Paused;
                let connected = runtime.block_on(ReceiverSession::connect(
                    (*next_device).clone(),
                    cancel.clone(),
                ));
                match connected {
                    Ok(next_session) => {
                        session = next_session;
                        device = *next_device;
                        let name = device.name.clone();
                        let id = device.id.clone();
                        post(&bridge, generation, move |b, a| {
                            let mut state = b.shared.lock().unwrap();
                            state.casting.loading = true;
                            drop(state);
                            let ui = a.global::<crate::CastBridge>();
                            ui.set_active(false);
                            ui.set_target_name(name.into());
                            ui.set_target_id(id.into());
                            ui.set_status(text::tr("Preparing media for receiver…").into());
                        });
                        if session.needs_replacement() {
                            pending = Some((source, position, paused));
                            post(&bridge, generation, |_, a| {
                                let ui = a.global::<crate::CastBridge>();
                                ui.set_replacement(true);
                                ui.set_connecting(false);
                            });
                        } else {
                            commands.push_front(Command::Load(Box::new(source), position, paused));
                        }
                    }
                    Err(error) => {
                        post(&bridge, generation, move |b, a| {
                            let native = b.player.screenshot_session();
                            let mut state = b.shared.lock().unwrap();
                            state.casting.active = false;
                            state.casting.loading = false;
                            state.casting.selected = false;
                            state.casting.recovery = Some((source, position, paused, native));
                            drop(state);
                            let ui = a.global::<crate::CastBridge>();
                            ui.set_active(false);
                            ui.set_selected(false);
                            ui.set_connecting(false);
                            ui.set_recovery_available(true);
                            ui.set_error(text::tr(error_text(error.kind)).into());
                        });
                    }
                }
            }
            Some(Command::Retry) => match runtime.block_on(session.reconnect()) {
                Ok(snapshot) => {
                    if let Some(media) = &media {
                        media.set_playhead(snapshot.position);
                    }
                    let snapshot = source_snapshot(snapshot, active_source.as_ref());
                    poll_enabled = true;
                    failures = 0;
                    last_poll = Instant::now() - Duration::from_secs(2);
                    post(&bridge, generation, move |b, _| b.cast_publish(snapshot));
                }
                Err(error) => post(&bridge, generation, move |_, a| {
                    a.global::<crate::CastBridge>()
                        .set_error(text::tr(error_text(error.kind)).into())
                }),
            },
            Some(Command::Replace(yes)) => {
                post(&bridge, generation, |_, a| {
                    a.global::<crate::CastBridge>().set_replacement(false)
                });
                if yes {
                    session.authorize_replacement();
                    if let Some((s, p, paused)) = pending.take() {
                        let _ = load(
                            &bridge,
                            generation,
                            &runtime,
                            &mut session,
                            &device,
                            &cancel,
                            s,
                            p,
                            paused,
                            &mut media,
                            &mut active_source,
                            &mut service,
                        );
                    }
                } else {
                    pending = None;
                    post(&bridge, generation, |b, a| {
                        b.shared.lock().unwrap().casting.loading = false;
                        a.global::<crate::CastBridge>().set_connecting(false);
                    });
                }
            }
            Some(Command::Load(source, position, paused)) => {
                let source = *source;
                bridge.shared.lock().unwrap().casting.pending_source =
                    Some((source.clone(), position, paused));
                post(&bridge, generation, |_, a| {
                    a.global::<crate::CastBridge>().set_unverified(false)
                });
                if !source.request.is_current() {
                    continue;
                }
                if session.needs_replacement() && media.is_none() {
                    pending = Some((source, position, paused));
                    post(&bridge, generation, |_, a| {
                        let ui = a.global::<crate::CastBridge>();
                        ui.set_replacement(true);
                        ui.set_connecting(false);
                    });
                    continue;
                }
                if load(
                    &bridge,
                    generation,
                    &runtime,
                    &mut session,
                    &device,
                    &cancel,
                    source,
                    position,
                    paused,
                    &mut media,
                    &mut active_source,
                    &mut service,
                ) {
                    load_id = load_id.wrapping_add(1);
                    last_poll = Instant::now() - Duration::from_secs(2);
                }
            }
            Some(Command::Control(command)) => match runtime.block_on(session.command(command)) {
                Ok(snapshot) => {
                    if let Some(media) = &media {
                        media.set_playhead(snapshot.position);
                    }
                    let snapshot = source_snapshot(snapshot, active_source.as_ref());
                    last_snapshot = snapshot.clone();
                    if let Some(service) = &service {
                        service.update(&device.name, snapshot.state == PlaybackState::Paused);
                        service.set_can_pause(snapshot.owned && snapshot.capabilities.pause);
                    }
                    post(&bridge, generation, move |b, a| {
                        b.cast_publish(snapshot);
                        if let ReceiverCommand::SetTextTrack(track) = command {
                            a.global::<crate::CastBridge>()
                                .set_selected_text_track(track.unwrap_or(0) as i32);
                        }
                    });
                }
                Err(e) => post(&bridge, generation, move |_, a| {
                    a.global::<crate::CastBridge>()
                        .set_error(text::tr(error_text(e.kind)).into())
                }),
            },
            Some(Command::Stop(return_here)) => {
                if let Ok(snapshot) = runtime.block_on(session.snapshot()) {
                    if let Some(media) = &media {
                        media.set_playhead(snapshot.position);
                    }
                    let snapshot = source_snapshot(snapshot, active_source.as_ref());
                    if matches!(
                        snapshot.state,
                        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                    ) {
                        last_snapshot = snapshot;
                    }
                }
                match runtime.block_on(session.stop_owned_media()) {
                    Ok(()) => {
                        media = None;
                        pending = None;
                        if let Some(service) = service.take() {
                            service.stop();
                        }
                        let source = active_source.take();
                        if let Some(source) = &source {
                            bridge.finish_cast_source(source);
                        }
                        let position = last_snapshot.position;
                        let paused = last_snapshot.state == PlaybackState::Paused;
                        post(&bridge, generation, move |b, a| {
                            {
                                let mut state = b.shared.lock().unwrap();
                                state.casting.active = false;
                                state.casting.loading = false;
                                state.casting.pending_source = None;
                                state.casting.recovery = None;
                                state.casting.source = None;
                            }
                            let ui = a.global::<crate::CastBridge>();
                            ui.set_active(false);
                            ui.set_connecting(false);
                            ui.set_uncertain(false);
                            ui.set_status(text::tr("Casting stopped").into());
                            if return_here {
                                if let Some(mut source) = source {
                                    if let Some(intent) = source.intent.clone() {
                                        source.request =
                                            b.restore_cast_playback(intent, source.target.clone());
                                    }
                                    b.shared.lock().unwrap().casting.source = Some(source.clone());
                                    let preferences = b.prepare_playback_preferences();
                                    if b.player
                                        .play_with_source_subtitles_and_load_policy(
                                            &source.url,
                                            position,
                                            &source.headers,
                                            &source.subtitles,
                                            preferences,
                                            crate::player::PlaybackLoadPolicy {
                                                allow_external: false,
                                                paused: false,
                                            },
                                        )
                                        .is_ok()
                                    {
                                        b.player.set_paused(paused);
                                        b.mark_playback_opened(true);
                                        b.cast_disconnect();
                                        ui.set_open(false);
                                    }
                                }
                            } else {
                                b.finish_playback_request();
                            }
                        });
                    }
                    Err(e) => {
                        if timer_stop {
                            media = None;
                            let source = active_source.take();
                            if let Some(source) = &source {
                                bridge.finish_cast_source(source);
                            }
                            if let Some(service) = service.take() {
                                service.stop();
                            }
                            post(&bridge, generation, move |b, a| {
                                let native = b.player.screenshot_session();
                                let mut state = b.shared.lock().unwrap();
                                state.casting.active = false;
                                state.casting.loading = false;
                                state.casting.source = None;
                                state.casting.pending_source = None;
                                if let Some(source) = source {
                                    state.casting.recovery =
                                        Some((source, last_snapshot.position, true, native));
                                }
                                drop(state);
                                let ui = a.global::<crate::CastBridge>();
                                ui.set_active(false);
                                ui.set_uncertain(true);
                                ui.set_connecting(false);
                                ui.set_error(text::tr("Sleep timer finished. Receiver stop could not be confirmed.").into());
                            });
                        } else {
                            post(&bridge, generation, move |_, a| {
                                a.global::<crate::CastBridge>().set_error(text::tr(if e.kind==CastErrorKind::Takeover{error_text(e.kind)}else{"Could not confirm receiver stopped. Local playback remains paused. Retry Stop casting."}).into())
                            });
                        }
                    }
                }
            }
            None => {}
        }
        if let Some(error) = media.as_ref().and_then(MediaGuard::delivery_error) {
            let stopped = runtime.block_on(session.stop_owned_media()).is_ok();
            media = None;
            let source = active_source.take();
            if let Some(source) = &source {
                bridge.finish_cast_source(source);
            }
            if let Some(service) = service.take() {
                service.stop();
            }
            let position = last_snapshot.position;
            let paused = last_snapshot.state == PlaybackState::Paused;
            post(&bridge, generation, move |b, a| {
                let native = b.player.screenshot_session();
                let mut state = b.shared.lock().unwrap();
                state.casting.active = false;
                state.casting.loading = false;
                state.casting.pending_source = None;
                state.casting.source = None;
                if let Some(source) = source {
                    state.casting.recovery = Some((source, position, paused, native));
                }
                drop(state);
                let ui = a.global::<crate::CastBridge>();
                ui.set_active(false);
                ui.set_connecting(false);
                ui.set_uncertain(!stopped);
                ui.set_recovery_available(true);
                ui.set_error(text::tr(media_error_text(error)).into());
            });
        }
        if media.is_some()
            && let Some(service) = &service
        {
            service.update(&device.name, last_snapshot.state == PlaybackState::Paused);
        }
        if media.is_some() && poll_enabled && last_poll.elapsed() >= Duration::from_secs(1) {
            last_poll = Instant::now();
            match runtime.block_on(session.snapshot()) {
                Ok(snapshot) => {
                    if let Some(media) = &media {
                        media.set_playhead(snapshot.position);
                    }
                    let mut snapshot = source_snapshot(snapshot, active_source.as_ref());
                    failures = 0;
                    if snapshot.state == PlaybackState::Idle && snapshot.end_reason.is_none() {
                        idle_observations = idle_observations.saturating_add(1);
                        if idle_observations < 2 {
                            continue;
                        }
                        snapshot.end_reason = Some(nova_cast::EndReason::Cancelled);
                        snapshot.position = last_snapshot.position;
                        snapshot.duration = last_snapshot.duration;
                    } else {
                        idle_observations = 0;
                    }
                    if snapshot.end_reason.is_some() {
                        snapshot.position = last_snapshot.position;
                        snapshot.duration = snapshot.duration.or(last_snapshot.duration);
                    }
                    if let Some(service) = &service {
                        service.set_can_pause(snapshot.owned && snapshot.capabilities.pause);
                    }
                    last_snapshot = snapshot.clone();
                    if snapshot.owned
                        && snapshot.end_reason.is_none()
                        && let Some(target) = active_source
                            .as_mut()
                            .and_then(|source| source.target.as_mut())
                    {
                        bridge.note_cast_progress(
                            target,
                            snapshot.position,
                            snapshot.duration.unwrap_or(0.),
                            snapshot.state != PlaybackState::Playing,
                        );
                    }
                    let terminal = snapshot.end_reason;
                    let finished = terminal == Some(nova_cast::EndReason::Finished);
                    let finished_identity = if finished {
                        active_source
                            .as_ref()
                            .and_then(Source::identity)
                            .map(|identity| (load_id, identity))
                    } else {
                        None
                    };
                    let timer_finished = bridge.cast_sleep_tick(finished_identity);
                    let accepted = snapshot.owned
                        && matches!(
                            snapshot.state,
                            PlaybackState::Playing | PlaybackState::Paused
                        );
                    if terminal.is_none() {
                        let target_session = active_source
                            .as_ref()
                            .and_then(|source| source.target.as_ref())
                            .map_or(0, |target| target.session);
                        bridge.cast_publish_latest(generation, snapshot, load_id, target_session);
                    } else {
                        let target_session = active_source
                            .as_ref()
                            .and_then(|source| source.target.as_ref())
                            .map_or(0, |target| target.session);
                        post(&bridge, generation, move |b, a| {
                            let same_target = b
                                .shared
                                .lock()
                                .unwrap()
                                .playback
                                .as_ref()
                                .is_some_and(|target| target.session == target_session);
                            if (accepted || finished) && !timer_finished && same_target {
                                b.observe_queue_playback(load_id, finished);
                            }
                            b.cast_publish(snapshot);
                            if terminal.is_some() {
                                a.global::<crate::CastBridge>().set_error(
                                text::tr(match terminal {
                                    Some(nova_cast::EndReason::Finished) => {
                                        "Playback finished on receiver"
                                    }
                                    Some(nova_cast::EndReason::Takeover) => {
                                        "Playback changed on another device. Reconnect to continue."
                                    }
                                    _ => "Receiver playback stopped. Choose a source to continue.",
                                })
                                .into(),
                            );
                            }
                        });
                    }
                    if terminal.is_some() {
                        media = None;
                        let source = active_source.take();
                        if let Some(source) = &source {
                            bridge.finish_cast_source(source);
                        }
                        pending = None;
                        if let Some(service) = service.take() {
                            service.stop();
                        }
                        post(&bridge, generation, move |b, a| {
                            {
                                let mut state = b.shared.lock().unwrap();
                                state.casting.active = false;
                                state.casting.source = None;
                                state.casting.pending_source = None;
                                state.casting.recovery = None;
                            }
                            a.global::<crate::CastBridge>().set_active(false);
                            if source
                                .as_ref()
                                .is_none_or(|source| b.playback_request_is_current(&source.request))
                            {
                                let next = if finished && !timer_finished && b.queue_autoplay() {
                                    b.queue_successor()
                                } else {
                                    None
                                };
                                b.finish_playback_request();
                                if let Some(next) = next
                                    && let Err(error) = b.continue_queue_from(&next.key())
                                {
                                    a.global::<crate::CastBridge>()
                                        .set_error(text::tr(&error).into());
                                }
                            }
                        });
                    }
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    post(&bridge, generation, move |_, a| {
                        let ui = a.global::<crate::CastBridge>();
                        ui.set_stale(true);
                        ui.set_error(text::tr(error_text(e.kind)).into());
                    });
                    if failures >= 3 {
                        poll_enabled = false;
                    }
                }
            }
        }
    }
    // Explicit cancellation/shutdown cleans up only media still owned by this session.
    if media.is_some() {
        let _ = runtime.block_on(session.stop_owned_media());
    }
    runtime.block_on(session.close_transport());
    if let Some(source) = &active_source {
        bridge.finish_cast_source(source);
    }
    drop(media);
    if let Some(service) = service.take() {
        service.stop();
    }
    post(&bridge, generation, |b, a| {
        {
            let mut state = b.shared.lock().unwrap();
            state.casting.active = false;
            state.casting.loading = false;
            state.casting.selected = false;
        }
        let ui = a.global::<crate::CastBridge>();
        ui.set_active(false);
        ui.set_selected(false);
        ui.set_connecting(false);
        ui.set_stale(false);
        ui.set_target_id("".into());
        b.finish_playback_request();
    });
}

#[allow(clippy::too_many_arguments)]
fn load(
    bridge: &Bridge,
    generation: u64,
    runtime: &tokio::runtime::Runtime,
    session: &mut ReceiverSession,
    device: &Device,
    cancel: &Arc<AtomicBool>,
    mut source: Source,
    position: f64,
    paused: bool,
    media: &mut Option<MediaGuard>,
    active_source: &mut Option<Source>,
    service: &mut Option<super::android_cast::CastServiceLease>,
) -> bool {
    let mut recovery = None;
    let mut recovery_position = position;
    let mut load_sent = false;
    let mut attempt_guard = None;
    let attempt = Arc::new(AtomicBool::new(false));
    bridge.shared.lock().unwrap().casting.attempt_cancel = Some(attempt.clone());
    let result = (|| {
        if matches!(source.descriptor, SourceDescriptor::LoopbackTorrent { .. })
            && source.torrent_lease.is_none()
        {
            return Err("Torrent source is no longer available. Choose the source again.");
        }
        let options = nova_cast_media::PrepareOptions {
            cache_dir: app_cache_dir().join("casting"),
            selected_audio_stream: source.selected_audio,
            start_position: position,
            max_buffer_bytes: bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .casting_remux_buffer
                .bytes(),
            allow_unverified: source.allow_unverified,
            text_tracks: source
                .subtitles
                .iter()
                .take(8)
                .enumerate()
                .map(|(index, subtitle)| nova_cast_media::SourceTextTrack {
                    id: index as i64 + 1,
                    name: format!("{} {}", text::tr("Subtitle"), index + 1),
                    language: subtitle.language.clone().unwrap_or_default(),
                    url: subtitle.url.clone(),
                    headers: if same_origin(&source.url, &subtitle.url) {
                        source.headers.clone()
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            ..Default::default()
        };
        if service.is_none() {
            *service = Some(super::android_cast::start(&device.name, false).map_err(
                |_| "Background casting could not start. Keep Nova open and try again.",
            )?);
        }
        let lease = service.as_ref().unwrap().clone();
        lease.register_cancellation(attempt.clone());
        let _heartbeat = SetupHeartbeat::start(
            device.name.clone(),
            lease.clone(),
            cancel.clone(),
            attempt.clone(),
            source.request.clone(),
        );
        let profile = nova_cast_media::OutputProfile {
            hls: device.protocol == nova_cast::Protocol::GoogleCast,
            ..Default::default()
        };
        let prepared = nova_cast_media::prepare(
            source.descriptor.clone(),
            device.receiver_ip,
            profile,
            options,
            attempt.clone(),
        )
        .map_err(media_error_text)?;
        if cancel.load(Ordering::Acquire)
            || attempt.load(Ordering::Acquire)
            || !source.request.is_current()
        {
            return Err("Connection cancelled");
        }

        let (sender, ack) = mpsc::sync_channel(1);
        let pause_ticket = Arc::new(HandoffTicket::default());
        let callback_ticket = pause_ticket.clone();
        let callback_cancel = cancel.clone();
        let callback_attempt = attempt.clone();
        let request = source.request.clone();
        post(bridge, generation, move |b, a| {
            if !callback_ticket.commit(
                !callback_cancel.load(Ordering::Acquire)
                    && !callback_attempt.load(Ordering::Acquire)
                    && b.playback_request_is_current(&request),
            ) {
                return;
            }
            let local = a.get_player_open() && a.get_playback_started();
            let position = if local {
                f64::from(a.get_position())
            } else {
                position
            };
            let paused = if local { a.get_is_paused() } else { paused };
            if local {
                b.player.set_paused(true);
            }
            let native = b.player.screenshot_session();
            let _ = sender.send((position, paused, local, native));
        });
        let (position, paused, local, native) =
            pause_ticket.receive(&ack, Duration::from_secs(5))?;
        recovery = Some((local, paused, native));
        recovery_position = position;
        if cancel.load(Ordering::Acquire)
            || attempt.load(Ordering::Acquire)
            || !source.request.is_current()
        {
            return Err("Connection cancelled");
        }
        if position > 3.
            && !prepared.seekable
            && prepared.mode != nova_cast_media::DeliveryMode::PacketCopyHls
        {
            return Err(
                "This source cannot resume on the receiver. Play locally or choose another source.",
            );
        }
        let timeline_offset = prepared.timeline_offset;
        if position + 0.1 < timeline_offset {
            return Err("Playback position changed while preparing. Try casting again.");
        }
        let receiver_position = (position - timeline_offset).max(0.);
        source.timeline_offset = timeline_offset;
        source.duration = prepared.duration;
        prepared.guard.set_playhead(receiver_position);
        attempt_guard = Some(prepared.guard);
        load_sent = true;
        let mut snapshot = match runtime.block_on(
            session.load(MediaLoad {
                url: prepared.receiver_url,
                mime_type: prepared.mime_type,
                title: source.title.clone(),
                subtitle: None,
                duration: prepared
                    .duration
                    .map(|duration| (duration - timeline_offset).max(0.)),
                start_position: receiver_position,
                paused: true,
                text_tracks: prepared.text_tracks.clone(),
            }),
        ) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                if e.kind == CastErrorKind::ReplacementRequired {
                    return Err("Receiver is already in use.");
                }
                return Err(error_text(e.kind));
            }
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !transfer_ready(&snapshot) {
            if cancel.load(Ordering::Acquire)
                || attempt.load(Ordering::Acquire)
                || !source.request.is_current()
                || Instant::now() >= deadline
            {
                let _ = runtime.block_on(session.stop_owned_media());
                return Err("Receiver did not confirm playback. Try another source.");
            }
            runtime.block_on(async {
                tokio::time::sleep(Duration::from_millis(200)).await;
            });
            snapshot = runtime
                .block_on(session.snapshot())
                .map_err(|e| error_text(e.kind))?;
        }
        if cancel.load(Ordering::Acquire)
            || attempt.load(Ordering::Acquire)
            || !source.request.is_current()
        {
            let _ = runtime.block_on(session.stop_owned_media());
            return Err("Connection cancelled");
        }
        if snapshot.state != PlaybackState::Paused {
            snapshot = runtime
                .block_on(session.command(ReceiverCommand::SetPaused(true)))
                .map_err(|_| "Receiver could not pause safely. Local playback was retained.")?;
        }
        if snapshot.state != PlaybackState::Paused {
            return Err("Receiver could not pause safely. Local playback was retained.");
        }
        if !transfer_position_confirmed(&snapshot, position, timeline_offset) {
            if !snapshot.capabilities.seek || !prepared.seekable {
                return Err(
                    "This source cannot resume on the receiver. Play locally or choose another source.",
                );
            }
            snapshot=runtime.block_on(session.command(ReceiverCommand::Seek(receiver_position))).map_err(|_|"Receiver did not confirm the resume position. Local playback was retained.")?;
            if !transfer_position_confirmed(&snapshot, position, timeline_offset) {
                return Err(
                    "Receiver did not confirm the resume position. Local playback was retained.",
                );
            }
        }
        let track_rows = prepared
            .text_tracks
            .iter()
            .filter_map(|track| {
                i32::try_from(track.id)
                    .ok()
                    .map(|id| crate::CastTextTrackRow {
                        id,
                        name: track.name.clone().into(),
                    })
            })
            .collect::<Vec<_>>();
        let selected_text = source.selected_text;
        if let Some(selected) = selected_text
            && snapshot.capabilities.text_tracks
            && prepared.text_tracks.iter().any(|t| t.id == selected)
        {
            snapshot=runtime.block_on(session.command(ReceiverCommand::SetTextTrack(Some(selected)))).map_err(|_|"Selected subtitles could not be enabled on this receiver. Play locally to preserve subtitles.")?;
        }
        if !transfer_ready(&snapshot)
            || snapshot.state != PlaybackState::Paused
            || !transfer_position_confirmed(&snapshot, position, timeline_offset)
        {
            return Err(
                "Receiver did not confirm the resume position. Local playback was retained.",
            );
        }
        let subtitle_notice=source.subtitle_warning.or_else(||(selected_text.is_some()&&!snapshot.capabilities.text_tracks).then_some("This receiver cannot display the selected subtitles. Play locally to preserve them.")).or_else(||(!prepared.text_track_warnings.is_empty()).then_some("Some external subtitles are unavailable on this receiver. Video can continue or you can play locally."));
        snapshot = source_snapshot(snapshot, Some(&source));
        snapshot.capabilities.seek &= prepared.seekable;
        let seekable = prepared.seekable;
        let snapshot_pause_capability = snapshot.owned && snapshot.capabilities.pause;
        let (sender, ack) = mpsc::sync_channel(1);
        let commit_ticket = Arc::new(HandoffTicket::default());
        let callback_ticket = commit_ticket.clone();
        let callback_cancel = cancel.clone();
        let callback_attempt = attempt.clone();
        let title = source.title.clone();
        let commit_request = source.request.clone();
        let commit_target = source.target.clone();
        post(bridge, generation, move |b, a| {
            if !callback_ticket.commit(
                !callback_cancel.load(Ordering::Acquire)
                    && !callback_attempt.load(Ordering::Acquire)
                    && b.playback_request_is_current(&commit_request),
            ) {
                return;
            }
            {
                let mut state = b.shared.lock().unwrap();
                if let Some(target) = commit_target {
                    state.playback = Some(target);
                }
                state.casting.active = true;
                state.casting.loading = false;
                state.casting.seekable = seekable;
                state.casting.timeline_offset = timeline_offset;
                state.casting.pending_source = None;
                state.casting.recovery = None;
                state.casting.source = None;
            }
            b.mark_cast_opened();
            b.player.close();
            a.set_player_open(false);
            a.set_playback_started(false);
            let ui = a.global::<crate::CastBridge>();
            ui.set_open(true);
            ui.set_active(true);
            ui.set_connecting(false);
            ui.set_recovery_available(false);
            ui.set_title(title.into());
            ui.set_text_tracks(Rc::new(VecModel::from(track_rows)).into());
            ui.set_selected_text_track(selected_text.unwrap_or(0) as i32);
            ui.set_subtitle_notice(subtitle_notice.map(text::tr).unwrap_or_default().into());
            b.cast_publish(snapshot);
            let _ = sender.send(());
        });
        commit_ticket.receive(&ack, Duration::from_secs(5))?;
        source.observation_id = bridge.shared.lock().unwrap().casting.observation_id;
        *media = attempt_guard.take();
        *active_source = Some(source.clone());
        if !paused
            && !cancel.load(Ordering::Acquire)
            && !attempt.load(Ordering::Acquire)
            && source.request.is_current()
        {
            match runtime.block_on(session.command(ReceiverCommand::SetPaused(false))) {
                Ok(snapshot) => {
                    let snapshot = source_snapshot(snapshot, Some(&source));
                    post(bridge, generation, move |b, _| b.cast_publish(snapshot));
                }
                Err(e) => post(bridge, generation, move |_, a| {
                    a.global::<crate::CastBridge>()
                        .set_error(text::tr(error_text(e.kind)).into())
                }),
            }
        }
        lease.set_can_pause(snapshot_pause_capability);
        lease.update(&device.name, paused);
        let _ = local;
        Ok(())
    })();
    if let Err(message) = result {
        // The adapter rechecks identity even after cancellation, and never stops another sender.
        let stop_confirmed = !load_sent || runtime.block_on(session.stop_owned_media()).is_ok();
        if load_sent && stop_confirmed {
            if let Some(old) = active_source.take() {
                bridge.finish_cast_source(&old);
            }
            *media = None;
        }
        if !stop_confirmed && let Some(guard) = attempt_guard.take() {
            *media = Some(guard);
            *active_source = Some(source.clone());
        }
        let delivery_active = media.is_some();
        // Cleanup is allowed after generation revocation; publication is not. Restore only the exact retained local session.
        if let Some((true, paused, native)) = recovery {
            let bridge = bridge.clone();
            let request = source.request.clone();
            let _ = profiles::invoke_from_event_loop(move || {
                let state = bridge.shared.lock().unwrap();
                let restore = !state.casting.active
                    && (state.casting.generation == generation || !state.casting.loading)
                    && request.is_current()
                    && bridge.player.screenshot_session() == native;
                drop(state);
                if restore && stop_confirmed {
                    bridge.player.set_paused(paused);
                }
            });
        }
        let recovery_source = source.clone();
        post(bridge, generation, move |b, a| {
            let mut state = b.shared.lock().unwrap();
            state.casting.loading = false;
            state.casting.active = delivery_active;
            let recover_local = !a.get_player_open() && !delivery_active;
            if recover_local {
                state.casting.recovery = Some((
                    recovery_source.clone(),
                    recovery_position,
                    paused,
                    b.player.screenshot_session(),
                ));
            }
            if !stop_confirmed && let Some((_, paused, native)) = recovery {
                state.casting.recovery = Some((recovery_source, recovery_position, paused, native));
            }
            drop(state);
            let ui = a.global::<crate::CastBridge>();
            ui.set_connecting(false);
            ui.set_uncertain(!stop_confirmed);
            ui.set_recovery_available(recover_local);
            if !delivery_active && stop_confirmed {
                ui.set_active(false);
            }
            ui.set_error(text::tr(if stop_confirmed{message}else{"Could not confirm receiver stopped. Local playback remains paused. Retry Stop casting."}).into());
            ui.set_unverified(
                message == media_error_text(nova_cast_media::MediaError::UnverifiedFormat),
            );
        });
        if media.is_none()
            && let Some(service) = service.take()
        {
            service.stop();
        }
        false
    } else {
        true
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // State has Drop, so struct-update syntax would move from a Drop value.
mod tests {
    use super::*;

    #[test]
    fn handoff_timeout_revokes_both_delayed_pause_and_delayed_commit_side_effects() {
        for phase in ["pause", "commit"] {
            let ticket = HandoffTicket::default();
            let (_sender, receiver) = mpsc::sync_channel::<()>(1);
            assert!(
                ticket.receive(&receiver, Duration::ZERO).is_err(),
                "{phase}"
            );
            assert!(
                !ticket.commit(true),
                "a delayed {phase} callback must do nothing"
            );
        }
    }

    #[test]
    fn handoff_commit_winner_cannot_be_revoked_by_a_delayed_ack() {
        for phase in ["pause", "commit"] {
            let ticket = HandoffTicket::default();
            assert!(ticket.commit(true), "{phase}");
            assert!(
                !ticket.cancel_pending(),
                "{phase} already owns its side effects"
            );
            let (sender, receiver) = mpsc::sync_channel(1);
            sender.send(42).unwrap();
            assert_eq!(ticket.receive(&receiver, Duration::ZERO).unwrap(), 42);
            assert!(!ticket.commit(true), "{phase} must never run twice");
        }
    }

    #[test]
    fn expired_handoff_deadline_cannot_be_claimed_before_worker_timeout_runs() {
        let ticket = HandoffTicket::new(Duration::ZERO);
        assert!(!ticket.commit(true));
        assert!(ticket.cancel_pending());
    }

    #[test]
    fn stale_request_cancels_a_pending_handoff_without_claiming_side_effects() {
        let ticket = HandoffTicket::default();
        assert!(!ticket.commit(false));
        assert!(!ticket.commit(true), "a stale request cannot be revived");
        let (_sender, receiver) = mpsc::sync_channel::<()>(1);
        assert!(ticket.receive(&receiver, Duration::ZERO).is_err());
    }

    #[test]
    fn transfer_requires_owned_ready_media_without_a_terminal_reason() {
        let mut snapshot = ReceiverSnapshot {
            owned: true,
            state: PlaybackState::Buffering,
            ..Default::default()
        };
        assert!(
            !transfer_ready(&snapshot),
            "BUFFERING cannot unload the retained local player"
        );
        snapshot.state = PlaybackState::Paused;
        assert!(transfer_ready(&snapshot));
        snapshot.owned = false;
        assert!(
            !transfer_ready(&snapshot),
            "another sender's paused media is not an accepted load"
        );
        snapshot.owned = true;
        snapshot.end_reason = Some(nova_cast::EndReason::Takeover);
        assert!(!transfer_ready(&snapshot));
    }

    #[test]
    fn transfer_position_requires_finite_confirmed_source_time() {
        let mut snapshot = ReceiverSnapshot {
            position: 4.,
            ..Default::default()
        };
        assert!(transfer_position_confirmed(&snapshot, 124., 120.));
        assert!(!transfer_position_confirmed(&snapshot, 400., 120.));
        snapshot.position = f64::NAN;
        assert!(!transfer_position_confirmed(&snapshot, 124., 120.));
        snapshot.position = f64::INFINITY;
        assert!(!transfer_position_confirmed(&snapshot, 124., 120.));
        snapshot.position = 0.166666;
        assert!(transfer_position_confirmed(&snapshot, 0., -0.166666));
    }

    #[test]
    fn subtitle_credentials_stay_on_the_exact_source_origin() {
        assert!(same_origin(
            "https://media.example/video?private=1",
            "https://media.example/sub.vtt?private=2"
        ));
        assert!(!same_origin(
            "https://media.example/video",
            "https://captions.example/sub.vtt"
        ));
        assert!(!same_origin(
            "https://media.example/video",
            "http://media.example/sub.vtt"
        ));
        assert!(!same_origin(
            "https://media.example/video",
            "https://media.example:8443/sub.vtt"
        ));
        assert!(!same_origin(
            "private-token",
            "https://media.example/sub.vtt"
        ));
    }

    #[test]
    fn replacing_an_intent_cancels_only_preparation_not_active_delivery() {
        let active = Arc::new(AtomicBool::new(false));
        let request = crate::torrent::PlaybackRequest::default();
        let mut state = State::default();
        state.attempt_cancel = Some(active.clone());
        state.active = true;
        state.capture_destination(&request);
        assert!(
            !active.load(Ordering::Acquire),
            "active media must survive a new source lookup"
        );
        state.loading = true;
        state.capture_destination(&crate::torrent::PlaybackRequest::default());
        assert!(
            active.load(Ordering::Acquire),
            "a superseded native preparation must stop immediately"
        );
    }

    #[test]
    fn session_drop_cancels_discovery_and_setup_and_wakes_the_worker() {
        let discovery = Arc::new(AtomicBool::new(false));
        let setup = Arc::new(AtomicBool::new(false));
        let lifetime = Arc::new(AtomicBool::new(false));
        let tx = Arc::new(Mailbox::default());
        let rx = tx.clone();
        let mut state = State::default();
        state.discovery_cancel = Some(discovery.clone());
        state.attempt_cancel = Some(setup.clone());
        state.cancel = Some(lifetime.clone());
        state.worker = Some(tx);
        drop(state);
        assert!(discovery.load(Ordering::Acquire));
        assert!(setup.load(Ordering::Acquire));
        assert!(lifetime.load(Ordering::Acquire));
        assert!(matches!(
            rx.receive(Duration::ZERO),
            Some(Command::Shutdown)
        ));
    }

    #[test]
    fn a_cloned_source_lease_prevents_hls_bundle_deletion_until_the_final_release() {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "nova-cast-lease-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let playlist = root.join("index.m3u8");
        std::fs::write(&playlist, b"#EXTM3U").unwrap();
        let lease = FileLease::acquire(playlist.to_str().unwrap()).unwrap();
        let worker = lease.clone();
        assert!(path_leased(&root));
        assert!(path_leased(&playlist));
        drop(lease);
        assert!(path_leased(&root));
        drop(worker);
        assert!(!path_leased(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compatibility_and_network_errors_have_distinct_recovery_messages() {
        assert_ne!(
            media_error_text(nova_cast_media::MediaError::UnverifiedFormat),
            media_error_text(nova_cast_media::MediaError::UnsupportedCodec)
        );
        assert_ne!(
            media_error_text(nova_cast_media::MediaError::Network),
            media_error_text(nova_cast_media::MediaError::UnsupportedCodec)
        );
        assert!(error_text(CastErrorKind::Takeover).contains("another device"));
        for kind in [
            CastErrorKind::Cancelled,
            CastErrorKind::Timeout,
            CastErrorKind::Network,
            CastErrorKind::Protocol,
            CastErrorKind::Unsupported,
            CastErrorKind::Takeover,
            CastErrorKind::ReplacementRequired,
            CastErrorKind::InvalidInput,
        ] {
            assert!(!error_text(kind).contains("http"));
        }
    }
    #[test]
    fn mailbox_coalesces_scrubbing_and_prioritizes_stop_over_pending_commands() {
        let mailbox = Mailbox::default();
        for position in 0..10_000 {
            mailbox.send(Command::Control(ReceiverCommand::Seek(position as f64)));
            mailbox.send(Command::Control(ReceiverCommand::SetVolume(0.5)));
        }
        assert_eq!(mailbox.queue.lock().unwrap().len(), 2);
        mailbox.send(Command::Stop(false));
        assert!(matches!(
            mailbox.receive(Duration::ZERO),
            Some(Command::Stop(false))
        ));
        assert!(mailbox.receive(Duration::ZERO).is_none());
        mailbox.send(Command::Control(ReceiverCommand::SetPaused(false)));
        mailbox.send(Command::Shutdown);
        assert!(matches!(
            mailbox.receive(Duration::ZERO),
            Some(Command::Shutdown)
        ));
    }
    #[test]
    fn ram_receiver_positions_map_to_the_original_resume_timeline_including_b_frame_lead() {
        let mut source = Source {
            descriptor: SourceDescriptor::Http {
                url: "https://example.invalid/private".into(),
                headers: Vec::new(),
                private: true,
            },
            url: String::new(),
            headers: Vec::new(),
            subtitles: Vec::new(),
            request: Default::default(),
            title: String::new(),
            _lease: None,
            torrent_lease: None,
            selected_audio: None,
            allow_unverified: false,
            target: None,
            intent: None,
            selected_text: None,
            subtitle_warning: None,
            observation_id: 1,
            timeline_offset: 120.,
            duration: Some(1800.),
        };
        let snapshot = source_snapshot(
            ReceiverSnapshot {
                position: 4.,
                duration: Some(1680.),
                ..Default::default()
            },
            Some(&source),
        );
        assert_eq!(snapshot.position, 124.);
        assert_eq!(snapshot.duration, Some(1800.));
        source.timeline_offset = -0.166666;
        let snapshot = source_snapshot(
            ReceiverSnapshot {
                position: 0.166666,
                ..Default::default()
            },
            Some(&source),
        );
        assert!(
            snapshot.position.abs() < 0.00001,
            "negative DTS lead must map exactly to source start"
        );
    }
}

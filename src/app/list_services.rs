//! Profile-owned Simkl/MDBList/Trakt actors. Scope is explicit; merge operations are
//! additive and never infer a removal from an absent remote record.
use super::*;
use nova_list_services::{
    Account, ApiError, Client, Credentials, Episode, HttpsTransport, ListStatus, MediaIds,
    MediaType, RemoteTitle, ScrobbleAction, Secret, Service,
};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Instant;

pub(super) const DOMAIN: &str = "list_services";
const MAX_TITLES: usize = 20_000;
const SERVICE_COUNT: usize = 3;
#[cfg(test)]
#[path = "list_services_tests.rs"]
mod tests;
#[path = "list_services_trakt.rs"]
mod trakt;
// library() can perform several bounded pages. Check cancellation at each
// physical request, so cancelling a pass does not start another page afterward.
struct SessionTransport {
    inner: Arc<dyn nova_list_services::Transport>,
    handle: Arc<Handle>,
}
impl nova_list_services::Transport for SessionTransport {
    fn send(
        &self,
        request: nova_list_services::Request,
    ) -> Result<nova_list_services::Response, ApiError> {
        let i = if request.url.starts_with("https://api.simkl.com/") {
            0
        } else if request.url.starts_with("https://api.mdblist.com/") {
            1
        } else {
            2
        };
        if !profiles::is_current(&profiles::capture())
            || self.handle.stop.load(Ordering::Acquire)
            || self
                .handle
                .urgent
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.owner().0 == i)
        {
            return Err(ApiError::Rejected);
        }
        self.inner.send(request)
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Connection {
    version: u32,
    credentials: Credentials,
    account: Account,
    epoch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<trakt::Refresh>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Selection {
    enabled: bool,
    // Existing history is checkpointed on opt-in unless explicitly authorized.
    since: u64,
    include_history: bool,
    grant: String,
    // Journal counters belong to one device; this field never travels over
    // sync. A receiving device checkpoints its own journal for each grant.
    #[serde(skip)]
    event_boundary: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SelectionIdentity {
    kind: MediaType,
    ids: MediaIds,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Preferences {
    full: bool,
    automatic: bool,
}
#[derive(Clone)]
struct Source {
    entry: LibraryEntry,
    ids: MediaIds,
    kind: MediaType,
    episodes: Vec<(String, Episode)>,
    unmapped: usize,
}
#[derive(Clone)]
struct Review {
    service: usize,
    media_type: String,
    id: String,
}
#[derive(Clone)]
struct Permit {
    service: usize,
    generation: u64,
    connection: Connection,
    source: Source,
    selection: Selection,
}
enum Command {
    Device(String, u64),
    Connect(usize, String, Secret, u64),
    Disconnect(usize, u64),
    Cancel(usize, u64),
    Sync(usize, u64),
    Automatic(usize, bool, u64),
    Scope(usize, bool, u64),
    Review(Review, u64),
    Select(Review, Selection, u64),
    SyncTitle(Review, u64),
}
impl Command {
    fn owner(&self) -> (usize, u64) {
        match self {
            Self::Device(_, generation) => (2, *generation),
            Self::Connect(i, _, _, generation)
            | Self::Disconnect(i, generation)
            | Self::Cancel(i, generation)
            | Self::Sync(i, generation)
            | Self::Automatic(i, _, generation)
            | Self::Scope(i, _, generation) => (*i, *generation),
            Self::Review(review, generation)
            | Self::Select(review, _, generation)
            | Self::SyncTitle(review, generation) => (review.service, *generation),
        }
    }
}
pub(super) struct Handle {
    finished: Arc<AtomicBool>,
    stop: AtomicBool,
    generation: [AtomicU64; SERVICE_COUNT],
    tx: SyncSender<Command>,
    rx: Mutex<Option<Receiver<Command>>>,
    urgent: Mutex<Vec<Command>>,
    wake: AtomicBool,
    playback: Mutex<Option<trakt::Observation>>,
    playback_end: Mutex<trakt::PlaybackEnd>,
    scrobbles: Mutex<std::collections::VecDeque<(trakt::Observation, u64)>>,
}
impl Handle {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        Self {
            finished: Arc::new(AtomicBool::new(true)),
            stop: AtomicBool::new(false),
            generation: std::array::from_fn(|_| AtomicU64::new(0)),
            tx,
            rx: Mutex::new(Some(rx)),
            urgent: Mutex::new(Vec::new()),
            wake: AtomicBool::new(false),
            playback: Mutex::new(None),
            playback_end: Mutex::new(trakt::PlaybackEnd::default()),
            scrobbles: Mutex::new(std::collections::VecDeque::new()),
        }
    }
    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    fn send(&self, command: Command) {
        let _ = self.tx.try_send(command);
    }
    fn control(&self, service: usize, command: Command) {
        debug_assert_eq!(service, command.owner().0);
        let mut pending = self.urgent.lock().unwrap();
        pending.retain(|older| !same_control(older, &command));
        pending.push(command);
    }
    fn bump(&self, service: usize) -> u64 {
        self.generation[service].fetch_add(1, Ordering::AcqRel) + 1
    }
}
fn index(service: i32) -> Option<usize> {
    usize::try_from(service).ok().filter(|i| *i < SERVICE_COUNT)
}
fn service(index: usize) -> Service {
    [Service::Simkl, Service::MdbList, Service::Trakt][index]
}
impl Bridge {
    pub(super) fn start_list_services(&self) {
        if !self.profile_services_can_start() {
            return;
        }
        let handle = self.list_services();
        let Some(rx) = handle.rx.lock().unwrap().take() else {
            return;
        };
        handle.finished.store(false, Ordering::Release);
        let done = profile_actions::WorkerDone(handle.finished.clone());
        let bridge = self.clone();
        thread::spawn(move || {
            let _done = done;
            match HttpsTransport::new() {
                Ok(http) => Worker::new(
                    bridge,
                    Client::new(Arc::new(SessionTransport {
                        inner: Arc::new(http),
                        handle,
                    })),
                )
                .run(rx),
                Err(_) => publish(&bridge, |app| {
                    app.set_list_services_status(strings(std::array::from_fn(|_| {
                        text::tr("Service unavailable. Try again.").into()
                    })))
                }),
            }
        });
    }
    pub(super) fn shutdown_list_services(&self) {
        let h = self.list_services();
        h.stop.store(true, Ordering::Release);
        for i in 0..SERVICE_COUNT {
            h.bump(i);
        }
    }
    pub(super) fn list_services_wake(&self) {
        self.list_services().wake.store(true, Ordering::Release);
    }
    pub(super) fn list_services_connect(&self, value: i32, client: String, token: String) {
        let Some(i) = index(value) else {
            return;
        };
        if i == 2 && token.trim().is_empty() {
            let h = self.list_services();
            let generation = h.bump(i);
            h.control(i, Command::Device(client.trim().to_owned(), generation));
            return;
        }
        let Ok(token) = Secret::new(token.trim().to_owned()) else {
            if let Some(app) = self.app() {
                let current = app.get_list_services_status();
                let mut statuses = std::array::from_fn(|index| {
                    current.row_data(index).unwrap_or_default().to_string()
                });
                statuses[i] = text::tr("Enter an API key or access token first.").into();
                app.set_list_services_status(strings(statuses));
            }
            return;
        };
        let h = self.list_services();
        let epoch = h.bump(i);
        h.control(
            i,
            Command::Connect(i, client.trim().to_owned(), token, epoch),
        );
    }
    pub(super) fn list_services_disconnect(&self, value: i32) {
        if let Some(i) = index(value) {
            let h = self.list_services();
            let epoch = h.bump(i);
            h.control(i, Command::Disconnect(i, epoch));
        }
    }
    pub(super) fn list_services_cancel(&self, value: i32) {
        if let Some(i) = index(value) {
            let h = self.list_services();
            let generation = h.bump(i);
            h.control(i, Command::Cancel(i, generation));
        }
    }
    pub(super) fn list_services_sync(&self, value: i32) {
        if let Some(i) = index(value) {
            let h = self.list_services();
            h.send(Command::Sync(i, h.generation[i].load(Ordering::Acquire)));
        }
    }
    pub(super) fn list_services_automatic_changed(&self, value: i32, enabled: bool) {
        if let Some(i) = index(value) {
            let h = self.list_services();
            h.control(
                i,
                Command::Automatic(i, enabled, h.generation[i].load(Ordering::Acquire)),
            );
        }
    }
    pub(super) fn list_services_scope_changed(&self, value: i32, full: bool) {
        if let Some(i) = index(value) {
            let h = self.list_services();
            let generation = h.generation[i].load(Ordering::Acquire);
            h.control(i, Command::Scope(i, full, generation));
        }
    }
    pub(super) fn list_services_open_account(&self, value: i32) {
        if let Some(i) = index(value) {
            let _ = crate::player::open_browser(if i == 0 {
                "https://simkl.com/settings/developer/"
            } else if i == 1 {
                "https://mdblist.com/preferences/"
            } else {
                "https://developer.trakt.tv/apps"
            });
        }
    }
    fn list_services_review(&self, value: i32) -> Option<Review> {
        let service = index(value)?;
        let state = self.personal_state()?;
        let item = state.modal_item.as_ref()?;
        Some(Review {
            service,
            media_type: item.type_.clone(),
            id: item.id.clone(),
        })
    }
    pub(super) fn list_services_title_open(&self, value: i32) {
        if let Some(review) = self.list_services_review(value) {
            let h = self.list_services();
            let generation = h.generation[review.service].load(Ordering::Acquire);
            h.send(Command::Review(review, generation));
        }
    }
    pub(super) fn list_services_title_select(&self, value: i32, enabled: bool, history: bool) {
        if let Some(review) = self.list_services_review(value) {
            let h = self.list_services();
            let generation = h.generation[review.service].load(Ordering::Acquire);
            let boundary = (|| {
                let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                let raw = owner
                    .lock()
                    .unwrap()
                    .extra_value(&profiles::storage_key(nova_tracking::EVENT_COUNTER_KEY))
                    .map_err(|_| ApiError::Rejected)?;
                raw.map(|raw| {
                    serde_json::from_str::<u64>(&raw).map_err(|_| ApiError::InvalidResponse)
                })
                .transpose()
                .map(|value| value.unwrap_or(0))
            })();
            let Ok(event_boundary) = boundary else {
                return;
            };
            h.control(
                review.service,
                Command::Select(
                    review,
                    Selection {
                        enabled,
                        include_history: history,
                        since: now_secs(),
                        grant: format!(
                            "{}:{}:{}",
                            nova_config::now_ms(),
                            nova_sync::local_device().unwrap_or(0),
                            NEXT_GRANT.fetch_add(1, Ordering::Relaxed)
                        ),
                        event_boundary,
                    },
                    generation,
                ),
            );
        }
    }
    pub(super) fn list_services_title_sync(&self, value: i32) {
        if let Some(review) = self.list_services_review(value) {
            let h = self.list_services();
            let generation = h.generation[review.service].load(Ordering::Acquire);
            h.send(Command::SyncTitle(review, generation));
        }
    }
}
struct Worker {
    bridge: Bridge,
    client: Client,
    connections: [Option<Connection>; SERVICE_COUNT],
    verified: [bool; SERVICE_COUNT],
    automatic: [bool; SERVICE_COUNT],
    full: [bool; SERVICE_COUNT],
    busy: [bool; SERVICE_COUNT],
    status: [String; SERVICE_COUNT],
    next: [Instant; SERVICE_COUNT],
    retry: [u64; SERVICE_COUNT],
    review: Option<Review>,
    paused: [bool; SERVICE_COUNT],
    force: [bool; SERVICE_COUNT],
    device: Option<trakt::DeviceSession>,
    last_scrobble: Option<(trakt::Observation, u64)>,
    rejected_scrobbles: std::collections::VecDeque<(trakt::Observation, u64)>,
}
impl Worker {
    fn new(bridge: Bridge, client: Client) -> Self {
        Self {
            bridge,
            client,
            connections: std::array::from_fn(|_| None),
            verified: [false; SERVICE_COUNT],
            automatic: [true; SERVICE_COUNT],
            full: [false; SERVICE_COUNT],
            busy: [false; SERVICE_COUNT],
            status: std::array::from_fn(|_| String::new()),
            next: [Instant::now(); SERVICE_COUNT],
            retry: [5; SERVICE_COUNT],
            review: None,
            paused: [false; SERVICE_COUNT],
            force: [false; SERVICE_COUNT],
            device: None,
            last_scrobble: None,
            rejected_scrobbles: std::collections::VecDeque::new(),
        }
    }
    fn run(mut self, rx: Receiver<Command>) {
        self.publish();
        loop {
            if !profiles::is_current(&profiles::capture())
                || self.bridge.list_services().stop.load(Ordering::Acquire)
            {
                return;
            }
            let urgent = std::mem::take(&mut *self.bridge.list_services().urgent.lock().unwrap());
            for command in urgent {
                self.command(command);
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(command) => self.command(command),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !profiles::is_current(&profiles::capture()) {
                return;
            }
            self.poll_trakt_device();
            let wake = self
                .bridge
                .list_services()
                .wake
                .swap(false, Ordering::AcqRel);
            let mut projected = true;
            for i in 0..SERVICE_COUNT {
                if let Err(error) = self.restore(i) {
                    projected = false;
                    self.failed(i, error);
                }
                if i == 2 {
                    self.flush_trakt_observations();
                }
                if self.verified[i] && self.force[i] {
                    self.force[i] = false;
                    self.sync(i, None);
                } else if self.verified[i]
                    && !self.paused[i]
                    && self.automatic[i]
                    && remote_playback::is_foreground()
                    && (wake || Instant::now() >= self.next[i])
                {
                    self.sync(i, None);
                }
            }
            if projected && let Err(error) = self.acknowledge() {
                self.failed(0, error);
            }
            if wake {
                self.publish();
            }
        }
    }
    fn command(&mut self, command: Command) {
        let (i, generation) = command.owner();
        if !profiles::is_current(&profiles::capture())
            || self.bridge.list_services().generation[i].load(Ordering::Acquire) != generation
        {
            return;
        }
        let result = match command {
            Command::Device(client_id, generation) => {
                Some((2, self.begin_trakt_device(client_id, generation)))
            }
            Command::Connect(i, client_id, token, generation) => {
                self.busy[i] = true;
                self.status[i] = text::tr("Verifying account…").into();
                self.publish();
                let credentials = Credentials {
                    service: service(i),
                    client_id,
                    token,
                };
                let result = self.client.verify(&credentials).and_then(|account| {
                    self.live(i, generation)?;
                    let connection = Connection {
                        version: 1,
                        refresh: None,
                        credentials,
                        account,
                        epoch: format!(
                            "{}:{}",
                            nova_config::now_ms(),
                            nova_sync::local_device().map_err(|_| ApiError::Rejected)?
                        ),
                    };
                    let preferences = self.install_connection(i, &connection)?;
                    self.connections[i] = Some(connection);
                    self.verified[i] = true;
                    self.paused[i] = false;
                    self.full[i] = preferences.full;
                    self.automatic[i] = preferences.automatic;
                    self.next[i] = Instant::now();
                    Ok(())
                });
                self.busy[i] = false;
                self.status[i] = text::tr("Connected").into();
                Some((i, result))
            }
            Command::Disconnect(i, generation) => {
                if i == 2 {
                    self.device = None;
                }
                let result = self.live(i, generation).and_then(|_| {
                    if let Some(current) = self.record::<Connection>(&format!("connection:{i}"))? {
                        self.account_preferences(i, &current)?;
                    }
                    self.set_record::<Connection>(&format!("connection:{i}"), None)
                });
                if result.is_ok() {
                    self.connections[i] = None;
                    self.verified[i] = false;
                    self.busy[i] = false;
                    self.status[i] = text::tr("Not connected").into();
                }
                Some((i, result))
            }
            Command::Cancel(i, _) => {
                if i == 2 {
                    self.device = None;
                }
                self.busy[i] = false;
                self.paused[i] = true;
                self.status[i] = text::tr("Sync paused. Select Sync now to resume.").into();
                None
            }
            Command::Sync(i, _) => {
                self.paused[i] = false;
                self.sync(i, None);
                None
            }
            Command::Automatic(i, enabled, _) => {
                let result = self.flag(i, "automatic", enabled);
                if result.is_ok() {
                    self.automatic[i] = enabled;
                    if i == 2 && !enabled {
                        self.bridge
                            .list_services()
                            .scrobbles
                            .lock()
                            .unwrap()
                            .clear();
                        self.last_scrobble = None;
                    }
                }
                Some((i, result))
            }
            Command::Scope(i, full, _) => {
                let result = self.flag(i, "full", full);
                if result.is_ok() {
                    self.full[i] = full;
                    self.paused[i] = false;
                    self.force[i] = true;
                }
                Some((i, result))
            }
            Command::Review(review, _) => {
                self.review = Some(review);
                None
            }
            Command::Select(review, selection, _) => {
                let i = review.service;
                let enabled = selection.enabled;
                let result = self.connections[i]
                    .as_ref()
                    .ok_or(ApiError::Authentication)
                    .cloned()
                    .and_then(|connection| {
                        storage::try_set_str(
                            &boundary_key(
                                i,
                                &connection,
                                &review.media_type,
                                &review.id,
                                &selection.grant,
                            ),
                            &selection.event_boundary.to_string(),
                        )
                        .map_err(|_| ApiError::Rejected)?;
                        let identity = selection_identity(&review.media_type, &review.id);
                        self.save_selection(i, &connection, &review, &selection, identity.as_ref())
                    });
                self.review = Some(review.clone());
                if result.is_ok() && enabled {
                    self.force[i] = true;
                }
                Some((i, result))
            }
            Command::SyncTitle(review, _) => {
                self.review = Some(review.clone());
                self.paused[review.service] = false;
                self.sync(review.service, Some(&review));
                None
            }
        };
        if let Some((i, Err(error))) = result {
            self.failed(i, error);
        }
        self.publish();
    }
    fn gate(&self, i: usize, generation: u64) -> Result<(), ApiError> {
        self.live(i, generation)?;
        if self
            .bridge
            .list_services()
            .urgent
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.owner().0 == i)
        {
            return Err(ApiError::Rejected);
        }
        Ok(())
    }
    fn live(&self, i: usize, generation: u64) -> Result<(), ApiError> {
        if !profiles::is_current(&profiles::capture())
            || self.bridge.list_services().stop.load(Ordering::Acquire)
            || self.bridge.list_services().generation[i].load(Ordering::Acquire) != generation
        {
            return Err(ApiError::Rejected);
        }
        Ok(())
    }
    fn current(&self, i: usize, generation: u64, connection: &Connection) -> Result<(), ApiError> {
        self.gate(i, generation)?;
        let current: Option<Connection> = self.record(&format!("connection:{i}"))?;
        if current.as_ref().is_none_or(|c| {
            c.epoch != connection.epoch
                || c.account.id != connection.account.id
                || c.credentials.token != connection.credentials.token
                || c.credentials.client_id != connection.credentials.client_id
                || c.credentials.service != connection.credentials.service
        }) {
            return Err(ApiError::Authentication);
        }
        Ok(())
    }
    fn record<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>, ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let store = owner.lock().unwrap();
        store
            .record(&sync::profile_domain(DOMAIN), key)
            .and_then(|r| r.value.as_ref())
            .map(|raw| serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse))
            .transpose()
    }
    fn set_record<T: Serialize>(&self, key: &str, value: Option<&T>) -> Result<(), ApiError> {
        if !profiles::is_current(&profiles::capture()) {
            return Err(ApiError::Rejected);
        }
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let raw = value
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| ApiError::InvalidInput)?;
        let mut store = owner.lock().unwrap();
        store.set(&sync::profile_domain(DOMAIN), key, raw, 0, device);
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn install_connection(
        &self,
        i: usize,
        connection: &Connection,
    ) -> Result<Preferences, ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let preferences = store_connection(&mut store, i, connection, device)?;
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(preferences)
    }
    fn account_preferences(
        &self,
        i: usize,
        connection: &Connection,
    ) -> Result<Preferences, ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let before = store.clock();
        let preferences = migrate_preferences(&mut store, i, connection, device)?;
        let changed = store.clock() != before;
        if store.needs_save() {
            store.save().map_err(|_| ApiError::Rejected)?;
        }
        drop(store);
        if changed {
            nova_sync::request_local_sync();
        }
        Ok(preferences)
    }
    fn save_selection(
        &self,
        i: usize,
        connection: &Connection,
        review: &Review,
        selection: &Selection,
        identity: Option<&SelectionIdentity>,
    ) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let current: Option<Connection> = stored(&store, &format!("connection:{i}"))?;
        if current
            .as_ref()
            .is_none_or(|c| !same_session(c, connection))
        {
            return Err(ApiError::Authentication);
        }
        store_selection(
            &mut store, i, connection, review, selection, identity, device,
        )?;
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn flag(&self, i: usize, name: &str, value: bool) -> Result<(), ApiError> {
        let connection = self.connections[i]
            .as_ref()
            .ok_or(ApiError::Authentication)?;
        self.live(
            i,
            self.bridge.list_services().generation[i].load(Ordering::Acquire),
        )?;
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let current: Option<Connection> = stored(&store, &format!("connection:{i}"))?;
        if current
            .as_ref()
            .is_none_or(|c| !same_session(c, connection))
        {
            return Err(ApiError::Authentication);
        }
        store.set(
            &sync::profile_domain(DOMAIN),
            &flag_key(i, connection, name),
            Some(value.to_string()),
            0,
            device,
        );
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn restore(&mut self, i: usize) -> Result<(), ApiError> {
        let prior = (self.full[i], self.automatic[i], self.verified[i]);
        let loaded: Option<Connection> = self.record(&format!("connection:{i}"))?;
        let same = match (&self.connections[i], &loaded) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.epoch == b.epoch
                    && a.account.id == b.account.id
                    && a.credentials.token == b.credentials.token
                    && a.credentials.client_id == b.credentials.client_id
                    && a.credentials.service == b.credentials.service
                    && a.refresh == b.refresh
            }
            _ => false,
        };
        if !same {
            if i != 2
                || self.connections[i].is_some()
                || self.bridge.list_services().generation[i].load(Ordering::Acquire) != 0
            {
                self.bridge.list_services().bump(i);
            }
            self.verified[i] = false;
            self.connections[i] = loaded;
            self.next[i] = Instant::now();
            self.paused[i] = false;
        }
        if i == 2 {
            self.refresh_trakt()?;
        }
        if let Some(connection) = self.connections[i].clone() {
            if connection.version != 1
                || connection.credentials.service != service(i)
                || connection.account.id.is_empty()
                || connection.epoch.is_empty()
            {
                return Err(ApiError::InvalidResponse);
            }
            let preferences = self.account_preferences(i, &connection)?;
            self.full[i] = preferences.full;
            self.automatic[i] = preferences.automatic;
            self.project_grants(i, &connection)?;
            if !self.verified[i] && !self.paused[i] && Instant::now() >= self.next[i] {
                let generation = self.bridge.list_services().generation[i].load(Ordering::Acquire);
                let account = self.client.verify(&connection.credentials)?;
                self.current(i, generation, &connection)?;
                if account.id != connection.account.id {
                    return Err(ApiError::Authentication);
                }
                if i == 0
                    && let Ok(Some(raw)) = storage::try_get_str(&checkpoint_key(&connection))
                    && let Ok(checkpoint) =
                        serde_json::from_str::<nova_list_services::SimklCheckpoint>(&raw)
                {
                    self.client
                        .restore_simkl_checkpoint(&connection.credentials, checkpoint)?;
                }
                self.verified[i] = true;
                if i == 2 {
                    self.retry[i] = 5;
                    self.status[i] = text::tr("Connected").into();
                }
            }
        }
        if !same || prior != (self.full[i], self.automatic[i], self.verified[i]) {
            self.publish();
        }
        Ok(())
    }
    fn project_grants(&self, i: usize, connection: &Connection) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let sources = {
            let store = owner.lock().unwrap();
            let domain = sync::profile_domain(DOMAIN);
            if !store.pending_domains().contains(&domain) {
                return Ok(());
            }
            store
                .records_with_prefix(&domain, "selected:")
                .iter()
                .filter_map(|(key, _)| {
                    serde_json::from_str::<(usize, String, String, String)>(
                        key.strip_prefix("selected:")?,
                    )
                    .ok()
                })
                .filter(|(service, account, _, _)| {
                    *service == i && account == &connection.account.id
                })
                .map(|(_, _, type_, id)| (type_, id))
                .collect::<Vec<_>>()
        };
        for (type_, id) in sources {
            let _ = self.enabled(i, connection, &type_, &id)?;
        }
        Ok(())
    }
    fn acknowledge(&self) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let domain = sync::profile_domain(DOMAIN);
        if store.pending_domains().contains(&domain) {
            let digest = store.digest().get(&domain).cloned();
            if let Some(basis) = digest {
                store
                    .mark_projected(&domain, &basis)
                    .map_err(|_| ApiError::Rejected)?;
                store.save().map_err(|_| ApiError::Rejected)?;
            }
        }
        Ok(())
    }
    fn selection(
        &self,
        i: usize,
        connection: &Connection,
        type_: &str,
        id: &str,
    ) -> Result<Option<Selection>, ApiError> {
        self.record(&selection_key(i, connection, type_, id))
    }
    fn enabled(
        &self,
        i: usize,
        connection: &Connection,
        type_: &str,
        id: &str,
    ) -> Result<Option<Selection>, ApiError> {
        let full = self
            .record(&flag_key(i, connection, "full"))?
            .unwrap_or(false);
        let mut selected = self.selection(i, connection, type_, id)?;
        if selected.is_some() {
            // Upgrade older native-only choices while current catalog proof is
            // still available. Later library/cache removal cannot erase it.
            let key = identity_key(i, connection, type_, id);
            if self.record::<SelectionIdentity>(&key)?.is_none()
                && let Some(identity) = selection_identity(type_, id)
            {
                self.set_record(&key, Some(&identity))?;
            }
        } else if full && let Some(identity) = selection_identity(type_, id) {
            let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
            if excluded_identity(&owner.lock().unwrap(), i, connection, &identity)? {
                return Ok(None);
            }
        }
        if let Some(s) = selected.as_mut() {
            if s.grant.is_empty() || s.grant.len() > 256 {
                return Err(ApiError::InvalidResponse);
            }
            let key = boundary_key(i, connection, type_, id, &s.grant);
            s.event_boundary = match storage::try_get_str(&key).map_err(|_| ApiError::Rejected)? {
                Some(raw) => serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?,
                None => {
                    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                    let raw = owner
                        .lock()
                        .unwrap()
                        .extra_value(&profiles::storage_key(nova_tracking::EVENT_COUNTER_KEY))
                        .map_err(|_| ApiError::Rejected)?;
                    let boundary = raw
                        .map(|raw| {
                            serde_json::from_str::<u64>(&raw).map_err(|_| ApiError::InvalidResponse)
                        })
                        .transpose()?
                        .unwrap_or(0);
                    storage::try_set_str(&key, &boundary.to_string())
                        .map_err(|_| ApiError::Rejected)?;
                    boundary
                }
            };
        }
        Ok(effective_selection(full, selected))
    }
    fn scope_current(
        &self,
        i: usize,
        generation: u64,
        connection: &Connection,
        source: &Source,
        expected: &Selection,
    ) -> Result<(), ApiError> {
        self.current(i, generation, connection)?;
        if expected.grant.is_empty() {
            let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
            if excluded_identity(
                &owner.lock().unwrap(),
                i,
                connection,
                &SelectionIdentity {
                    kind: source.kind,
                    ids: source.ids.clone(),
                },
            )? {
                return Err(ApiError::Rejected);
            }
        }
        if self
            .enabled(i, connection, &source.entry.type_, &source.entry.id)?
            .as_ref()
            != Some(expected)
        {
            return Err(ApiError::Rejected);
        }
        Ok(())
    }
    fn sync(&mut self, i: usize, only: Option<&Review>) {
        if !self.verified[i] || Instant::now() < self.next[i] && self.retry[i] > 5 {
            return;
        }
        self.busy[i] = true;
        self.status[i] = text::tr("Merging saved titles and watched history…").into();
        self.publish();
        let generation = self.bridge.list_services().generation[i].load(Ordering::Acquire);
        let result = self.merge(i, generation, only);
        self.busy[i] = false;
        match result {
            Ok(pending) => {
                self.retry[i] = 5;
                self.next[i] = Instant::now() + Duration::from_secs(60);
                self.status[i] = if pending == 0 {
                    text::tr("Library and watched history merged").into()
                } else {
                    text::list_services_pending(pending)
                };
            }
            Err(error) => self.failed(i, error),
        }
        self.publish();
    }
    fn merge(
        &mut self,
        i: usize,
        generation: u64,
        only: Option<&Review>,
    ) -> Result<usize, ApiError> {
        let connection = self.connections[i]
            .clone()
            .ok_or(ApiError::Authentication)?;
        self.current(i, generation, &connection)?;
        let entries = self
            .bridge
            .personal_state()
            .ok_or(ApiError::Rejected)?
            .entries
            .clone();
        if entries.len() > MAX_TITLES {
            return Err(ApiError::InvalidInput);
        }
        let mut sources = Vec::new();
        let mut pending = 0;
        for entry in entries.iter().cloned() {
            if only.is_some_and(|r| r.id != entry.id || r.media_type != entry.type_) {
                continue;
            }
            let Some(selection) = self.enabled(i, &connection, &entry.type_, &entry.id)? else {
                continue;
            };
            match source(entry) {
                Some(source) => sources.push((source, selection)),
                None => pending += 1,
            }
        }
        if sources.is_empty() && !self.full[i] {
            return Ok(pending);
        }
        let remote = self.client.library(&connection.credentials)?;
        self.current(i, generation, &connection)?;
        if remote.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        if i == 0
            && let Some(checkpoint) = self.client.simkl_checkpoint(&connection.credentials)
        {
            let raw = serde_json::to_string(&checkpoint).map_err(|_| ApiError::InvalidResponse)?;
            storage::try_set_str(&checkpoint_key(&connection), &raw)
                .map_err(|_| ApiError::Rejected)?;
        }
        if self.full[i] && only.is_none() {
            for item in &remote {
                if item.status == ListStatus::Removed {
                    continue;
                }
                if entries.iter().any(|entry| {
                    (entry.type_ == "movie") == (item.media_type == MediaType::Movie)
                        && media_ids(&entry.type_, &entry.id)
                            .is_some_and(|ids| safe_match(&ids, &item.ids))
                }) {
                    continue;
                }
                let Some(entry) = imported_entry(item) else {
                    pending += 1;
                    continue;
                };
                // Explicit title exclusions still apply in whole-library mode.
                let Some(selection) = self.enabled(i, &connection, &entry.type_, &entry.id)? else {
                    continue;
                };
                if let Some(source) = source(entry) {
                    sources.push((source, selection));
                } else {
                    pending += 1;
                }
            }
        }
        for (source, selection) in sources {
            self.scope_current(i, generation, &connection, &source, &selection)?;
            let matches: Vec<_> = remote
                .iter()
                .filter(|r| {
                    compatible_kind(source.kind, r.media_type) && safe_match(&source.ids, &r.ids)
                })
                .collect();
            if matches.len() > 1 {
                pending += 1;
                continue;
            }
            let remote = matches.first().copied();
            let (saved, progress) = {
                let state = self.bridge.personal_state().ok_or(ApiError::Rejected)?;
                (
                    state
                        .entries
                        .iter()
                        .any(|e| e.id == source.entry.id && e.type_ == source.entry.type_),
                    state.progress.clone(),
                )
            };
            if saved && remote.is_none() {
                self.scope_current(i, generation, &connection, &source, &selection)?;
                self.client.set_list_status(
                    &connection.credentials,
                    &source.ids,
                    source.kind,
                    ListStatus::Planning,
                )?;
            }
            let targets: Vec<_> = if source.kind == MediaType::Movie {
                vec![(source.entry.id.clone(), None)]
            } else {
                source
                    .episodes
                    .iter()
                    .map(|(id, e)| (id.clone(), Some(*e)))
                    .collect()
            };
            let latest_resume = targets
                .iter()
                .filter_map(|(native, episode)| {
                    let p = progress.get(&progress_map_key(&source.entry.id, native))?;
                    (resume_percent(p).is_some()
                        && (selection.include_history || p.updated_at_secs > selection.since))
                        .then_some((native.clone(), *episode, p.clone()))
                })
                .max_by_key(|(_, _, p)| p.updated_at_secs);
            if source.kind != MediaType::Movie && source.episodes.is_empty() {
                pending += 1;
            }
            if source.unmapped > 0 {
                pending += 1;
            }
            let mut imported = HashMap::new();
            for (native, episode) in &targets {
                let key = progress_map_key(&source.entry.id, native);
                let existing = progress.get(&key);
                let remote_watched = remote.is_some_and(|r| match episode {
                    None => r.watched,
                    Some(e) => r.episodes.contains(e),
                });
                let event = local_watched_sequence(&source.entry.id, native)?;
                let export = existing.is_some_and(|p| history_allowed(&selection, p, event));
                let live_scrobble = i == 2
                    && self
                        .bridge
                        .list_services()
                        .has_trakt_observation(&source.entry.id, Some(native));
                if export && !remote_watched && !live_scrobble {
                    self.scope_current(i, generation, &connection, &source, &selection)?;
                    self.client.set_watched(
                        &connection.credentials,
                        &source.ids,
                        source.kind,
                        *episode,
                        true,
                    )?;
                }
                if remote_watched && existing.is_none_or(|p| !p.watched && p.unwatched_at_secs == 0)
                {
                    let mut value = existing.cloned().unwrap_or_else(|| EpisodeProgress {
                        series_id: source.entry.id.clone(),
                        episode_id: native.clone(),
                        history: Some(PlaybackHistory {
                            type_: source.entry.type_.clone(),
                            title: source.entry.name.clone(),
                            poster_url: source.entry.poster_url.clone(),
                            season: episode.map(|e| e.season),
                            episode: episode.map(|e| e.number),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                    value.watched = true;
                    value.updated_at_secs = now_secs();
                    imported.insert(key, value);
                }
            }
            // Never manufacture seconds from an unknown duration or let an
            // unrelated episode's position overwrite the remote checkpoint.
            let live_scrobble = i == 2
                && self
                    .bridge
                    .list_services()
                    .has_trakt_observation(&source.entry.id, None);
            if let Some((_, episode, p)) = latest_resume.filter(|_| !live_scrobble) {
                let percent = resume_percent(&p).unwrap();
                let compatible =
                    remote.is_none_or(|r| r.progress.is_none() || r.playback_episode == episode);
                if compatible
                    && remote
                        .and_then(|r| r.progress)
                        .is_none_or(|r| percent > r + 0.1)
                {
                    self.scope_current(i, generation, &connection, &source, &selection)?;
                    self.client.scrobble(
                        &connection.credentials,
                        &source.ids,
                        source.kind,
                        episode,
                        percent,
                        ScrobbleAction::Pause,
                    )?;
                } else if !compatible {
                    pending += 1;
                }
            }
            if let Some(r) = remote
                && let Some(percent) = r.progress.filter(|p| *p > 0.0 && *p < 100.0)
            {
                if let Some((native, _)) = targets
                    .iter()
                    .find(|(_, episode)| *episode == r.playback_episode)
                {
                    let key = progress_map_key(&source.entry.id, native);
                    if let Some(p) = progress
                        .get(&key)
                        .filter(|p| p.duration_secs.is_finite() && p.duration_secs > 0.0)
                    {
                        let current_percent = p.position_secs / p.duration_secs * 100.0;
                        if percent > current_percent + 0.1 && !p.watched {
                            let mut value = imported.remove(&key).unwrap_or_else(|| p.clone());
                            value.position_secs = p.duration_secs * percent / 100.0;
                            value.updated_at_secs = now_secs();
                            imported.insert(key, value);
                        }
                    } else {
                        pending += 1;
                    }
                } else {
                    pending += 1;
                }
            }
            if let Some(remote) = remote {
                let uncovered = remote
                    .episodes
                    .iter()
                    .filter(|episode| !source.episodes.iter().any(|(_, e)| e == *episode))
                    .count();
                if uncovered > 0 {
                    pending += 1;
                }
            }
            let mut patch = persistence::ExternalPatch {
                progress: imported,
                library: None,
            };
            if !saved && remote.is_some() {
                patch.library = Some(persistence::ExternalLibraryEdit {
                    id: source.entry.id.clone(),
                    media_type: source.entry.type_.clone(),
                    entry: Some(source.entry.clone()),
                });
            }
            if !patch.is_empty() {
                self.scope_current(i, generation, &connection, &source, &selection)?;
                self.apply(
                    Permit {
                        service: i,
                        generation,
                        connection: connection.clone(),
                        source,
                        selection,
                    },
                    patch,
                    progress,
                )?;
            }
        }
        Ok(pending)
    }
    fn apply(
        &self,
        permit: Permit,
        patch: persistence::ExternalPatch,
        before: HashMap<String, EpisodeProgress>,
    ) -> Result<(), ApiError> {
        let Permit {
            service: i,
            generation,
            connection,
            source,
            selection,
        } = permit;
        self.scope_current(i, generation, &connection, &source, &selection)?;
        let bridge = self.bridge.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        profiles::invoke_from_event_loop(move || {
            let result = (|| {
                if bridge.list_services().generation[i].load(Ordering::Acquire) != generation {
                    return Err(ApiError::Rejected);
                }
                if bridge
                    .list_services()
                    .urgent
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|c| c.owner().0 == i)
                {
                    return Err(ApiError::Rejected);
                }
                let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                let store = owner.try_lock().map_err(|_| ApiError::Rejected)?;
                let current = store
                    .record(&sync::profile_domain(DOMAIN), &format!("connection:{i}"))
                    .and_then(|r| r.value.as_ref())
                    .and_then(|raw| serde_json::from_str::<Connection>(raw).ok());
                if current.is_none_or(|c| c.epoch != connection.epoch) {
                    return Err(ApiError::Authentication);
                }
                let domain = sync::profile_domain(DOMAIN);
                let decode = |key: &str| store.record(&domain, key).and_then(|r| r.value.as_ref());
                let full = decode(&flag_key(i, &connection, "full"))
                    .map(|raw| serde_json::from_str::<bool>(raw))
                    .transpose()
                    .map_err(|_| ApiError::InvalidResponse)?
                    .unwrap_or(false);
                let mut selected = decode(&selection_key(
                    i,
                    &connection,
                    &source.entry.type_,
                    &source.entry.id,
                ))
                .map(|raw| serde_json::from_str::<Selection>(raw))
                .transpose()
                .map_err(|_| ApiError::InvalidResponse)?;
                if let Some(s) = selected.as_mut() {
                    s.event_boundary = selection.event_boundary;
                }
                if effective_selection(full, selected).as_ref() != Some(&selection) {
                    return Err(ApiError::Rejected);
                }
                if selection.grant.is_empty()
                    && excluded_identity(
                        &store,
                        i,
                        &connection,
                        &SelectionIdentity {
                            kind: source.kind,
                            ids: source.ids.clone(),
                        },
                    )?
                {
                    return Err(ApiError::Rejected);
                }
                let _guard = sync::ApplyingGuard::new();
                let _external = stremio_events::ExternalGuard::new();
                let mut state = bridge.shared.try_lock().map_err(|_| ApiError::Rejected)?;
                if !profiles::is_current(&profiles::capture()) {
                    return Err(ApiError::Rejected);
                }
                if patch
                    .progress
                    .keys()
                    .any(|key| state.progress.get(key) != before.get(key))
                {
                    return Err(ApiError::Rejected);
                }
                if let Some(edit) = &patch.library
                    && state
                        .entries
                        .iter()
                        .any(|e| e.id == edit.id && e.type_ == edit.media_type)
                {
                    return Err(ApiError::Rejected);
                }
                let receipt = persistence::try_external(patch.clone()).ok_or(ApiError::Rejected)?;
                if let Some(edit) = patch.library
                    && let Some(entry) = edit.entry
                {
                    state.entries.push(entry);
                }
                state.progress.extend(patch.progress);
                drop(state);
                bridge
                    .stremio()
                    .surfaces_dirty
                    .store(true, Ordering::Release);
                Ok(receipt)
            })();
            let _ = tx.send(result);
        })
        .map_err(|_| ApiError::Rejected)?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| ApiError::Rejected)??
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| ApiError::Rejected)?
            .map_err(|_| ApiError::Rejected)
    }
    fn failed(&mut self, i: usize, error: ApiError) {
        self.busy[i] = false;
        let seconds = match error {
            ApiError::RateLimited { retry_after } => retry_after.clamp(1, 86_400),
            ApiError::Pending if i == 2 => self.retry[i].max(30),
            _ => self.retry[i],
        };
        self.next[i] = Instant::now() + Duration::from_secs(seconds);
        self.retry[i] = (self.retry[i] * 2).min(300);
        self.status[i] = text::tr(match error {
            ApiError::Authentication => {
                self.verified[i] = false;
                self.paused[i] = true;
                "Reconnect this account to continue"
            }
            ApiError::RateLimited { .. } => "Service rate limit reached. Sync will retry.",
            ApiError::Pending if i == 2 => {
                "Waiting for refreshed sign-in from paired devices. Reconnect if this continues."
            }
            ApiError::Rejected => "Sync interrupted or local save failed. Try Sync now.",
            ApiError::InvalidInput | ApiError::InvalidResponse => {
                "Unsupported or invalid service data. Some titles remain pending."
            }
            ApiError::Unsupported => "This operation is unavailable for this service.",
            _ => "Service unavailable. Sync will retry.",
        })
        .into();
        self.publish();
    }
    fn publish(&self) {
        let accounts = std::array::from_fn(|i| {
            self.connections[i]
                .as_ref()
                .map(|c| c.account.name.clone())
                .unwrap_or_default()
        });
        let status = self.status.clone();
        let connected = self.verified;
        let busy = self.busy;
        let automatic = self.automatic;
        let full = self.full;
        let review = self.review.clone();
        let title = review.as_ref().map(|r| {
            let connection = self.connections[r.service].as_ref();
            let selected = connection
                .and_then(|c| {
                    self.enabled(r.service, c, &r.media_type, &r.id)
                        .ok()
                        .flatten()
                })
                .is_some();
            let ids = media_ids(&r.media_type, &r.id);
            let resolved = ids.is_some();
            let identity = ids
                .map(|ids| {
                    ids.imdb
                        .unwrap_or_else(|| format!("TMDB {}", ids.tmdb.unwrap_or_default()))
                })
                .unwrap_or_else(|| {
                    text::tr("No confirmed IMDb or TMDB identity. Refresh this title's metadata.")
                        .into()
                });
            (selected, resolved, identity)
        });
        let bridge = self.bridge.clone();
        publish(&self.bridge, move |app| {
            app.set_list_services_account(strings(accounts));
            app.set_list_services_status(strings(status.clone()));
            app.set_list_services_connected(bools(connected));
            app.set_list_services_busy(bools(busy));
            app.set_list_services_automatic(bools(automatic));
            app.set_list_services_whole_library(bools(full));
            if let (Some(review), Some((selected, resolved, identity))) = (review, title) {
                if app.get_tracking_service() != review.service as i32 + 3
                    || bridge.personal_state().is_none_or(|s| {
                        s.modal_item
                            .as_ref()
                            .is_none_or(|m| m.id != review.id || m.type_ != review.media_type)
                    })
                {
                    return;
                }
                app.set_list_services_title_selected(selected);
                app.set_list_services_title_resolved(resolved);
                app.set_list_services_title_connected(connected[review.service]);
                app.set_list_services_title_busy(busy[review.service]);
                app.set_list_services_title_status(status[review.service].clone().into());
                app.set_list_services_title_identity(identity.into());
            }
        });
    }
}
fn flag_key(i: usize, c: &Connection, field: &str) -> String {
    format!(
        "account:{}:{field}",
        serde_json::to_string(&(i, &c.account.id)).unwrap()
    )
}
fn legacy_flag_key(i: usize, c: &Connection, field: &str) -> String {
    format!(
        "session:{i}:{}:{field}",
        serde_json::to_string(&c.epoch).unwrap()
    )
}
fn same_session(a: &Connection, b: &Connection) -> bool {
    a.version == 1
        && b.version == 1
        && a.epoch == b.epoch
        && a.account.id == b.account.id
        && a.credentials.service == b.credentials.service
        && a.credentials.client_id == b.credentials.client_id
        && a.credentials.token == b.credentials.token
}
fn stored<T: serde::de::DeserializeOwned>(
    store: &nova_sync::Store,
    key: &str,
) -> Result<Option<T>, ApiError> {
    store
        .record(&sync::profile_domain(DOMAIN), key)
        .and_then(|r| r.value.as_ref())
        .map(|raw| serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse))
        .transpose()
}
fn migrate_preferences(
    store: &mut nova_sync::Store,
    i: usize,
    connection: &Connection,
    device: u64,
) -> Result<Preferences, ApiError> {
    let mut values = [false, true];
    for (index, field) in ["full", "automatic"].iter().enumerate() {
        let key = flag_key(i, connection, field);
        values[index] = match stored::<bool>(store, &key)? {
            Some(value) => value,
            None => {
                let value = stored::<bool>(store, &legacy_flag_key(i, connection, field))?
                    .unwrap_or(values[index]);
                store.set(
                    &sync::profile_domain(DOMAIN),
                    &key,
                    Some(value.to_string()),
                    0,
                    device,
                );
                value
            }
        };
    }
    Ok(Preferences {
        full: values[0],
        automatic: values[1],
    })
}
fn store_connection(
    store: &mut nova_sync::Store,
    i: usize,
    connection: &Connection,
    device: u64,
) -> Result<Preferences, ApiError> {
    // Preserve the previous account's legacy session preferences before
    // replacing its only durable credential pointer with another epoch.
    if let Some(previous) = stored::<Connection>(store, &format!("connection:{i}"))?
        && previous.version == 1
        && previous.credentials.service == service(i)
    {
        migrate_preferences(store, i, &previous, device)?;
    }
    let preferences = migrate_preferences(store, i, connection, device)?;
    store.set(
        &sync::profile_domain(DOMAIN),
        &format!("connection:{i}"),
        Some(serde_json::to_string(connection).map_err(|_| ApiError::InvalidInput)?),
        0,
        device,
    );
    Ok(preferences)
}
fn identity_key(i: usize, connection: &Connection, type_: &str, id: &str) -> String {
    format!(
        "identity:{}",
        serde_json::to_string(&(i, &connection.account.id, type_, id)).unwrap()
    )
}
fn selection_identity(type_: &str, id: &str) -> Option<SelectionIdentity> {
    let kind = match type_ {
        "movie" => MediaType::Movie,
        "series" | "anime" => MediaType::Show,
        _ => return None,
    };
    Some(SelectionIdentity {
        kind,
        ids: media_ids(type_, id)?,
    })
}
fn store_selection(
    store: &mut nova_sync::Store,
    i: usize,
    connection: &Connection,
    review: &Review,
    selection: &Selection,
    identity: Option<&SelectionIdentity>,
    device: u64,
) -> Result<(), ApiError> {
    let domain = sync::profile_domain(DOMAIN);
    let raw = serde_json::to_string(selection).map_err(|_| ApiError::InvalidInput)?;
    let identity = identity
        .map(|identity| {
            if !identity.ids.valid() {
                return Err(ApiError::InvalidInput);
            }
            serde_json::to_string(identity).map_err(|_| ApiError::InvalidInput)
        })
        .transpose()?;
    if let Some(raw) = identity {
        store.set(
            &domain,
            &identity_key(i, connection, &review.media_type, &review.id),
            Some(raw),
            0,
            device,
        );
    }
    // Retain an earlier proof if the title or metadata was removed before the
    // user stopped tracking it. Re-enabling the native choice lifts exclusion.
    store.set(
        &domain,
        &selection_key(i, connection, &review.media_type, &review.id),
        Some(raw),
        0,
        device,
    );
    Ok(())
}
fn excluded_identity(
    store: &nova_sync::Store,
    i: usize,
    connection: &Connection,
    identity: &SelectionIdentity,
) -> Result<bool, ApiError> {
    let prefix = format!(
        "identity:[{i},{},",
        serde_json::to_string(&connection.account.id).unwrap()
    );
    let rows = store.records_with_prefix(&sync::profile_domain(DOMAIN), &prefix);
    if rows.len() > MAX_TITLES {
        return Err(ApiError::InvalidResponse);
    }
    for (key, raw) in rows {
        let retained: SelectionIdentity =
            serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?;
        if !retained.ids.valid() {
            return Err(ApiError::InvalidResponse);
        }
        if compatible_kind(retained.kind, identity.kind) && safe_match(&retained.ids, &identity.ids)
        {
            let choice = format!(
                "selected:{}",
                key.strip_prefix("identity:")
                    .ok_or(ApiError::InvalidResponse)?
            );
            if stored::<Selection>(store, &choice)?.is_some_and(|selection| !selection.enabled) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn same_control(a: &Command, b: &Command) -> bool {
    if a.owner().0 != b.owner().0 {
        return false;
    }
    match (a, b) {
        (Command::Select(a, _, _), Command::Select(b, _, _)) => {
            a.id == b.id && a.media_type == b.media_type
        }
        (Command::Device(..), Command::Device(..)) => true,
        (Command::Scope(..), Command::Scope(..))
        | (Command::Automatic(..), Command::Automatic(..))
        | (Command::Cancel(..), Command::Cancel(..)) => true,
        (
            Command::Connect(..) | Command::Disconnect(..),
            Command::Connect(..) | Command::Disconnect(..),
        ) => true,
        _ => false,
    }
}
fn checkpoint_key(c: &Connection) -> String {
    format!(
        "list_services:simkl_cache:v1:{}",
        serde_json::to_string(&c.account.id).unwrap()
    )
}
fn effective_selection(full: bool, selected: Option<Selection>) -> Option<Selection> {
    match selected {
        Some(s) if !s.enabled => None,
        Some(mut s) => {
            if full {
                s.include_history = true;
                s.since = 0;
                s.event_boundary = 0;
            }
            Some(s)
        }
        None if full => Some(Selection {
            enabled: true,
            since: 0,
            include_history: true,
            grant: String::new(),
            event_boundary: 0,
        }),
        None => None,
    }
}
fn history_allowed(selection: &Selection, progress: &EpisodeProgress, event: u64) -> bool {
    progress.watched && (selection.include_history || event > selection.event_boundary)
}
static NEXT_GRANT: AtomicU64 = AtomicU64::new(1);
fn boundary_key(i: usize, c: &Connection, type_: &str, id: &str, grant: &str) -> String {
    format!(
        "list_services:boundary:v1:{}",
        serde_json::to_string(&(i, &c.account.id, type_, id, grant)).unwrap()
    )
}
fn watched_event_key(series: &str, episode: &str) -> String {
    format!(
        "list_services:watched:v1:{}",
        serde_json::to_string(&(series, episode)).unwrap()
    )
}
pub(super) fn local_watched_sequence(series: &str, episode: &str) -> Result<u64, ApiError> {
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let raw = owner
        .lock()
        .unwrap()
        .extra_value(&profiles::storage_key(&watched_event_key(series, episode)))
        .map_err(|_| ApiError::Rejected)?;
    raw.map(|raw| serde_json::from_str::<u64>(&raw).map_err(|_| ApiError::InvalidResponse))
        .transpose()
        .map(|value| value.unwrap_or(0))
}
pub(super) fn record_watched_events(
    store: &mut nova_sync::Store,
    before: &HashMap<String, EpisodeProgress>,
    changes: &[nova_tracking::WatchChange],
    sequence: u64,
    remote: bool,
) {
    if remote {
        return;
    }
    for change in changes.iter().filter(|c| c.watched) {
        if before
            .get(&progress_map_key(&change.series_id, &change.episode_id))
            .is_some_and(|p| p.watched)
        {
            continue;
        }
        store.queue_extra(
            &profiles::storage_key(&watched_event_key(&change.series_id, &change.episode_id)),
            Some(sequence.to_string()),
        );
    }
}
fn resume_percent(progress: &EpisodeProgress) -> Option<f64> {
    if progress.watched
        || !progress.position_secs.is_finite()
        || !progress.duration_secs.is_finite()
        || progress.position_secs <= 0.0
        || progress.duration_secs <= progress.position_secs
    {
        return None;
    }
    let percent = progress.position_secs / progress.duration_secs * 100.0;
    (percent > 0.0 && percent < 100.0).then_some(percent)
}
fn selection_key(i: usize, c: &Connection, type_: &str, id: &str) -> String {
    format!(
        "selected:{}",
        serde_json::to_string(&(i, &c.account.id, type_, id)).unwrap()
    )
}
fn compatible_kind(a: MediaType, b: MediaType) -> bool {
    (a == MediaType::Movie) == (b == MediaType::Movie)
}
fn safe_match(a: &MediaIds, b: &MediaIds) -> bool {
    a.matches(b)
        && !a
            .imdb
            .as_ref()
            .zip(b.imdb.as_ref())
            .is_some_and(|(a, b)| a != b)
        && !a.tmdb.zip(b.tmdb).is_some_and(|(a, b)| a != b)
}
fn media_ids(type_: &str, id: &str) -> Option<MediaIds> {
    use nova_providers::{ExternalId, IdNamespace, IdResolution};
    let ids = tracking::current_source_ids(type_, id);
    let imdb = match ids.resolve_id(IdNamespace::Imdb) {
        IdResolution::Unique(ExternalId::Imdb(id)) => Some(id),
        IdResolution::Conflict(_) => return None,
        _ => None,
    };
    let tmdb = match ids.resolve_id(if type_ == "movie" {
        IdNamespace::TmdbMovie
    } else {
        IdNamespace::TmdbTv
    }) {
        IdResolution::Unique(ExternalId::TmdbMovie(id) | ExternalId::TmdbTv(id)) => {
            Some(u64::from(id.get()))
        }
        IdResolution::Conflict(_) => return None,
        _ => None,
    };
    let result = MediaIds {
        imdb,
        tmdb,
        ..Default::default()
    };
    result.valid().then_some(result)
}
fn source(entry: LibraryEntry) -> Option<Source> {
    let ids = media_ids(&entry.type_, &entry.id)?;
    let kind = if entry.type_ == "movie" {
        MediaType::Movie
    } else if matches!(entry.type_.as_str(), "series" | "anime") {
        MediaType::Show
    } else {
        return None;
    };
    // Native numbering can describe split/merged seasons. Only canonical episode
    // aliases prove the remote season and episode; parent identity alone cannot.
    let episodes = read_episodes_cache_for(&entry.type_, &entry.id).unwrap_or_default();
    let mapped = episode_mapping(ids.imdb.as_deref(), &episodes);
    let unmapped = if kind == MediaType::Movie {
        0
    } else {
        episodes.len().saturating_sub(mapped.len())
    };
    Some(Source {
        entry,
        ids,
        kind,
        episodes: mapped,
        unmapped,
    })
}
fn episode_mapping(imdb: Option<&str>, videos: &[Video]) -> Vec<(String, Episode)> {
    let mut mapped: Vec<_> = videos
        .iter()
        .filter_map(|v| {
            let imdb = imdb?;
            let prefix = format!("{imdb}:");
            let mut candidates: Vec<_> = std::iter::once(v.id.clone())
                .chain(confirmed_episode_stream_ids(v))
                .filter_map(|id| {
                    let suffix = id.strip_prefix(&prefix)?;
                    let (s, e) = suffix.split_once(':')?;
                    let result = Episode {
                        season: s.parse().ok()?,
                        number: e.parse().ok()?,
                    };
                    (result.number > 0).then_some(result)
                })
                .collect();
            candidates.sort_by_key(|e| (e.season, e.number));
            candidates.dedup();
            (candidates.len() == 1).then(|| (v.id.clone(), candidates[0]))
        })
        .collect();
    let mut counts = HashMap::new();
    for (_, e) in &mapped {
        *counts.entry((e.season, e.number)).or_insert(0usize) += 1;
    }
    mapped.retain(|(_, e)| counts[&(e.season, e.number)] == 1);
    mapped
}
fn imported_entry(remote: &RemoteTitle) -> Option<LibraryEntry> {
    let type_ = if remote.media_type == MediaType::Movie {
        "movie"
    } else {
        "series"
    };
    let id = remote.ids.imdb.clone().or_else(|| {
        remote.ids.tmdb.map(|id| {
            format!(
                "tmdb:{}:{id}",
                if type_ == "movie" { "movie" } else { "tv" }
            )
        })
    })?;
    Some(LibraryEntry {
        id,
        type_: type_.into(),
        name: remote.title.clone(),
        year: remote.year.map(|y| y.to_string()).unwrap_or_default(),
        poster_url: String::new(),
        background_url: String::new(),
        genres: vec![],
        description: String::new(),
        categories: vec![],
        watch_status: WatchStatus::Auto,
        imported_tracking_status: None,
        added_at_secs: now_secs(),
    })
}
fn strings(values: [String; SERVICE_COUNT]) -> slint::ModelRc<slint::SharedString> {
    slint::ModelRc::new(slint::VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}
pub(super) fn clear_ui(app: &AppWindow) {
    app.set_list_services_account(strings(std::array::from_fn(|_| String::new())));
    app.set_list_services_status(strings(std::array::from_fn(|_| String::new())));
    app.set_list_services_connected(bools([false; SERVICE_COUNT]));
    app.set_list_services_busy(bools([false; SERVICE_COUNT]));
    app.set_list_services_automatic(bools([true; SERVICE_COUNT]));
    app.set_list_services_whole_library(bools([false; SERVICE_COUNT]));
    app.set_list_services_title_selected(false);
    app.set_list_services_title_connected(false);
    app.set_list_services_title_busy(false);
    app.set_list_services_title_resolved(false);
    app.set_list_services_title_status("".into());
    app.set_list_services_title_identity("".into());
}
fn bools(values: [bool; SERVICE_COUNT]) -> slint::ModelRc<bool> {
    slint::ModelRc::new(slint::VecModel::from(values.to_vec()))
}

#[cfg(test)]
mod policy_regressions {
    use super::*;

    fn connection(i: usize, account: &str, epoch: &str) -> Connection {
        Connection {
            version: 1,
            refresh: None,
            credentials: Credentials {
                service: service(i),
                client_id: "nova-test".into(),
                token: Secret::new(format!("token-{account}-{epoch}")).unwrap(),
            },
            account: Account {
                id: account.into(),
                name: account.into(),
            },
            epoch: epoch.into(),
        }
    }
    fn choice(enabled: bool) -> Selection {
        Selection {
            enabled,
            since: 10,
            include_history: false,
            grant: "explicit-native-choice".into(),
            event_boundary: 0,
        }
    }
    fn identity() -> SelectionIdentity {
        SelectionIdentity {
            kind: MediaType::Show,
            ids: MediaIds {
                imdb: Some("tt0903747".into()),
                tmdb: Some(1396),
                ..Default::default()
            },
        }
    }

    #[test]
    fn native_exclusion_blocks_canonical_reimport_after_native_metadata_is_gone() {
        for i in 0..SERVICE_COUNT {
            let connection = connection(i, "account-a", "first-sign-in");
            let review = Review {
                service: i,
                media_type: "series".into(),
                id: "native:opaque-season".into(),
            };
            let mut store = nova_sync::Store::default();
            // Capture current proof with the original grant. Stopping later
            // must preserve it even if no native entry or metadata remains.
            store_selection(
                &mut store,
                i,
                &connection,
                &review,
                &choice(true),
                Some(&identity()),
                1,
            )
            .unwrap();
            store_selection(&mut store, i, &connection, &review, &choice(false), None, 1).unwrap();
            let remote = RemoteTitle {
                ids: identity().ids,
                media_type: MediaType::Show,
                title: "Breaking Bad".into(),
                year: None,
                status: ListStatus::Planning,
                episodes: Vec::new(),
                watched: false,
                progress: None,
                playback_episode: None,
            };
            let canonical = imported_entry(&remote).unwrap();
            assert_ne!(canonical.id, review.id);
            let no_native_choice = stored::<Selection>(
                &store,
                &selection_key(i, &connection, &canonical.type_, &canonical.id),
            )
            .unwrap();
            assert!(
                effective_selection(true, no_native_choice).is_some(),
                "the native key alone cannot protect a canonical import"
            );
            assert!(
                excluded_identity(
                    &store,
                    i,
                    &connection,
                    &SelectionIdentity {
                        kind: remote.media_type,
                        ids: remote.ids
                    }
                )
                .unwrap()
            );

            let other = self::connection(i, "account-b", "another-sign-in");
            assert!(!excluded_identity(&store, i, &other, &identity()).unwrap());
            let movie = SelectionIdentity {
                kind: MediaType::Movie,
                ..identity()
            };
            assert!(!excluded_identity(&store, i, &connection, &movie).unwrap());
            let conflicting = SelectionIdentity {
                ids: MediaIds {
                    imdb: Some("tt0944947".into()),
                    tmdb: Some(1396),
                    ..Default::default()
                },
                ..identity()
            };
            assert!(!excluded_identity(&store, i, &connection, &conflicting).unwrap());
        }
    }

    #[test]
    fn returning_to_selected_mode_retains_choices_and_explicit_reenable_lifts_exclusion() {
        let connection = connection(1, "account-a", "first");
        let review = Review {
            service: 1,
            media_type: "series".into(),
            id: "native:season".into(),
        };
        let mut store = nova_sync::Store::default();
        store_selection(
            &mut store,
            1,
            &connection,
            &review,
            &choice(false),
            Some(&identity()),
            1,
        )
        .unwrap();
        for full in [true, false] {
            store.set(
                &sync::profile_domain(DOMAIN),
                &flag_key(1, &connection, "full"),
                Some(full.to_string()),
                0,
                1,
            );
            let retained = stored::<Selection>(
                &store,
                &selection_key(1, &connection, &review.media_type, &review.id),
            )
            .unwrap();
            assert!(effective_selection(full, retained).is_none());
            assert!(excluded_identity(&store, 1, &connection, &identity()).unwrap());
        }
        store_selection(&mut store, 1, &connection, &review, &choice(true), None, 1).unwrap();
        assert!(!excluded_identity(&store, 1, &connection, &identity()).unwrap());
        let selected = stored::<Selection>(
            &store,
            &selection_key(1, &connection, &review.media_type, &review.id),
        )
        .unwrap();
        assert_eq!(effective_selection(false, selected), Some(choice(true)));
    }

    #[test]
    fn reconnecting_a_then_b_then_a_restores_account_preferences_with_fresh_session_guards() {
        for i in 0..SERVICE_COUNT {
            let mut store = nova_sync::Store::default();
            let a = connection(i, "account-a", "a-first");
            store_connection(&mut store, i, &a, 1).unwrap();
            for (field, value) in [("full", true), ("automatic", false)] {
                store.set(
                    &sync::profile_domain(DOMAIN),
                    &flag_key(i, &a, field),
                    Some(value.to_string()),
                    0,
                    1,
                );
            }
            let b = connection(i, "account-b", "b-first");
            assert_eq!(
                store_connection(&mut store, i, &b, 1).unwrap(),
                Preferences {
                    full: false,
                    automatic: true
                }
            );
            let returning = connection(i, "account-a", "a-second");
            assert_eq!(
                store_connection(&mut store, i, &returning, 1).unwrap(),
                Preferences {
                    full: true,
                    automatic: false
                }
            );
            assert_eq!(flag_key(i, &a, "full"), flag_key(i, &returning, "full"));
            assert!(
                !same_session(&a, &returning),
                "retained preferences cannot authorize an old credential epoch"
            );
            assert!(!same_session(&b, &returning));
            let current: Connection = stored(&store, &format!("connection:{i}")).unwrap().unwrap();
            assert!(same_session(&current, &returning));
        }
    }

    #[test]
    fn active_legacy_preferences_migrate_before_account_switch_and_do_not_overwrite_new_choices() {
        let mut store = nova_sync::Store::default();
        let old = connection(0, "account-a", "legacy-epoch");
        store.set(
            &sync::profile_domain(DOMAIN),
            "connection:0",
            Some(serde_json::to_string(&old).unwrap()),
            0,
            1,
        );
        store.set(
            &sync::profile_domain(DOMAIN),
            &legacy_flag_key(0, &old, "full"),
            Some("true".into()),
            0,
            1,
        );
        store.set(
            &sync::profile_domain(DOMAIN),
            &legacy_flag_key(0, &old, "automatic"),
            Some("false".into()),
            0,
            1,
        );
        store_connection(&mut store, 0, &connection(0, "account-b", "new-account"), 1).unwrap();
        let returning = connection(0, "account-a", "reconnected");
        assert_eq!(
            store_connection(&mut store, 0, &returning, 1).unwrap(),
            Preferences {
                full: true,
                automatic: false
            }
        );
        store.set(
            &sync::profile_domain(DOMAIN),
            &flag_key(0, &returning, "full"),
            Some("false".into()),
            0,
            2,
        );
        assert!(
            !migrate_preferences(&mut store, 0, &old, 1).unwrap().full,
            "an old session snapshot cannot replace a newer account preference"
        );
    }
}
fn publish(bridge: &Bridge, action: impl FnOnce(AppWindow) + Send + 'static) {
    let weak = bridge.app.clone();
    let _ = profiles::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            action(app);
        }
    });
}

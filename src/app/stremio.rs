//! Account-scoped Stremio worker. HTTP and reconciliation run off the UI thread;
//! materialization is prepared off-thread and saved by the ordered writer;
//! only small checked state patches and visible models touch the event loop.
use super::*;
use nova_stremio::{
    ApiError, Canonical, Client, HttpsTransport, LibraryItem, Projection, Resume, Secret, Stamped,
};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Instant;

pub(super) const DOMAIN: &str = "stremio";
const CONNECTION: &str = "connection";
const MAX_TITLES: usize = 20_000;
const MAPPING_VERSION: u32 = 3;

#[path = "stremio_apply.rs"]
mod materialize;
#[path = "stremio_selection.rs"]
mod selection;
use selection::{Selection, SyncMode};

#[cfg(test)]
#[path = "stremio_ingress_tests.rs"]
mod ingress_tests;
#[cfg(test)]
#[path = "stremio_session_tests.rs"]
mod session_tests;
#[cfg(test)]
#[path = "stremio_tests.rs"]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
struct Connection {
    version: u32,
    account_id: String,
    email: String,
    auth_key: Secret,
    #[serde(default)]
    epoch: String,
    automatic: bool,
    started: bool,
    bootstrap_done: bool,
    #[serde(default)]
    mode: SyncMode,
}
impl Connection {
    fn valid(&self) -> bool {
        self.version == 1
            && !self.account_id.is_empty()
            && self.account_id.len() <= 512
            && !self.auth_key.expose().is_empty()
            && self.auth_key.expose().len() <= 4096
            && self.epoch.len() <= 256
    }
    fn same_session(&self, other: &Self) -> bool {
        self.account_id == other.account_id
            && self.auth_key.expose() == other.auth_key.expose()
            && self.epoch == other.epoch
    }
}

#[derive(Clone, Copy)]
enum SessionFlag {
    Automatic(bool),
    Started,
    Bootstrap,
    Mode(SyncMode),
}
fn session_key(connection: &Connection, field: &str) -> String {
    format!(
        "session:{}:{field}",
        serde_json::to_string(&connection.epoch).expect("string epoch")
    )
}
fn mode_key(connection: &Connection) -> String {
    format!(
        "account:{}:mode",
        serde_json::to_string(&connection.account_id).expect("account ID")
    )
}
fn migrate_mode(
    store: &mut nova_sync::Store,
    connection: &Connection,
    device: u64,
) -> Result<SyncMode, ApiError> {
    let domain = crate::app::sync::profile_domain(DOMAIN);
    if let Some(raw) = store
        .record(&domain, &mode_key(connection))
        .and_then(|row| row.value.as_ref())
    {
        return serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse);
    }
    let mode = store
        .record(&domain, &session_key(connection, "mode"))
        .and_then(|row| row.value.as_ref())
        .map(|raw| serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse))
        .transpose()?
        .unwrap_or(connection.mode);
    store.set(
        &domain,
        &mode_key(connection),
        Some(serde_json::to_string(&mode).map_err(|_| ApiError::InvalidInput)?),
        0,
        device,
    );
    Ok(mode)
}
fn write_connection_record(
    store: &mut nova_sync::Store,
    connection: Option<&Connection>,
    device: u64,
) -> Result<(), ApiError> {
    let domain = crate::app::sync::profile_domain(DOMAIN);
    let raw = connection
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| ApiError::InvalidInput)?;
    // The active credential is the only pointer to an older session's mode.
    // Preserve that account choice before a reconnect or disconnect removes it.
    let previous = store
        .record(&domain, CONNECTION)
        .and_then(|row| row.value.as_ref())
        .and_then(|raw| serde_json::from_str::<Connection>(raw).ok())
        .filter(Connection::valid);
    if let Some(previous) = previous {
        migrate_mode(store, &previous, device)?;
    }
    if let Some(connection) = connection {
        migrate_mode(store, connection, device)?;
    }
    store.set(&domain, CONNECTION, raw, 0, device);
    Ok(())
}
fn session_flags(store: &nova_sync::Store, connection: &mut Connection) -> Result<(), ApiError> {
    let boolean = |field: &str| {
        store
            .record(
                &crate::app::sync::profile_domain(DOMAIN),
                &session_key(connection, field),
            )
            .and_then(|r| r.value.as_ref())
            .map(|raw| serde_json::from_str::<bool>(raw).map_err(|_| ApiError::InvalidResponse))
            .transpose()
    };
    let automatic = boolean("automatic")?;
    let started = boolean("started")?;
    let bootstrap = boolean("bootstrap")?;
    connection.automatic = automatic.unwrap_or(connection.automatic);
    // Every writer stores only true for these two markers. Preference writes
    // cannot reset initial merge completion on an offline device's snapshot.
    connection.started |= started.unwrap_or(false);
    connection.bootstrap_done |= bootstrap.unwrap_or(false);
    let domain = crate::app::sync::profile_domain(DOMAIN);
    if let Some(raw) = store
        .record(&domain, &mode_key(connection))
        .and_then(|row| row.value.as_ref())
        .or_else(|| {
            store
                .record(&domain, &session_key(connection, "mode"))
                .and_then(|row| row.value.as_ref())
        })
    {
        connection.mode = serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse)?;
    }
    Ok(())
}
fn write_session_flag(
    store: &mut nova_sync::Store,
    expected: &Connection,
    flag: SessionFlag,
    device: u64,
) -> Result<Connection, ApiError> {
    let mut current = store
        .record(&crate::app::sync::profile_domain(DOMAIN), CONNECTION)
        .and_then(|r| r.value.as_ref())
        .map(|raw| serde_json::from_str::<Connection>(raw).map_err(|_| ApiError::InvalidResponse))
        .transpose()?
        .filter(|c| c.valid() && c.same_session(expected))
        .ok_or(ApiError::Authentication)?;
    let (name, value) = match flag {
        SessionFlag::Automatic(value) => ("automatic", value.to_string()),
        SessionFlag::Started => ("started", "true".into()),
        SessionFlag::Bootstrap => ("bootstrap", "true".into()),
        SessionFlag::Mode(value) => (
            "mode",
            serde_json::to_string(&value).map_err(|_| ApiError::InvalidInput)?,
        ),
    };
    let key = if matches!(flag, SessionFlag::Mode(_)) {
        mode_key(&current)
    } else {
        session_key(&current, name)
    };
    store.set(
        &crate::app::sync::profile_domain(DOMAIN),
        &key,
        Some(value),
        0,
        device,
    );
    session_flags(store, &mut current)?;
    Ok(current)
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Mapping {
    #[serde(default)]
    version: u32,
    media_type: String,
    id: String,
    videos: Vec<nova_stremio::Video>,
    episode_ids: BTreeMap<String, String>,
    checked_at: u64,
    #[serde(default)]
    inventory_revision: u64,
    #[serde(default)]
    descriptor_revision: u64,
    #[serde(default)]
    local_inventory_revision: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct NativeBinding {
    media_type: String,
    id: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Title {
    version: u32,
    item: LibraryItem,
    mapping: Mapping,
    local: Canonical,
    remote: Canonical,
    desired: Canonical,
    pending: bool,
    /// Accepted upload of this exact serialized item; verification can resume
    /// later without discarding a success returned after a profile switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted_upload: Option<String>,
    #[serde(default)]
    progress_merged: bool,
    #[serde(default)]
    binding: Option<NativeBinding>,
    #[serde(default)]
    selection: Option<Selection>,
}
impl Title {
    fn new(item: LibraryItem) -> Self {
        Self {
            version: 1,
            mapping: Mapping {
                media_type: item.media_type().into(),
                id: item.id().into(),
                ..Default::default()
            },
            item,
            local: Canonical::default(),
            remote: Canonical::default(),
            desired: Canonical::default(),
            pending: false,
            accepted_upload: None,
            progress_merged: false,
            binding: None,
            selection: None,
        }
    }
}

struct PendingTitle {
    name: String,
    media_type: String,
    id: String,
    reason: stremio_metadata::PendingReason,
}
impl PendingTitle {
    fn new(
        name: &str,
        media_type: &str,
        id: &str,
        reason: stremio_metadata::PendingReason,
    ) -> Self {
        Self {
            name: name.into(),
            media_type: media_type.into(),
            id: id.into(),
            reason,
        }
    }
}

enum Command {
    Connect(u64),
    Cancel(u64),
    Start(u64),
    Sync(u64),
    Automatic(bool, u64),
    Mode(SyncMode, u64),
    Review(selection::Request, u64),
    Disconnect(u64),
    OpenLink,
    Shutdown,
}
pub(super) struct Handle {
    finished: Arc<AtomicBool>,
    tx: SyncSender<Command>,
    rx: Mutex<Option<Receiver<Command>>>,
    generation: AtomicU64,
    review_generation: AtomicU64,
    urgent: Mutex<Option<Command>>,
    mode_change: Mutex<Option<Command>>,
    selection_change: Mutex<Option<Command>>,
    wake: AtomicBool,
    checkpoint: AtomicBool,
    pub(super) surfaces_dirty: AtomicBool,
}
impl Handle {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        Self {
            finished: Arc::new(AtomicBool::new(true)),
            tx,
            rx: Mutex::new(Some(rx)),
            generation: AtomicU64::new(0),
            review_generation: AtomicU64::new(0),
            urgent: Mutex::new(None),
            mode_change: Mutex::new(None),
            selection_change: Mutex::new(None),
            wake: AtomicBool::new(false),
            checkpoint: AtomicBool::new(false),
            surfaces_dirty: AtomicBool::new(false),
        }
    }
    fn send(&self, command: Command) {
        // Consent changes must remain deliverable while HTTP blocks the actor.
        // Only the latest choice for this single open review can be current.
        if matches!(&command, Command::Mode(_, _)) {
            *self.mode_change.lock().unwrap() = Some(command);
            return;
        }
        if matches!(
            &command,
            Command::Review(
                selection::Request::Confirm(_, _) | selection::Request::Unlink(_, _),
                _
            )
        ) {
            *self.selection_change.lock().unwrap() = Some(command);
            return;
        }
        if matches!(
            &command,
            Command::Connect(_) | Command::Cancel(_) | Command::Disconnect(_) | Command::Shutdown
        ) {
            // Cancellation must remain deliverable while a slow HTTP call has
            // filled the ordinary command queue. Generations invalidate any
            // older control action replaced in this single priority slot.
            *self.urgent.lock().unwrap() = Some(command);
            return;
        }
        if self.tx.try_send(command).is_err() {
            tracing::warn!("Stremio command queue unavailable");
        }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl Bridge {
    pub(super) fn start_stremio(&self) {
        if !self.profile_services_can_start() {
            return;
        }
        let handle = self.stremio();
        let Some(rx) = handle.rx.lock().unwrap().take() else {
            return;
        };
        handle.finished.store(false, Ordering::Release);
        let done = profile_actions::WorkerDone(handle.finished.clone());
        let bridge = self.clone();
        thread::spawn(move || {
            let _done = done;
            match HttpsTransport::new() {
                Ok(transport) => Worker::new(bridge, Client::new(transport)).run(rx),
                Err(error) => publish_error(&bridge, error, "Connecting to Stremio…"),
            }
        });
    }
    pub(super) fn stremio_connect(&self) {
        let generation = self.stremio().generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.stremio().send(Command::Connect(generation));
    }
    pub(super) fn stremio_cancel(&self) {
        let generation = self.stremio().generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.stremio().send(Command::Cancel(generation));
    }
    pub(super) fn stremio_start(&self) {
        self.stremio().send(Command::Start(
            self.stremio().generation.load(Ordering::Acquire),
        ));
    }
    pub(super) fn stremio_sync(&self) {
        self.stremio().send(Command::Sync(
            self.stremio().generation.load(Ordering::Acquire),
        ));
    }
    pub(super) fn stremio_automatic(&self, enabled: bool) {
        self.stremio().send(Command::Automatic(
            enabled,
            self.stremio().generation.load(Ordering::Acquire),
        ));
    }
    pub(super) fn stremio_disconnect(&self) {
        let generation = self.stremio().generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.stremio().send(Command::Disconnect(generation));
    }
    pub(super) fn stremio_open_link(&self) {
        self.stremio().send(Command::OpenLink);
    }
    pub(super) fn stremio_wake(&self) {
        self.stremio().wake.store(true, Ordering::Release);
    }
    pub(super) fn stremio_checkpoint(&self) {
        self.stremio().checkpoint.store(true, Ordering::Release);
    }
    pub(super) fn shutdown_stremio(&self) {
        self.stremio().generation.fetch_add(1, Ordering::AcqRel);
        self.stremio().send(Command::Shutdown);
    }
}

struct Login {
    code: Secret,
    link: String,
    generation: u64,
    expires: Instant,
    poll_after: Instant,
    _background: Box<dyn Send>,
}
struct Worker<T: nova_stremio::Transport> {
    bridge: Bridge,
    owner: profiles::ProfileStore,
    client: Client<T>,
    connection: Option<Connection>,
    connection_raw: Option<Secret>,
    verified: bool,
    auth_blocked: bool,
    login: Option<Login>,
    titles: BTreeMap<String, Title>,
    remote_items: BTreeMap<String, LibraryItem>,
    descriptors: Vec<serde_json::Value>,
    pending: BTreeMap<String, PendingTitle>,
    hydrate_after: BTreeMap<(String, String), (Instant, u64)>,
    retry_at: Instant,
    retry_secs: u64,
    next_sync: Instant,
    next_local_scan: Instant,
    dirty_since: Option<Instant>,
    last_position_upload: Instant,
    last_foreground: bool,
    force: bool,
    preview_ready: bool,
    phase: &'static str,
    applied: std::collections::BTreeSet<(String, String)>,
    review: Option<selection::Review>,
}
impl<T: nova_stremio::Transport> Worker<T> {
    fn new(bridge: Bridge, client: Client<T>) -> Self {
        let now = Instant::now();
        Self {
            owner: profiles::ProfileStore::capture(),
            bridge,
            client,
            connection: None,
            connection_raw: None,
            verified: false,
            auth_blocked: false,
            login: None,
            titles: BTreeMap::new(),
            remote_items: BTreeMap::new(),
            descriptors: Vec::new(),
            pending: BTreeMap::new(),
            hydrate_after: BTreeMap::new(),
            retry_at: now,
            retry_secs: 5,
            next_sync: now,
            next_local_scan: now,
            dirty_since: None,
            last_position_upload: now.checked_sub(Duration::from_secs(30)).unwrap_or(now),
            last_foreground: true,
            force: false,
            preview_ready: false,
            phase: "",
            applied: Default::default(),
            review: None,
        }
    }
    fn run(mut self, rx: Receiver<Command>) {
        loop {
            if !profiles::is_current(&profiles::capture()) {
                return;
            }
            let urgent = self.bridge.stremio().urgent.lock().unwrap().take();
            if let Some(command) = urgent {
                if matches!(command, Command::Shutdown) {
                    return;
                }
                self.command(command);
            }
            let mode = self.bridge.stremio().mode_change.lock().unwrap().take();
            if let Some(command) = mode {
                self.command(command);
            }
            let selection = self
                .bridge
                .stremio()
                .selection_change
                .lock()
                .unwrap()
                .take();
            if let Some(command) = selection {
                self.command(command);
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(command) => self.command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Err(error) = self.tick() {
                self.failed(error);
            }
        }
    }
    fn command(&mut self, command: Command) {
        if !profiles::is_current(&profiles::capture()) {
            return;
        }
        match command {
            Command::Connect(generation) => {
                if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
                    return;
                }
                self.login = None;
                self.phase = "Connecting to Stremio…";
                ui(&self.bridge, |app| {
                    app.set_stremio_busy(true);
                    app.set_stremio_status(text::tr("Connecting to Stremio…").into());
                });
                let result = (|| {
                    let background =
                        tracking::sign_in_background().map_err(|_| ApiError::Offline)?;
                    let code = self.client.create_link(now_secs())?;
                    if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let link = code.link.clone();
                    let display_code = code.code.expose().to_owned();
                    self.login = Some(Login {
                        code: code.code.clone(),
                        link: link.clone(),
                        generation,
                        expires: Instant::now() + Duration::from_secs(300),
                        poll_after: Instant::now(),
                        _background: background,
                    });
                    ui(&self.bridge, move |app| {
                        app.set_stremio_link_code(display_code.into());
                        app.set_stremio_link_url(link.into());
                        app.set_stremio_status(text::tr("Finish sign-in in your browser").into());
                    });
                    let _ = crate::player::open_browser(&self.login.as_ref().unwrap().link);
                    Ok(())
                })();
                if let Err(error) = result {
                    self.failed(error);
                }
            }
            Command::Cancel(generation) => {
                if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
                    return;
                }
                self.login = None;
                ui(&self.bridge, |app| {
                    app.set_stremio_busy(false);
                    app.set_stremio_link_code("".into());
                    app.set_stremio_link_url("".into());
                    app.set_stremio_status(text::tr("Stremio sign-in cancelled").into());
                });
            }
            Command::OpenLink => {
                if let Some(login) = &self.login
                    && login.generation == self.bridge.stremio().generation.load(Ordering::Acquire)
                {
                    let _ = crate::player::open_browser(&login.link);
                }
            }
            Command::Start(generation)
                if generation == self.bridge.stremio().generation.load(Ordering::Acquire)
                    && self.preview_ready
                    && self.verified =>
            {
                if let Some(connection) = self.connection.clone() {
                    self.phase = "Saving local changes…";
                    if self.save_flag(&connection, SessionFlag::Started).is_ok() {
                        self.preview_ready = false;
                        self.publish_connection();
                        self.force = true;
                    } else {
                        self.storage_failed();
                    }
                }
            }
            Command::Sync(generation)
                if generation == self.bridge.stremio().generation.load(Ordering::Acquire) =>
            {
                self.force = true;
                self.hydrate_after.clear();
                self.retry_at = Instant::now();
            }
            Command::Automatic(enabled, generation)
                if generation == self.bridge.stremio().generation.load(Ordering::Acquire) =>
            {
                if let Some(connection) = self.connection.clone() {
                    if self
                        .save_flag(&connection, SessionFlag::Automatic(enabled))
                        .is_ok()
                    {
                        self.publish_connection();
                    } else {
                        self.storage_failed();
                    }
                }
            }
            Command::Mode(mode, generation)
                if generation == self.bridge.stremio().generation.load(Ordering::Acquire) =>
            {
                if let Some(connection) = self.connection.clone() {
                    if self.save_flag(&connection, SessionFlag::Mode(mode)).is_ok() {
                        self.preview_ready = false;
                        self.pending.clear();
                        self.review = None;
                        self.force = false;
                        self.next_sync = Instant::now();
                        self.publish_connection();
                    } else {
                        self.storage_failed();
                    }
                }
            }
            Command::Review(request, generation) => {
                if let Err(error) = self.selection_request(request, generation) {
                    let review = self
                        .bridge
                        .stremio()
                        .review_generation
                        .load(Ordering::Acquire);
                    selection::review_error(&self.bridge, review, error);
                }
            }
            Command::Disconnect(generation) => {
                if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
                    return;
                }
                self.login = None;
                if self.write_connection(None).is_err() {
                    self.storage_failed();
                    return;
                }
                self.connection = None;
                self.verified = false;
                self.titles.clear();
                self.remote_items.clear();
                self.pending.clear();
                self.publish_connection();
            }
            _ => {}
        }
    }
    fn tick(&mut self) -> Result<(), ApiError> {
        if !profiles::is_current(&profiles::capture()) {
            return Err(ApiError::Rejected);
        }
        let generation = self.bridge.stremio().generation.load(Ordering::Acquire);
        if self.bridge.stremio().wake.swap(false, Ordering::AcqRel) {
            self.next_local_scan = Instant::now();
        }
        if self.login.is_some() {
            self.phase = "Connecting to Stremio…";
        }
        self.poll_login()?;
        self.phase = "Restoring Stremio connection…";
        self.restore_connection()?;
        if Instant::now() < self.retry_at {
            return Ok(());
        }
        if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(connection) = self.connection.clone().filter(|_| self.verified) else {
            return Ok(());
        };
        if self
            .bridge
            .stremio()
            .checkpoint
            .swap(false, Ordering::AcqRel)
            && connection.automatic
        {
            self.next_sync = Instant::now();
        }
        if self.login.is_some() {
            return Ok(());
        }
        if !connection.started {
            if !self.preview_ready {
                self.prepare_preview(&connection)?;
            }
            return Ok(());
        }
        let foreground = remote_playback::is_foreground();
        if foreground && !self.last_foreground {
            self.next_sync = Instant::now();
        }
        self.last_foreground = foreground;
        if !foreground && !self.force {
            return Ok(());
        }
        let mut changed = false;
        if Instant::now() >= self.next_local_scan {
            changed = self.local_changed(&connection);
            self.next_local_scan = Instant::now() + Duration::from_secs(2);
        }
        if changed && self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
        let dirty_due = self
            .dirty_since
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(5));
        if self.force || (connection.automatic && (Instant::now() >= self.next_sync || dirty_due)) {
            self.force = false;
            self.synchronize(&connection)?;
            self.dirty_since = None;
            self.next_sync = Instant::now() + Duration::from_secs(60);
            self.retry_secs = 5;
        }
        Ok(())
    }
    fn poll_login(&mut self) -> Result<(), ApiError> {
        let Some(login) = &mut self.login else {
            return Ok(());
        };
        if login.generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
            self.login = None;
            return Ok(());
        }
        if Instant::now() >= login.expires {
            self.login = None;
            ui(&self.bridge, |app| {
                app.set_stremio_busy(false);
                app.set_stremio_link_code("".into());
                app.set_stremio_link_url("".into());
                app.set_stremio_status(text::tr("Stremio sign-in expired. Connect again.").into());
            });
            return Ok(());
        }
        if Instant::now() < login.poll_after || Instant::now() < self.retry_at {
            return Ok(());
        }
        login.poll_after = Instant::now() + Duration::from_secs(3);
        let generation = login.generation;
        let Some(auth_key) = self.client.read_link(&login.code, now_secs())? else {
            return Ok(());
        };
        if !profiles::is_current(&profiles::capture())
            || generation != self.bridge.stremio().generation.load(Ordering::Acquire)
        {
            return Err(ApiError::Rejected);
        }
        let user = self.client.get_user(&auth_key, now_secs())?;
        if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
            self.login = None;
            return Ok(());
        }
        let mut connection = Connection {
            version: 1,
            account_id: user.id,
            email: user.email,
            auth_key,
            epoch: format!(
                "{}:{:016x}:{generation}",
                nova_config::now_ms(),
                nova_sync::local_device().map_err(|_| ApiError::Rejected)?
            ),
            automatic: true,
            started: false,
            bootstrap_done: false,
            mode: SyncMode::default(),
        };
        self.write_connection(Some(&connection))
            .map_err(|_| ApiError::Rejected)?;
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        session_flags(&owner.lock().unwrap(), &mut connection)?;
        self.login = None;
        self.connection = Some(connection);
        self.verified = true;
        self.auth_blocked = false;
        self.titles = load_titles(&self.connection.as_ref().unwrap().account_id)
            .map_err(|_| ApiError::Rejected)?;
        self.remote_items.clear();
        self.pending.clear();
        self.preview_ready = false;
        self.publish_connection();
        Ok(())
    }
    fn restore_connection(&mut self) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let (raw, loaded, basis) = {
            let mut store = owner.lock().unwrap();
            // A full digest walks every episode. Only ingress pending for this
            // domain needs a projection basis, never the ordinary 500ms tick.
            let basis = if store
                .pending_domains()
                .iter()
                .any(|domain| domain == &crate::app::sync::profile_domain(DOMAIN))
            {
                store.save().map_err(|_| ApiError::Rejected)?;
                store
                    .digest()
                    .get(&crate::app::sync::profile_domain(DOMAIN))
                    .cloned()
            } else {
                None
            };
            let raw = store
                .record(&crate::app::sync::profile_domain(DOMAIN), CONNECTION)
                .and_then(|r| r.value.clone());
            let mut loaded = raw
                .as_ref()
                .map(|value| {
                    serde_json::from_str::<Connection>(value).map_err(|_| ApiError::InvalidResponse)
                })
                .transpose()?;
            if let Some(connection) = loaded.as_mut() {
                if !connection.valid() {
                    return Err(ApiError::InvalidResponse);
                }
                if store
                    .record(
                        &crate::app::sync::profile_domain(DOMAIN),
                        &mode_key(connection),
                    )
                    .and_then(|row| row.value.as_ref())
                    .is_none()
                {
                    migrate_mode(
                        &mut store,
                        connection,
                        nova_sync::local_device().map_err(|_| ApiError::Rejected)?,
                    )?;
                    store.save().map_err(|_| ApiError::Rejected)?;
                    nova_sync::request_local_sync();
                }
                session_flags(&store, connection)?;
            }
            (raw, loaded, basis)
        };
        let raw_changed = raw.as_deref() != self.connection_raw.as_ref().map(Secret::expose);
        let flags_changed = loaded
            .as_ref()
            .zip(self.connection.as_ref())
            .is_some_and(|(a, b)| {
                a.automatic != b.automatic
                    || a.started != b.started
                    || a.bootstrap_done != b.bootstrap_done
                    || a.mode != b.mode
            });
        if raw_changed || flags_changed {
            let same_account = loaded
                .as_ref()
                .zip(self.connection.as_ref())
                .is_some_and(|(a, b)| a.same_session(b));
            if !same_account {
                self.bridge
                    .stremio()
                    .generation
                    .fetch_add(1, Ordering::AcqRel);
                self.verified = false;
                self.auth_blocked = false;
                self.titles.clear();
                self.remote_items.clear();
                self.pending.clear();
                self.preview_ready = false;
                self.review = None;
            }
            self.connection = loaded;
            self.connection_raw = raw.map(Secret::new);
            if let Some(connection) = self.connection.clone()
                && !self.verified
                && !self.auth_blocked
                && Instant::now() >= self.retry_at
            {
                let user = self.client.get_user(&connection.auth_key, now_secs())?;
                if user.id != connection.account_id {
                    return Err(ApiError::Authentication);
                }
                self.check_current(
                    &connection,
                    self.bridge.stremio().generation.load(Ordering::Acquire),
                )?;
                self.titles =
                    load_titles(&connection.account_id).map_err(|_| ApiError::Rejected)?;
                self.verified = true;
                self.next_sync = Instant::now();
            }
            self.publish_connection();
        } else if self.connection.is_some()
            && !self.verified
            && !self.auth_blocked
            && Instant::now() >= self.retry_at
        {
            // Offline restoration retains the saved token but must verify it
            // before any retained write can be delivered.
            self.connection_raw = None;
            return self.restore_connection();
        }
        if let Some(basis) = basis {
            let mut store = owner.lock().unwrap();
            store
                .mark_projected(&crate::app::sync::profile_domain(DOMAIN), &basis)
                .map_err(|_| ApiError::Rejected)?;
            store.save().map_err(|_| ApiError::Rejected)?;
        }
        Ok(())
    }
    fn save_flag(&mut self, connection: &Connection, flag: SessionFlag) -> Result<(), ApiError> {
        if !profiles::is_current(&profiles::capture()) {
            return Err(ApiError::Rejected);
        }
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let effective = write_session_flag(&mut store, connection, flag, device)?;
        store.save().map_err(|_| ApiError::Rejected)?;
        self.connection = Some(effective);
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn write_connection(&mut self, connection: Option<&Connection>) -> Result<(), ()> {
        if !profiles::is_current(&profiles::capture()) {
            return Err(());
        }
        let owner = nova_sync::local_store().map_err(|_| ())?;
        let dev = nova_sync::local_device().map_err(|_| ())?;
        let raw = connection
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| ())?;
        let mut store = owner.lock().unwrap();
        write_connection_record(&mut store, connection, dev).map_err(|_| ())?;
        store.save().map_err(|_| ())?;
        self.connection_raw = raw.map(Secret::new);
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn publish_connection(&self) {
        let connection = self.connection.clone();
        let verified = self.verified;
        let preview_ready = self.preview_ready;
        ui(&self.bridge, move |app| {
            app.set_stremio_connected(connection.is_some() && verified);
            app.set_stremio_busy(false);
            app.set_stremio_link_code("".into());
            app.set_stremio_link_url("".into());
            app.set_stremio_account(
                connection
                    .as_ref()
                    .map(|c| c.email.clone())
                    .unwrap_or_default()
                    .into(),
            );
            app.set_stremio_automatic(connection.as_ref().is_some_and(|c| c.automatic));
            app.set_stremio_mode_index(connection.as_ref().map_or(0, |c| c.mode.index()));
            if connection.as_ref().is_some_and(|c| !c.started) {
                if !preview_ready {
                    app.set_stremio_preview(text::tr("Preparing library merge…").into());
                }
            } else {
                app.set_stremio_preview("".into());
            }
            app.set_stremio_merge_ready(preview_ready);
            if connection.is_none() {
                app.set_stremio_pending("".into());
                app.set_stremio_status(text::tr("Stremio is disconnected").into());
            }
        });
    }
    fn prepare_preview(&mut self, connection: &Connection) -> Result<(), ApiError> {
        let generation = self.bridge.stremio().generation.load(Ordering::Acquire);
        self.check_current(connection, generation)?;
        if connection.mode == SyncMode::SelectedTitles {
            self.preview_ready = true;
            ui(&self.bridge, |app| {
                app.set_stremio_busy(false);
                app.set_stremio_merge_ready(true);
                app.set_stremio_preview(text::tr("Only explicitly enabled titles will sync. Enable a title from its Tracking page.").into());
                app.set_stremio_status(text::tr("Selected titles mode").into());
            });
            return Ok(());
        }
        ui(&self.bridge, |app| app.set_stremio_busy(true));
        self.set_phase("Reading Stremio addons…");
        self.descriptors = self
            .client
            .addon_collection(&connection.auth_key, now_secs())?;
        self.check_current(connection, generation)?;
        self.set_phase("Reading Stremio library…");
        let items = self
            .client
            .library_get(&connection.auth_key, &[], true, now_secs())?;
        if items.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        self.remote_items = items
            .into_iter()
            .map(|item| (item.id().into(), item))
            .collect();
        let entries = self
            .bridge
            .personal_state()
            .ok_or(ApiError::Rejected)?
            .entries
            .clone();
        let count = entries.len()
            + self
                .remote_items
                .values()
                .filter(|item| {
                    item.0.get("removed").and_then(|v| v.as_bool()) == Some(false)
                        && item.0.get("temp").and_then(|v| v.as_bool()) != Some(true)
                        && !entries
                            .iter()
                            .any(|e| e.id == item.id() && e.type_ == item.media_type())
                })
                .count();
        self.preview_ready = true;
        self.phase = "";
        ui(&self.bridge, move |app| {
            app.set_stremio_busy(false);
            app.set_stremio_preview(text::stremio_preview(count).into());
            app.set_stremio_merge_ready(true);
            app.set_stremio_status(text::tr("Ready to merge both libraries").into());
        });
        Ok(())
    }
    fn local_changed(&self, connection: &Connection) -> bool {
        let selections = nova_sync::local_store()
            .ok()
            .and_then(|owner| selection::load(&owner.lock().unwrap(), connection).ok())
            .unwrap_or_default();
        let (entries, progress) = {
            let Some(state) = self.bridge.personal_state() else {
                return false;
            };
            (state.entries.clone(), state.progress.clone())
        };
        (connection.mode == SyncMode::FullLibrary
            && entries.iter().any(|e| {
                !self.pending.contains_key(&e.id)
                    && !self
                        .titles
                        .values()
                        .any(|t| t.mapping.id == e.id && t.mapping.media_type == e.type_)
            }))
            || (connection.mode == SyncMode::FullLibrary
                && progress.values().any(|p| {
                    p.history.as_ref().is_some_and(|history| {
                        (p.watched || p.position_secs > 0.0)
                            && !self.pending.contains_key(&p.series_id)
                            && !self.titles.values().any(|title| {
                                title.mapping.id == p.series_id
                                    && title.mapping.media_type == history.type_
                            })
                    })
                }))
            || self
                .titles
                .values()
                .filter(|title| selection::allowed(connection.mode, &selections, title))
                .any(|title| {
                    let local = local_projection(&title.mapping, &entries, &progress);
                    let source = source_canonical(connection, title, &local, &entries, &progress);
                    let shared = read_shared_for_title(connection, title);
                    // Mesh field records can arrive independently of the native
                    // projection; Wake must notice them even if the UI is equal.
                    let shared_changed = shared.as_ref().is_ok_and(|value| {
                        nova_stremio::reconcile(&title.desired, value, false) != title.desired
                    });
                    let changed = source.as_ref().is_ok_and(|value| value != &title.local)
                        || source.is_err()
                        || shared_changed
                        || title.pending;
                    let only_position = source.as_ref().is_ok_and(|value| {
                        value.saved == title.local.saved
                            && value.watched == title.local.watched
                            && value.resume.value.is_some()
                    });
                    changed
                        && (shared_changed
                            || title.pending
                            || !only_position
                            || self.last_position_upload.elapsed() >= Duration::from_secs(30))
                })
            || (!connection.bootstrap_done)
    }
    fn synchronize(&mut self, connection: &Connection) -> Result<(), ApiError> {
        let generation = self.bridge.stremio().generation.load(Ordering::Acquire);
        self.check_current(connection, generation)?;
        ui(&self.bridge, |app| {
            app.set_stremio_busy(true);
            app.set_stremio_status(text::tr("Syncing with Stremio…").into());
        });
        self.set_phase("Saving local changes…");
        persistence::flush().map_err(|_| ApiError::Rejected)?;
        self.set_phase("Reading Stremio addons…");
        self.descriptors = self
            .client
            .addon_collection(&connection.auth_key, now_secs())?;
        self.check_current(connection, generation)?;
        let descriptor_revision = nova_config::fnv1a(
            &serde_json::to_vec(&self.descriptors).map_err(|_| ApiError::InvalidResponse)?,
        );
        let selections = {
            let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
            selection::load(&owner.lock().unwrap(), connection)?
        };
        self.titles.retain(|_, title| {
            title
                .selection
                .as_ref()
                .is_none_or(|s| selections.contains(s))
        });
        self.set_phase("Reading Stremio library…");
        let meta = self.client.library_meta(&connection.auth_key, now_secs())?;
        if meta.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        let fetch_ids: Vec<_> = meta
            .iter()
            .filter(|(id, _)| {
                connection.mode == SyncMode::FullLibrary
                    || selections.iter().any(|s| s.remote_id == *id)
            })
            .filter(|(id, stamp)| {
                self.remote_items
                    .get(id)
                    .is_none_or(|item| item.modified_ms() != *stamp)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for ids in fetch_ids.chunks(100) {
            self.check_current(connection, generation)?;
            for item in self
                .client
                .library_get(&connection.auth_key, ids, false, now_secs())?
            {
                self.remote_items.insert(item.id().into(), item);
            }
        }
        let (entries, progress, native_sources) = {
            let state = self.bridge.personal_state().ok_or(ApiError::Rejected)?;
            (
                state.entries.clone(),
                state.progress.clone(),
                state
                    .installed
                    .iter()
                    .filter(|source| source.enabled && source.available)
                    .map(|source| nova_providers::MetadataAddon {
                        url: source.url.clone(),
                        manifest: source.manifest.clone(),
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let mut locals = local_titles(&entries, &progress);
        if connection.mode == SyncMode::SelectedTitles {
            locals.retain(|local| {
                selections
                    .iter()
                    .any(|s| s.native_id == local.id && s.native_type == local.media_type)
            });
        }
        // A removed title can still own an explicit unwatch/removal event.
        // Keep its binding's cached identity evidence available for delivery.
        for title in self.titles.values() {
            if !selection::allowed(connection.mode, &selections, title) {
                continue;
            }
            if let Some(binding) = &title.binding
                && !locals
                    .iter()
                    .any(|local| local.id == binding.id && local.media_type == binding.media_type)
            {
                locals.push(stremio_metadata::LocalTitle {
                    id: binding.id.clone(),
                    media_type: binding.media_type.clone(),
                    videos: read_episodes_cache_for(&binding.media_type, &binding.id)
                        .unwrap_or_default(),
                });
            }
        }
        // A selected title need not have local history yet.
        for selected in selections.iter().filter(|s| s.enabled) {
            if !locals.iter().any(|local| {
                local.id == selected.native_id && local.media_type == selected.native_type
            }) {
                locals.push(stremio_metadata::LocalTitle {
                    id: selected.native_id.clone(),
                    media_type: selected.native_type.clone(),
                    videos: read_episodes_cache_for(&selected.native_type, &selected.native_id)
                        .unwrap_or_default(),
                });
            }
        }
        if locals.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        let request_context = profiles::capture();
        let request_handle = self.bridge.stremio();
        let fetch = |url: &str, timeout, limit| {
            if !profiles::is_current(&request_context)
                || request_handle.generation.load(Ordering::Acquire) != generation
            {
                return Err(());
            }
            net::fetch_metadata_bytes(url, timeout, limit).map_err(|_| ())
        };
        self.pending.clear();
        for local in &mut locals {
            if stremio_metadata::outbound_identity(local, &self.descriptors).is_ok() {
                continue;
            }
            let key = (local.media_type.clone(), local.id.clone());
            let revision = nova_providers::metadata_revision();
            if self
                .hydrate_after
                .get(&key)
                .is_some_and(|(after, known)| *known == revision && Instant::now() < *after)
            {
                continue;
            }
            self.check_current(connection, generation)?;
            self.set_phase("Resolving Stremio episode metadata…");
            self.hydrate_after
                .insert(key, (Instant::now() + Duration::from_secs(300), revision));
            if let Ok(metadata) = stremio_metadata::hydrate_local(local, &native_sources, &fetch) {
                local.videos =
                    write_episode_meta_cache_for(&local.media_type, &local.id, &metadata);
                merge_meta_header_for(
                    &local.media_type,
                    &local.id,
                    &meta_header_from_item(&metadata),
                );
            }
        }
        for item in self.remote_items.values() {
            if connection.mode == SyncMode::SelectedTitles {
                continue;
            }
            self.titles
                .entry(item.id().into())
                .or_insert_with(|| Title::new(item.clone()));
        }
        for selected in selections.iter().filter(|s| s.enabled) {
            let Some(local) = locals
                .iter()
                .find(|l| l.id == selected.native_id && l.media_type == selected.native_type)
            else {
                continue;
            };
            let item = self
                .remote_items
                .get(&selected.remote_id)
                .cloned()
                .unwrap_or_else(|| {
                    LibraryItem::new(
                        &selected.remote_id,
                        &selected.name,
                        &selected.remote_type,
                        nova_config::now_ms(),
                    )
                });
            let mut title = Title::new(item);
            title.binding = Some(NativeBinding {
                media_type: selected.native_type.clone(),
                id: selected.native_id.clone(),
            });
            title.selection = Some(selected.clone());
            let slot = title_slot(&title);
            if self
                .titles
                .get(&slot)
                .is_some_and(|t| t.selection.as_ref() == Some(selected))
            {
                continue;
            }
            match selection::resolve(selected, local, &title.item, &self.descriptors, &fetch) {
                Ok(resolved) => {
                    title.mapping = mapping(resolved, descriptor_revision);
                    self.titles.insert(slot, title);
                }
                Err(reason) => {
                    self.pending.insert(
                        slot,
                        PendingTitle::new(
                            &selected.name,
                            &selected.native_type,
                            &selected.native_id,
                            reason,
                        ),
                    );
                }
            }
        }
        for local in &locals {
            self.check_current(connection, generation)?;
            if connection.mode == SyncMode::SelectedTitles
                || selections
                    .iter()
                    .any(|s| s.native_id == local.id && s.native_type == local.media_type)
            {
                continue;
            }
            if self
                .titles
                .values()
                .any(|t| t.mapping.id == local.id && t.mapping.media_type == local.media_type)
            {
                continue;
            }
            let entry = entries
                .iter()
                .find(|entry| entry.id == local.id && entry.type_ == local.media_type);
            let history = progress
                .values()
                .filter(|p| p.series_id == local.id)
                .filter_map(|p| {
                    p.history
                        .as_ref()
                        .filter(|h| h.type_ == local.media_type)
                        .map(|h| (p, h))
                })
                .max_by_key(|(p, _)| p.updated_at_secs);
            if entry.is_none()
                && !progress
                    .values()
                    .any(|p| p.series_id == local.id && (p.watched || p.position_secs > 0.0))
            {
                continue;
            }
            let name = entry
                .map(|e| e.name.as_str())
                .or_else(|| history.map(|(_, h)| h.title.as_str()))
                .unwrap_or(&local.id);
            self.set_phase("Resolving Stremio episode metadata…");
            match stremio_metadata::outbound(
                local,
                name,
                nova_config::now_ms(),
                &self.descriptors,
                &fetch,
            ) {
                Ok((mut item, resolved)) => {
                    if entry.is_none() {
                        item.apply_membership(false, item.modified_ms());
                        item.0.insert("temp".into(), serde_json::json!(true));
                    }
                    let mut title = Title::new(item);
                    title.mapping = mapping(resolved, descriptor_revision);
                    if local.id != title.item.id() || local.media_type != title.item.media_type() {
                        title.binding = Some(NativeBinding {
                            id: local.id.clone(),
                            media_type: local.media_type.clone(),
                        });
                    }
                    // Each proven native representation keeps independent
                    // baselines while sharing the remote title's field clocks.
                    let slot = title_slot(&title);
                    if self.titles.get(&slot).is_some_and(|known| {
                        known.item.id() != title.item.id()
                            || known.mapping.id != title.mapping.id
                            || known.mapping.media_type != title.mapping.media_type
                    }) {
                        self.pending.insert(
                            local.id.clone(),
                            PendingTitle::new(
                                name,
                                &local.media_type,
                                &local.id,
                                stremio_metadata::PendingReason::RemoteTitleCollision,
                            ),
                        );
                    } else {
                        self.titles.entry(slot).or_insert(title);
                    }
                }
                Err(reason) => {
                    self.pending.insert(
                        local.id.clone(),
                        PendingTitle::new(name, &local.media_type, &local.id, reason),
                    );
                }
            }
        }
        let ids: Vec<_> = self.titles.keys().cloned().collect();
        if ids.len() > MAX_TITLES {
            return Err(ApiError::InvalidResponse);
        }
        for slot in ids {
            self.check_current(connection, generation)?;
            let mut title = self.titles.get(&slot).unwrap().clone();
            if !selection::allowed(connection.mode, &selections, &title) {
                continue;
            }
            let id = title.item.id().to_owned();
            let pending_export = title
                .pending
                .then(|| (title.item.modified_ms(), title.desired.clone()));
            let remote_item = self.remote_items.get(&id).cloned();
            let mut remote_confirmed = remote_item.is_some();
            if let Some(item) = remote_item.as_ref() {
                title.item = item.clone();
            }
            let stale_metadata = title.mapping.version != MAPPING_VERSION
                || title.mapping.checked_at == 0
                || title.mapping.inventory_revision != nova_providers::metadata_revision()
                || title.mapping.descriptor_revision != descriptor_revision
                || title.mapping.local_inventory_revision
                    != stremio_metadata::local_inventory_revision(locals.iter().find(|local| {
                        local.id == title.mapping.id && local.media_type == title.mapping.media_type
                    }))
                || nova_config::now_ms().saturating_sub(title.mapping.checked_at)
                    >= 6 * 60 * 60 * 1000;
            let mut progress_available = true;
            if stale_metadata {
                self.set_phase("Resolving Stremio episode metadata…");
                let resolved = if let Some(selected) = &title.selection {
                    locals
                        .iter()
                        .find(|local| {
                            local.id == selected.native_id
                                && local.media_type == selected.native_type
                        })
                        .ok_or(stremio_metadata::PendingReason::UnsupportedIdentity)
                        .and_then(|local| {
                            selection::resolve(
                                selected,
                                local,
                                &title.item,
                                &self.descriptors,
                                &fetch,
                            )
                        })
                } else if let Some(binding) = &title.binding {
                    locals
                        .iter()
                        .find(|local| {
                            local.id == binding.id && local.media_type == binding.media_type
                        })
                        .ok_or(stremio_metadata::PendingReason::UnsupportedIdentity)
                        .and_then(|local| {
                            stremio_metadata::resolve_bound(
                                &title.item,
                                local,
                                &self.descriptors,
                                &fetch,
                            )
                        })
                } else {
                    // A canonical card never consumes a provider's binding.
                    // Sources/seasons are reconciled in their own slots below.
                    let exact: Vec<_> = locals
                        .iter()
                        .filter(|local| {
                            local.id == id && local.media_type == title.item.media_type()
                        })
                        .cloned()
                        .collect();
                    stremio_metadata::resolve(&title.item, &exact, &self.descriptors, &fetch)
                };
                match resolved {
                    Ok(resolved) => title.mapping = mapping(resolved, descriptor_revision),
                    Err(reason) => {
                        progress_available = false;
                        self.pending.insert(
                            slot.clone(),
                            PendingTitle::new(
                                title.item.name(),
                                &title.mapping.media_type,
                                &title.mapping.id,
                                reason,
                            ),
                        );
                        if title.binding.is_some()
                            || title.mapping.id != title.item.id()
                            || title.mapping.media_type != title.item.media_type()
                        {
                            // A revoked/unverifiable provider alias cannot
                            // authorize even a membership write to that source.
                            self.titles.insert(slot, title);
                            continue;
                        }
                    }
                }
            }
            // Metadata/network requests can outlive a local user action. Take
            // each title's native snapshot only after those requests complete.
            let (current_entries, current_progress) = {
                let state = self.bridge.personal_state().ok_or(ApiError::Rejected)?;
                (state.entries.clone(), state.progress.clone())
            };
            let local = local_projection(&title.mapping, &current_entries, &current_progress);
            let local_state = source_canonical(
                connection,
                &title,
                &local,
                &current_entries,
                &current_progress,
            )?;
            let shared = read_shared_for_title(connection, &title)?;
            let acknowledged = {
                let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                let store = owner.lock().unwrap();
                store
                    .record(
                        &crate::app::sync::profile_domain(DOMAIN),
                        &format!("{}remote", shared_prefix(&connection.account_id, &id)),
                    )
                    .and_then(|r| r.value.as_ref())
                    .map(|raw| {
                        serde_json::from_str::<(u64, Canonical)>(raw)
                            .map_err(|_| ApiError::InvalidResponse)
                    })
                    .transpose()?
            }
            .filter(|(stamp, _)| {
                remote_item
                    .as_ref()
                    .is_some_and(|item| item.modified_ms() == *stamp)
            })
            .or(pending_export);
            let mut remote_projection = if progress_available {
                match title.item.projection(&title.mapping.videos) {
                    Ok(projection) => projection,
                    Err(_) => {
                        progress_available = false;
                        self.pending.insert(
                            slot.clone(),
                            PendingTitle::new(
                                title.item.name(),
                                &title.mapping.media_type,
                                &title.mapping.id,
                                stremio_metadata::PendingReason::WatchedHistoryUnavailable,
                            ),
                        );
                        membership_projection(&title.item)
                    }
                }
            } else {
                membership_projection(&title.item)
            };
            if remote_item.is_none() {
                remote_projection = title.remote.projection();
            }
            let remote_stamp = remote_item.as_ref().map_or(0, LibraryItem::modified_ms);
            let mut remote = title.remote.clone();
            if remote_item.is_some() {
                remote.observe(&remote_projection, remote_stamp);
                if title.remote.saved.changed_at == 0 {
                    // An explicit remote tombstone is an event even though
                    // the absence baseline also represented false.
                    remote.saved = Stamped {
                        value: remote_projection.saved,
                        changed_at: remote_stamp,
                    };
                }
                // Matching values alone cannot distinguish our export from a
                // later independent Stremio action. Reuse original action
                // clocks only for an acknowledged exact server revision.
                if let Some((_, known)) = acknowledged.filter(|(stamp, _)| *stamp == remote_stamp) {
                    if known.watched.len() > nova_stremio::MAX_VIDEOS
                        || max_stamp(&known) > nova_config::now_ms().saturating_add(60 * 60 * 1000)
                    {
                        return Err(ApiError::InvalidResponse);
                    }
                    if remote_projection.saved == known.saved.value {
                        remote.saved = known.saved;
                    }
                    for (episode, value) in &remote_projection.watched {
                        if let Some(field) = known
                            .watched
                            .get(episode)
                            .filter(|field| field.value == *value)
                        {
                            remote.watched.insert(episode.clone(), field.clone());
                        }
                    }
                    if remote_projection.resume == known.resume.value {
                        remote.resume = known.resume;
                    }
                }
            }
            if !progress_available {
                // A missing episode list is not an instruction to clear either
                // side's watched bits or current resume pointer.
                remote.watched = title.remote.watched.clone();
                remote.resume = title.remote.resume.clone();
            }
            let initial = !connection.bootstrap_done || !title.progress_merged;
            // A newly discovered native binding still owns its initial history
            // union even when another representation already observed Stremio.
            let local_state = nova_stremio::reconcile(&shared, &local_state, initial);
            let mut desired = nova_stremio::reconcile(&local_state, &remote, initial);
            if connection.bootstrap_done && (title.binding.is_none() || title.progress_merged) {
                // Deferred episode metadata still gets its initial watched
                // union, while a later removal remains a removal immediately.
                desired.saved = nova_stremio::reconcile(&local_state, &remote, false).saved;
            }
            title.local = local_state;
            title.remote = remote;
            title.desired = desired;
            title.pending = if progress_available {
                title.desired.projection() != remote_projection
            } else {
                title.desired.saved.value != remote_projection.saved
            };
            self.set_phase("Saving merged library on this device…");
            self.check_current(connection, generation)?;
            save_title(connection, &mut title)?;
            self.check_current(connection, generation)?;
            // The canonical account row coordinates source bindings; it must
            // not create a second library/history identity beside those sources.
            // Existing canonical entries remain independent, including ones the
            // user deliberately saved alongside a season-specific release.
            if title.binding.is_some()
                || !stremio_metadata::represented_by_native(&title.item, &locals, &self.descriptors)
            {
                title.desired =
                    self.apply_local(connection, &title, progress_available, generation)?;
            }
            let projection = title.desired.projection();
            title.pending = if progress_available {
                projection != remote_projection
            } else {
                projection.saved != remote_projection.saved
            };
            if title.pending {
                // Read immediately before writing so unrelated Stremio fields
                // and edits made during the metadata request are preserved.
                self.set_phase("Reading Stremio library…");
                let latest = self
                    .client
                    .library_get(
                        &connection.auth_key,
                        std::slice::from_ref(&id),
                        false,
                        now_secs(),
                    )?
                    .into_iter()
                    .find(|item| item.id() == id);
                if let Some(latest) = latest {
                    if latest.modified_ms() != remote_stamp {
                        self.remote_items.insert(id.clone(), latest);
                        self.titles.insert(slot, title);
                        self.force = true;
                        continue;
                    }
                    title.item = latest;
                } else if remote_item.is_some() {
                    // A disappearing remote record is not permission to
                    // recreate an old library entry from a cached snapshot.
                    self.remote_items.remove(&id);
                    self.titles.insert(slot, title);
                    self.force = true;
                    continue;
                }
                let (entries, progress) = {
                    let state = self.bridge.personal_state().ok_or(ApiError::Rejected)?;
                    (state.entries.clone(), state.progress.clone())
                };
                let native = local_projection(&title.mapping, &entries, &progress);
                let source = source_canonical(connection, &title, &native, &entries, &progress)?;
                let fresh = nova_stremio::reconcile(
                    &source,
                    &read_shared_for_title(connection, &title)?,
                    false,
                );
                let fresh = nova_stremio::reconcile(&title.desired, &fresh, false);
                if fresh != title.desired {
                    title.desired = fresh;
                    self.check_current(connection, generation)?;
                    save_title(connection, &mut title)?;
                    self.titles.insert(slot, title);
                    self.force = true;
                    continue;
                }
                let stamp = max_stamp(&title.desired).max(remote_stamp);
                if progress_available {
                    title
                        .item
                        .apply_projection(&projection, &title.mapping.videos, stamp)?;
                    if !projection.saved
                        && projection.resume.is_some()
                        && title.desired.resume.changed_at > title.remote.resume.changed_at
                    {
                        title.item.0.insert("temp".into(), serde_json::json!(true));
                    }
                } else {
                    // Membership-only writes preserve the opaque state object
                    // verbatim; empty episode coverage must never zero bits.
                    title.item.apply_membership(projection.saved, stamp);
                }
                // Persist the exact intended wire revision before sending.
                // A crash after acceptance can then recognize this export
                // without dating its source fields at the delivery time.
                self.check_current(connection, generation)?;
                save_title(connection, &mut title)?;
                if title.desired.projection() != projection {
                    self.titles.insert(slot, title);
                    self.force = true;
                    continue;
                }
                self.titles.insert(slot.clone(), title.clone());
                self.check_current(connection, generation)?;
                self.set_phase("Uploading changes to Stremio…");
                self.upload_title(connection, &mut title, generation)?;
                self.set_phase("Confirming changes with Stremio…");
                let verified = self
                    .client
                    .library_get(
                        &connection.auth_key,
                        std::slice::from_ref(&id),
                        false,
                        now_secs(),
                    )?
                    .into_iter()
                    .find(|item| item.id() == id)
                    .ok_or(ApiError::InvalidResponse)?;
                let actual = if progress_available {
                    verified.projection(&title.mapping.videos)?
                } else {
                    membership_projection(&verified)
                };
                let matches = if progress_available {
                    actual == projection
                } else {
                    actual.saved == projection.saved
                };
                if !matches {
                    self.remote_items.insert(id.clone(), verified);
                    self.titles.insert(slot, title);
                    self.force = true;
                    continue;
                }
                title.item = verified;
                remote_confirmed = true;
                if progress_available {
                    title.remote = title.desired.clone();
                } else {
                    title.remote.saved = title.desired.saved.clone();
                }
                title.pending = false;
                // The verification request was already issued. Preserve its
                // result for this durable owner before checking UI lifetime.
                self.owner.run(|| save_title(connection, &mut title))?;
            }
            if progress_available {
                title.local = title.desired.clone();
            } else {
                title.local.saved = title.desired.saved.clone();
            }
            if progress_available {
                title.progress_merged = true;
            }
            if remote_confirmed {
                self.remote_items.insert(id.clone(), title.item.clone());
            }
            self.set_phase("Saving merged library on this device…");
            self.check_current(connection, generation)?;
            save_title(connection, &mut title)?;
            let still_pending = if progress_available {
                title.desired.projection() != title.remote.projection()
            } else {
                title.desired.saved.value != title.remote.saved.value
            };
            if still_pending {
                title.pending = true;
                self.force = true;
                self.check_current(connection, generation)?;
                save_title(connection, &mut title)?;
            }
            self.titles.insert(slot, title);
        }
        self.check_current(connection, generation)?;
        if !connection.bootstrap_done {
            self.save_flag(connection, SessionFlag::Bootstrap)?;
        }
        self.last_position_upload = Instant::now();
        self.queue_refresh();
        self.phase = "";
        let pending = self
            .pending
            .values()
            .map(|title| {
                let reason = match title.reason {
                    stremio_metadata::PendingReason::InvalidIdentity => {
                        "Unsupported title identifier"
                    }
                    stremio_metadata::PendingReason::MetadataUnavailable => {
                        "Episode metadata unavailable"
                    }
                    stremio_metadata::PendingReason::AmbiguousIdentity => {
                        "Conflicting episode mappings"
                    }
                    stremio_metadata::PendingReason::EpisodeMappingUnavailable => {
                        "No confirmed episode mapping"
                    }
                    stremio_metadata::PendingReason::UnsupportedIdentity => {
                        "No confirmed Stremio catalog identifier"
                    }
                    stremio_metadata::PendingReason::RemoteTitleCollision => {
                        "Conflicting title mapping"
                    }
                    stremio_metadata::PendingReason::WatchedHistoryUnavailable => {
                        "Watched history could not be aligned"
                    }
                };
                text::stremio_pending_title(&title.name, &title.media_type, &title.id, reason)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        ui(&self.bridge, move |app| {
            app.set_stremio_busy(false);
            app.set_stremio_preview("".into());
            app.set_stremio_merge_ready(false);
            app.set_stremio_pending(pending.into());
            app.set_stremio_status(text::tr("Stremio sync complete").into());
        });
        Ok(())
    }
    fn set_phase(&mut self, phase: &'static str) {
        self.phase = phase;
        ui(&self.bridge, move |app| {
            app.set_stremio_status(text::tr(phase).into());
        });
    }
    fn upload_title(
        &self,
        connection: &Connection,
        title: &mut Title,
        generation: u64,
    ) -> Result<(), ApiError> {
        self.check_current(connection, generation)?;
        self.authorize_title(connection, title)?;
        title.accepted_upload = None;
        self.owner.run(|| save_title(connection, title))?;
        self.check_current(connection, generation)?;
        let fingerprint = format!(
            "{:016x}",
            fnv1a(
                serde_json::to_string(&title.item)
                    .map_err(|_| ApiError::InvalidResponse)?
                    .as_bytes()
            )
        );
        self.authorize_title(connection, title)?;
        self.client.library_put(
            &connection.auth_key,
            std::slice::from_ref(&title.item),
            now_secs(),
        )?;
        // A successful external mutation belongs to the issuing profile even
        // if cancellation arrived while HTTP was blocked. Keep pending until
        // verified; this receipt records acceptance rather than guessing it.
        title.accepted_upload = Some(fingerprint);
        self.owner.run(|| save_title(connection, title))?;
        self.check_current(connection, generation)
    }

    fn check_current(&self, connection: &Connection, generation: u64) -> Result<(), ApiError> {
        if !profiles::is_current(&profiles::capture()) {
            return Err(ApiError::Rejected);
        }
        if generation != self.bridge.stremio().generation.load(Ordering::Acquire) {
            return Err(ApiError::Rejected);
        }
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let store = owner.lock().unwrap();
        let mut current = store
            .record(&crate::app::sync::profile_domain(DOMAIN), CONNECTION)
            .and_then(|r| r.value.as_ref())
            .and_then(|raw| serde_json::from_str::<Connection>(raw).ok());
        if let Some(current) = current.as_mut() {
            session_flags(&store, current)?;
        }
        if current
            .as_ref()
            .is_none_or(|c| !c.valid() || !c.same_session(connection))
        {
            return Err(ApiError::Authentication);
        }
        if current.as_ref().is_some_and(|c| c.mode != connection.mode) {
            return Err(ApiError::Rejected);
        }
        Ok(())
    }
    fn authorize_title(&self, connection: &Connection, title: &Title) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        selection::authorize(&owner.lock().unwrap(), connection, title)
    }
    fn apply_local(
        &mut self,
        connection: &Connection,
        title: &Title,
        with_progress: bool,
        generation: u64,
    ) -> Result<Canonical, ApiError> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.check_current(connection, generation)?;
            self.authorize_title(connection, title)?;
            persistence::flush().map_err(|_| ApiError::Rejected)?;
            if Instant::now() >= deadline {
                return Err(ApiError::Rejected);
            }
            let Some(prepared) =
                materialize::prepare(&self.bridge, connection, title, with_progress, generation)?
            else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            // Nothing needs materializing: avoid an event-loop round trip for
            // every unchanged title in routine minute-by-minute polling.
            if let Some(desired) = prepared.unchanged() {
                return Ok(desired);
            }
            let bridge = self.bridge.clone();
            let (tx, rx) = mpsc::sync_channel(1);
            crate::app::profiles::invoke_from_event_loop(move || {
                let _ = tx.send(materialize::accept(&bridge, prepared, generation));
            })
            .map_err(|_| ApiError::Rejected)?;
            match rx
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| ApiError::Rejected)??
            {
                materialize::Accepted::Retry => thread::sleep(Duration::from_millis(10)),
                materialize::Accepted::Ready(desired, receipt) => {
                    if let Some(receipt) = receipt {
                        self.applied
                            .insert((title.mapping.media_type.clone(), title.mapping.id.clone()));
                        materialize::wait(receipt)?;
                    }
                    return Ok(desired);
                }
            }
        }
    }

    fn queue_refresh(&mut self) {
        if self.applied.is_empty() {
            return;
        }
        let changed = std::mem::take(&mut self.applied);
        let bridge = self.bridge.clone();
        let _ = crate::app::profiles::invoke_from_event_loop(move || {
            bridge
                .stremio()
                .surfaces_dirty
                .store(true, Ordering::Release);
            let matching = {
                let Some(state) = bridge.personal_state() else {
                    return;
                };
                state
                    .modal_item
                    .as_ref()
                    .is_some_and(|item| changed.contains(&(item.type_.clone(), item.id.clone())))
            };
            if matching {
                bridge.apply_episode_rows();
                bridge.apply_season_cards();
                bridge.refresh_detail_library_watched();
                if let Some(app) = bridge.app() {
                    let saved = {
                        let Some(state) = bridge.personal_state() else {
                            return;
                        };
                        state.modal_item.as_ref().is_some_and(|item| {
                            state
                                .entries
                                .iter()
                                .any(|entry| entry.id == item.id && entry.type_ == item.type_)
                        })
                    };
                    app.set_in_library(saved);
                }
            }
            bridge.refresh_stremio_surfaces_if_visible();
        });
    }
    fn failed(&mut self, error: ApiError) {
        self.queue_refresh();
        tracing::warn!(
            ?error,
            phase = self.phase,
            "Stremio sync failed; pending changes retained"
        );
        if error == ApiError::Authentication {
            self.verified = false;
            self.auth_blocked = true;
        }
        self.retry_at = Instant::now()
            + Duration::from_secs(match error {
                ApiError::RateLimited { retry_at } => {
                    retry_at.saturating_sub(now_secs()).clamp(1, 86_400)
                }
                ApiError::Authentication => 60,
                _ => self.retry_secs,
            });
        self.retry_secs = (self.retry_secs * 2).min(300);
        publish_error(&self.bridge, error, self.phase);
    }
    fn storage_failed(&mut self) {
        self.failed(ApiError::Rejected);
    }
}

fn ui(bridge: &Bridge, action: impl FnOnce(AppWindow) + Send + 'static) {
    let weak = bridge.app.clone();
    let _ = crate::app::profiles::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            action(app);
        }
    });
}
fn publish_error(bridge: &Bridge, error: ApiError, phase: &'static str) {
    ui(bridge, move |app| {
        app.set_stremio_busy(false);
        if error == ApiError::Authentication {
            app.set_stremio_connected(false);
        }
        let message = match error {
            ApiError::Http { status } => text::stremio_http_error(status),
            ApiError::RemoteRejected { code } => text::stremio_api_error(code),
            error => text::tr(match error {
                ApiError::Authentication => "Stremio needs reconnection",
                ApiError::Offline => "Stremio is unavailable. Changes will retry.",
                ApiError::Timeout => "Stremio request timed out. Changes will retry.",
                ApiError::RateLimited { .. } => "Stremio sync is rate limited. Changes will retry.",
                ApiError::InvalidInput | ApiError::InvalidResponse => {
                    "Stremio returned invalid data"
                }
                ApiError::Rejected => "Stremio sync could not be saved",
                ApiError::Http { .. } | ApiError::RemoteRejected { .. } => unreachable!(),
            })
            .to_owned(),
        };
        app.set_stremio_status(text::stremio_failure(&message, phase).into());
    });
}
fn mapping(resolved: stremio_metadata::ResolvedTitle, descriptor_revision: u64) -> Mapping {
    if let Some(metadata) = resolved.native_metadata() {
        // Only exact typed identities may populate Nova's native cache;
        // provider aliases keep their source-specific episode identifiers.
        write_episode_meta_cache_for(&resolved.media_type, &resolved.id, metadata);
        merge_meta_header_for(
            &resolved.media_type,
            &resolved.id,
            &meta_header_from_item(metadata),
        );
    }
    Mapping {
        version: MAPPING_VERSION,
        media_type: resolved.media_type,
        id: resolved.id,
        videos: resolved.videos,
        episode_ids: resolved.episode_ids,
        checked_at: nova_config::now_ms(),
        inventory_revision: nova_providers::metadata_revision(),
        descriptor_revision,
        local_inventory_revision: resolved.local_inventory_revision,
    }
}
fn local_titles(
    entries: &[LibraryEntry],
    progress: &HashMap<String, EpisodeProgress>,
) -> Vec<stremio_metadata::LocalTitle> {
    let mut identities: HashSet<_> = entries
        .iter()
        .map(|e| (e.type_.clone(), e.id.clone()))
        .collect();
    identities.extend(progress.values().filter_map(|p| {
        p.history
            .as_ref()
            .map(|h| (h.type_.clone(), p.series_id.clone()))
    }));
    identities
        .into_iter()
        .map(|(media_type, id)| stremio_metadata::LocalTitle {
            videos: read_episodes_cache_for(&media_type, &id).unwrap_or_default(),
            media_type,
            id,
        })
        .collect()
}
fn millis(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).min(u64::MAX as f64) as u64
    } else {
        0
    }
}
fn local_projection(
    mapping: &Mapping,
    entries: &[LibraryEntry],
    progress: &HashMap<String, EpisodeProgress>,
) -> Projection {
    let mut result = Projection {
        saved: entries
            .iter()
            .any(|e| e.id == mapping.id && e.type_ == mapping.media_type),
        ..Default::default()
    };
    let mut latest = None;
    for (remote, native) in &mapping.episode_ids {
        if let Some(p) = progress.get(&progress_map_key(&mapping.id, native)) {
            result.watched.insert(remote.clone(), p.watched);
            if latest
                .as_ref()
                .is_none_or(|(_, old): &(&String, &EpisodeProgress)| {
                    (p.updated_at_secs, native.as_str())
                        > (old.updated_at_secs, old.episode_id.as_str())
                })
            {
                latest = Some((remote, p));
            }
        }
    }
    if let Some((id, p)) = latest.filter(|(_, p)| playback::playback_can_resume(p)) {
        result.resume = Some(Resume {
            video_id: id.clone(),
            position_ms: millis(p.position_secs),
            duration_ms: millis(p.duration_secs),
        });
    }
    result
}
fn source_canonical(
    connection: &Connection,
    title: &Title,
    projection: &Projection,
    entries: &[LibraryEntry],
    progress: &HashMap<String, EpisodeProgress>,
) -> Result<Canonical, ApiError> {
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let (events, membership) = {
        let store = owner.lock().unwrap();
        let events = stremio_events::try_load(
            &store,
            &connection.account_id,
            &title.mapping.media_type,
            &title.mapping.id,
        )
        .map_err(|_| ApiError::InvalidResponse)?
        .unwrap_or_default();
        let membership = if title.local.saved.changed_at == 0 && events.saved.changed_at == 0 {
            let key = format!(
                "{}saved",
                shared_prefix(&connection.account_id, title.item.id())
            );
            store
                .record(&crate::app::sync::profile_domain(DOMAIN), &key)
                .and_then(|record| record.value.clone())
        } else {
            None
        };
        (events, membership)
    };
    // Reconcile large episode inventories without blocking UI store readers.
    let mut state = title.local.clone();
    let mut shared_membership = false;
    if let Some(raw) = membership {
        // Another connector may already own this title's membership. A newly
        // materialized library card carries a display date, not a fresh add.
        state.saved = serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?;
        shared_membership = true;
    }
    let membership_stamp = entries
        .iter()
        .find(|e| e.id == title.mapping.id && e.type_ == title.mapping.media_type)
        .map_or(0, |e| e.added_at_secs.saturating_mul(1000));
    if events.saved.changed_at > state.saved.changed_at {
        state.saved = events.saved.clone();
    } else if !shared_membership
        && events.saved.changed_at == 0
        && state.saved.value != projection.saved
        && (state.saved.changed_at == 0 || membership_stamp > state.saved.changed_at)
    {
        state.saved = Stamped {
            value: projection.saved,
            changed_at: membership_stamp,
        };
    }
    for (remote, watched) in &projection.watched {
        let native = title.mapping.episode_ids.get(remote).unwrap_or(remote);
        let event = events.watched.get(native);
        let checkpoint = progress.get(&progress_map_key(&title.mapping.id, native));
        // Playback updates the whole native checkpoint. Once a watched event
        // exists, that generic timestamp must not make old history newer.
        let stamp = checkpoint
            .map_or(0, |p| {
                if *watched {
                    p.updated_at_secs
                } else {
                    p.unwatched_at_secs
                }
            })
            .saturating_mul(1000);
        let field = state.watched.entry(remote.clone()).or_default();
        if let Some(event) = event.filter(|e| e.changed_at > 0) {
            if event.changed_at > field.changed_at {
                *field = event.clone();
            }
        } else if field.value != *watched && (field.changed_at == 0 || stamp > field.changed_at) {
            *field = Stamped {
                value: *watched,
                changed_at: stamp,
            };
        }
    }
    // Deleted native checkpoints are absent from projection.watched, but their
    // explicit false events remain actionable and must cross to Stremio.
    for (remote, native) in &title.mapping.episode_ids {
        if let Some(event) = events.watched.get(native) {
            let field = state.watched.entry(remote.clone()).or_default();
            if event.changed_at > field.changed_at {
                *field = event.clone();
            }
        }
    }
    let owns_resume = state
        .resume
        .value
        .as_ref()
        .is_none_or(|resume| title.mapping.episode_ids.contains_key(&resume.video_id));
    if events.resume.changed_at > 0 {
        let mapped = match &events.resume.value {
            Some(resume) => title
                .mapping
                .episode_ids
                .iter()
                .find(|(_, native)| **native == resume.video_id)
                .map(|(remote, _)| {
                    Some(Resume {
                        video_id: remote.clone(),
                        ..resume.clone()
                    })
                }),
            None if owns_resume => Some(None),
            None => None,
        };
        if events.resume.changed_at > state.resume.changed_at
            && let Some(value) = mapped
        {
            state.resume = Stamped {
                value,
                changed_at: events.resume.changed_at,
            };
        }
    } else {
        let latest_stamp = title
            .mapping
            .episode_ids
            .values()
            .filter_map(|native| progress.get(&progress_map_key(&title.mapping.id, native)))
            .map(|p| p.updated_at_secs.saturating_mul(1000))
            .max()
            .unwrap_or(0);
        if (owns_resume || projection.resume.is_some())
            && state.resume.value != projection.resume
            && (state.resume.changed_at == 0 || latest_stamp > state.resume.changed_at)
        {
            state.resume = Stamped {
                value: projection.resume.clone(),
                changed_at: latest_stamp,
            };
        }
    }
    selection::restrict_for_mode(connection.mode, title.selection.as_ref(), &mut state);
    Ok(state)
}
fn membership_projection(item: &LibraryItem) -> Projection {
    Projection {
        saved: item.0.get("removed").and_then(|v| v.as_bool()) == Some(false)
            && item.0.get("temp").and_then(|v| v.as_bool()) != Some(true),
        ..Default::default()
    }
}
fn max_stamp(state: &Canonical) -> u64 {
    state
        .watched
        .values()
        .map(|v| v.changed_at)
        .chain([state.saved.changed_at, state.resume.changed_at])
        .max()
        .unwrap_or(0)
}
fn title_prefix(account: &str) -> String {
    format!(
        "stremio:state:v1:{}:",
        serde_json::to_string(account).unwrap()
    )
}
fn title_key(account: &str, id: &str) -> String {
    format!(
        "{}{}",
        title_prefix(account),
        serde_json::to_string(id).unwrap()
    )
}
fn title_slot(title: &Title) -> String {
    match &title.binding {
        None => title.item.id().into(),
        Some(binding) => {
            serde_json::to_string(&(title.item.id(), &binding.media_type, &binding.id))
                .expect("binding identifiers serialize as JSON strings")
        }
    }
}
fn title_storage_key(account: &str, title: &Title) -> String {
    match &title.binding {
        None => title_key(account, title.item.id()),
        Some(binding) => format!(
            "{}{}",
            title_prefix(account),
            serde_json::to_string(&(title.item.id(), &binding.media_type, &binding.id,))
                .expect("binding identifiers serialize as JSON strings")
        ),
    }
}
fn shared_prefix(account: &str, id: &str) -> String {
    format!("title:{}:", serde_json::to_string(&(account, id)).unwrap())
}
fn load_titles(account: &str) -> Result<BTreeMap<String, Title>, ()> {
    let rows = storage::try_scan_prefix(&title_prefix(account)).map_err(|_| ())?;
    if rows.len() > MAX_TITLES {
        return Err(());
    }
    let keys: HashSet<_> = rows.iter().map(|(key, _)| key.clone()).collect();
    let mut migrated = Vec::new();
    let mut titles = BTreeMap::new();
    for (key, raw) in rows {
        let mut title: Title = serde_json::from_str(&raw).map_err(|_| ())?;
        if title.version != 1 || title.item.id().is_empty() {
            return Err(());
        }
        if title.binding.is_none()
            && (title.mapping.id != title.item.id()
                || title.mapping.media_type != title.item.media_type())
        {
            title.binding = Some(NativeBinding {
                id: title.mapping.id.clone(),
                media_type: title.mapping.media_type.clone(),
            });
        }
        let destination = title_storage_key(account, &title);
        if destination != key {
            // Move a legacy single-source row atomically before a canonical
            // card can reuse its old key. Existing binding rows win over
            // leftover legacy copies from an interrupted older process.
            migrated.push((key, None));
            if keys.contains(&destination) {
                continue;
            }
            migrated.push((
                destination,
                Some(serde_json::to_string(&title).map_err(|_| ())?),
            ));
        }
        // A restart renews identity evidence before retained work is sent.
        title.mapping.checked_at = 0;
        titles.insert(title_slot(&title), title);
    }
    if !migrated.is_empty() {
        storage::try_write_batch(&migrated).map_err(|_| ())?;
    }
    Ok(titles)
}
fn read_shared(connection: &Connection, id: &str) -> Result<Canonical, ApiError> {
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let prefix = shared_prefix(&connection.account_id, id);
    let records = owner
        .lock()
        .unwrap()
        .records_with_prefix(&crate::app::sync::profile_domain(DOMAIN), &prefix);
    let mut result = Canonical::default();
    for (key, raw) in records {
        let Some(field) = key.strip_prefix(&prefix) else {
            continue;
        };
        match field {
            "saved" => {
                result.saved = serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?
            }
            "resume" => {
                result.resume = serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?
            }
            _ => {
                if let Some(id) = field.strip_prefix("watched:") {
                    result.watched.insert(
                        serde_json::from_str(id).map_err(|_| ApiError::InvalidResponse)?,
                        serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?,
                    );
                }
            }
        }
    }
    if max_stamp(&result) > nova_config::now_ms().saturating_add(60 * 60 * 1000)
        || result.watched.len() > nova_stremio::MAX_VIDEOS
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(result)
}
fn read_shared_for_title(connection: &Connection, title: &Title) -> Result<Canonical, ApiError> {
    let mut state = read_shared(connection, title.item.id())?;
    selection::restrict_for_mode(connection.mode, title.selection.as_ref(), &mut state);
    Ok(state)
}
fn save_title(connection: &Connection, title: &mut Title) -> Result<(), ApiError> {
    if max_stamp(&title.desired) > nova_config::now_ms().saturating_add(60 * 60 * 1000)
        || title.desired.watched.len() > nova_stremio::MAX_VIDEOS
    {
        return Err(ApiError::InvalidResponse);
    }
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let mut store = owner.lock().unwrap();
    if store
        .record("profile_deletions", &profiles::current_id())
        .is_some_and(|r| !r.is_deleted())
    {
        return Err(ApiError::Rejected);
    }
    if selection::authorize(&store, connection, title).is_err() {
        // A request already accepted while a selection was removed still
        // keeps its durable receipt; it must not republish upload authority.
        if title.accepted_upload.is_some() {
            store.queue_extra(
                &profiles::storage_key(&title_storage_key(&connection.account_id, title)),
                Some(serde_json::to_string(title).map_err(|_| ApiError::InvalidResponse)?),
            );
            return store.save().map_err(|_| ApiError::Rejected);
        }
        return Err(ApiError::Rejected);
    }
    let prefix = shared_prefix(&connection.account_id, title.item.id());
    fn field<T: Serialize + PartialEq + for<'de> Deserialize<'de>>(
        store: &mut nova_sync::Store,
        key: String,
        field: &mut Stamped<T>,
        remote: Option<&Stamped<T>>,
        initial_union: bool,
    ) -> Result<(), ApiError> {
        let raw = serde_json::to_string(field).map_err(|_| ApiError::InvalidResponse)?;
        let rank = u32::from(remote.is_some_and(|r| r == field));
        let version = nova_sync::Version::new(
            field.changed_at,
            rank,
            nova_config::fnv1a(raw.as_bytes()),
            false,
        );
        store.apply(
            &crate::app::sync::profile_domain(DOMAIN),
            &key,
            nova_sync::Record {
                value: Some(raw.clone()),
                version,
            },
        );
        if initial_union
            && let Some(retained) = store
                .record(&crate::app::sync::profile_domain(DOMAIN), &key)
                .and_then(|row| row.value.as_ref())
        {
            let retained: Stamped<T> =
                serde_json::from_str(retained).map_err(|_| ApiError::InvalidResponse)?;
            if retained.value != field.value
                && remote.is_some_and(|observed| {
                    observed.value == retained.value && observed.changed_at == retained.changed_at
                })
            {
                // Order a new binding's initial union just after the observed
                // server negative, retaining original source clocks in the
                // payload. Never date old history at delivery time: a native
                // edit made during the request must still supersede it.
                let observed = store
                    .record(&crate::app::sync::profile_domain(DOMAIN), &key)
                    .unwrap()
                    .version;
                store.apply(
                    &crate::app::sync::profile_domain(DOMAIN),
                    &key,
                    nova_sync::Record {
                        value: Some(raw.clone()),
                        version: nova_sync::Version::new(
                            observed.ts,
                            observed
                                .counter
                                .checked_add(1)
                                .ok_or(ApiError::InvalidResponse)?,
                            nova_config::fnv1a(raw.as_bytes()),
                            false,
                        ),
                    },
                );
            }
        }
        // A newer mesh action may have arrived after read_shared. Project the
        // winner actually retained by Store, never the rejected stale value.
        *field = store
            .record(&crate::app::sync::profile_domain(DOMAIN), &key)
            .and_then(|r| r.value.as_ref())
            .map(|raw| serde_json::from_str(raw).map_err(|_| ApiError::InvalidResponse))
            .transpose()?
            .ok_or(ApiError::Rejected)?;
        Ok(())
    }
    let union_membership = !title.progress_merged && title.desired.saved.value;
    field(
        &mut store,
        format!("{prefix}saved"),
        &mut title.desired.saved,
        Some(&title.remote.saved),
        union_membership,
    )?;
    field(
        &mut store,
        format!("{prefix}resume"),
        &mut title.desired.resume,
        Some(&title.remote.resume),
        false,
    )?;
    for (id, value) in &mut title.desired.watched {
        if title.selection.as_ref().is_some_and(|s| {
            !s.episode_ids.contains_key(id)
                || (connection.mode == SyncMode::SelectedTitles
                    && value.changed_at <= s.history_since_ms)
        }) {
            continue;
        }
        let initial_union = !title.progress_merged && value.value;
        field(
            &mut store,
            format!("{prefix}watched:{}", serde_json::to_string(id).unwrap()),
            value,
            title.remote.watched.get(id),
            initial_union,
        )?;
    }
    // Unknown local episode coverage must not hide independently received
    // fields from the shared canonical title.
    for (key, raw) in store.records_with_prefix(
        &crate::app::sync::profile_domain(DOMAIN),
        &format!("{prefix}watched:"),
    ) {
        if let Some(episode) = key.strip_prefix(&format!("{prefix}watched:")) {
            let episode: String =
                serde_json::from_str(episode).map_err(|_| ApiError::InvalidResponse)?;
            let value = serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?;
            if title.selection.as_ref().is_some_and(|s| {
                !s.episode_ids.contains_key(&episode) || {
                    let field: &Stamped<bool> = &value;
                    connection.mode == SyncMode::SelectedTitles
                        && field.changed_at <= s.history_since_ms
                }
            }) {
                continue;
            }
            title.desired.watched.insert(episode, value);
        }
    }
    // Publish the source clocks associated with an exact observed revision.
    // A receiver must also compare each field's value before using this ack.
    if let Ok(observed) = title.item.projection(&title.mapping.videos)
        && observed.saved == title.remote.saved.value
        && observed.resume == title.remote.resume.value
        && observed.watched.iter().all(|(id, value)| {
            title
                .remote
                .watched
                .get(id)
                .is_some_and(|field| field.value == *value)
        })
    {
        let stamp = title.item.modified_ms();
        if stamp > nova_config::now_ms().saturating_add(60 * 60 * 1000) {
            return Err(ApiError::InvalidResponse);
        }
        let raw = serde_json::to_string(&(stamp, &title.remote))
            .map_err(|_| ApiError::InvalidResponse)?;
        // Refined acknowledgments containing original clocks outrank an
        // observation that only knew the item's generic modification time.
        let rank = title
            .remote
            .watched
            .values()
            .map(|field| field.changed_at)
            .chain([
                title.remote.saved.changed_at,
                title.remote.resume.changed_at,
            ])
            .filter(|changed_at| *changed_at > 0 && *changed_at < stamp)
            .count() as u32;
        store.apply(
            &crate::app::sync::profile_domain(DOMAIN),
            &format!("{prefix}remote"),
            nova_sync::Record {
                version: nova_sync::Version::new(
                    stamp,
                    rank,
                    nova_config::fnv1a(raw.as_bytes()),
                    false,
                ),
                value: Some(raw),
            },
        );
    }
    store.queue_extra(
        &profiles::storage_key(&title_storage_key(&connection.account_id, title)),
        Some(serde_json::to_string(title).map_err(|_| ApiError::InvalidResponse)?),
    );
    store.save().map_err(|_| ApiError::Rejected)?;
    drop(store);
    nova_sync::request_local_sync();
    Ok(())
}

fn imported_progress(title: &Title, native: &str) -> EpisodeProgress {
    EpisodeProgress {
        series_id: title.mapping.id.clone(),
        episode_id: native.into(),
        history: Some(PlaybackHistory {
            type_: title.mapping.media_type.clone(),
            title: title.item.name().into(),
            poster_url: title
                .item
                .0
                .get("poster")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// Direct ingress tests use the same preparation, optimistic acceptance and
// ordered background commit without needing to pump a second event loop.
#[cfg(test)]
fn apply_title(
    bridge: &Bridge,
    account: &str,
    title: &Title,
    with_progress: bool,
    generation: u64,
) -> Result<Canonical, ApiError> {
    let _writer = (!persistence::running()).then(persistence::start);
    persistence::flush().map_err(|_| ApiError::Rejected)?;
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let connection: Connection = owner
        .lock()
        .unwrap()
        .record(&crate::app::sync::profile_domain(DOMAIN), CONNECTION)
        .and_then(|row| row.value.as_ref())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or(ApiError::Authentication)?;
    if connection.account_id != account {
        return Err(ApiError::Authentication);
    }
    let prepared = materialize::prepare(bridge, &connection, title, with_progress, generation)?
        .ok_or(ApiError::Rejected)?;
    match materialize::accept(bridge, prepared, generation)? {
        materialize::Accepted::Retry => Err(ApiError::Rejected),
        materialize::Accepted::Ready(desired, receipt) => {
            if let Some(receipt) = receipt {
                materialize::wait(receipt)?;
            }
            Ok(desired)
        }
    }
}

impl Bridge {
    /// Hidden Home/Library models are refreshed on reveal, never while an
    /// unrelated detail screen is scrolling. The normal UI tick consumes this
    /// flag; repeated syncs coalesce into one visible-model refresh.
    pub(super) fn refresh_stremio_surfaces_if_visible(&self) {
        if !self.stremio().surfaces_dirty.load(Ordering::Acquire) {
            return;
        }
        let Some(app) = self.app() else { return };
        if app.get_modal_visible()
            || app.get_player_open()
            || app.get_more_open()
            || app.get_show_settings()
            || app.get_show_about()
            || app.get_party_page_open()
            || app.get_remote_controls_open()
            || app.get_remote_devices_open()
            || !(app.get_show_home() || app.get_show_library() || app.get_show_calendar())
        {
            return;
        }
        if self.stremio().surfaces_dirty.swap(false, Ordering::AcqRel) {
            // This remains a model publication on the UI thread, once for the
            // changed batch and only once its dependent page is visible.
            let _guard = sync::ApplyingGuard::new();
            if app.get_show_calendar() {
                self.apply_calendar_to_ui();
            } else if app.get_show_library() {
                self.update_library_badges();
            } else {
                self.rebuild_continue_list();
                self.rebuild_upcoming_list();
                self.apply_home_to_ui();
                self.dispatch_continue_posters();
                self.dispatch_upcoming_posters();
            }
        }
    }
}

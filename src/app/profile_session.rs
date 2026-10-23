//! Lifetime boundaries for account, device, and selected personal work.
use super::*;
use parking_lot::{ArcMutexGuard, RawMutex};
use std::sync::RwLock;

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(super) struct AccountRuntime {
    /// Most recent sync notice, retained in semantic form so a language switch
    /// can rerender it instead of leaving the old translation on screen.
    pub sync_link_notice: Mutex<Option<SyncNotice>>,
    pub profile_recovery_pending: Arc<AtomicBool>,
    #[cfg(feature = "desktop")]
    pub sync_start_generation: Arc<AtomicU64>,
    #[cfg(feature = "desktop")]
    pub sync_starting: Arc<AtomicBool>,
}

pub(super) struct DeviceRuntime {
    pub sessions: Arc<SessionDirectory>,
    pub account: AccountRuntime,
    pub catalog_gen: Arc<AtomicU64>,
    pub home_showcase_gen: Arc<AtomicU64>,
    pub home_catalog_rows_gen: Arc<AtomicU64>,
    #[cfg(feature = "desktop")]
    pub poster_tx: PosterTx,
    pub player: crate::player::Player,
    pub downloads: DownloadCoordinator,
    pub last_stream: Arc<last_stream::Cache>,
    pub profile_loaded_epoch: Arc<AtomicU64>,
    pub downloads_seen: Arc<AtomicU64>,
    pub image_usage_scan: Arc<Mutex<settings::DiskUsageScan>>,
    pub torrent_usage_scan: Arc<Mutex<settings::DiskUsageScan>>,
}
impl std::ops::Deref for DeviceRuntime {
    type Target = AccountRuntime;
    fn deref(&self) -> &Self::Target {
        &self.account
    }
}

pub(super) struct ProfileSession {
    runtime_id: u64,
    pub id: String,
    pub epoch: u64,
    pub store: profiles::ProfileStore,
    pub providers: std::sync::OnceLock<nova_providers::plugin_settings::SettingsCapability>,
    state: Arc<parking_lot::Mutex<Shared>>,
    pub tracking: tracking::StateHandle,
    pub stremio: Arc<stremio::Handle>,
    pub list_services: Arc<list_services::Handle>,
    pub cancelled: AtomicBool,
    pub services_requested: AtomicBool,
}
impl ProfileSession {
    fn new(runtime_id: u64, context: profiles::Context, state: Shared) -> Arc<Self> {
        Arc::new(Self {
            runtime_id,
            store: if profiles::enabled() {
                profiles::ProfileStore::for_profile(&context.id)
            } else {
                profiles::ProfileStore::capture()
            },
            id: context.id,
            epoch: context.epoch,
            state: Arc::new(parking_lot::Mutex::new(state)),
            providers: std::sync::OnceLock::new(),
            tracking: Arc::new(tracking::TrackingHandle::new()),
            stremio: Arc::new(stremio::Handle::new()),
            list_services: Arc::new(list_services::Handle::new()),
            cancelled: AtomicBool::new(false),
            services_requested: AtomicBool::new(false),
        })
    }
    // Never store a Context in its session: Context owns the Arc that keeps
    // originating work alive, so a back-reference would form a strong cycle.
    pub fn context(self: &Arc<Self>) -> profiles::Context {
        profiles::Context {
            id: self.id.clone(),
            epoch: self.epoch,
            session: Some(self.clone()),
        }
    }
    pub fn is_finished(&self) -> bool {
        self.tracking.is_finished()
            && self.stremio.is_finished()
            && self.list_services.is_finished()
    }
    pub fn provider_inventory(&self) -> Result<nova_providers::OwnedProviderSnapshot, String> {
        let (packages, metadata) = {
            let state = self.state.lock();
            (
                state.loaded_packages.values().cloned().collect(),
                state
                    .installed
                    .iter()
                    .filter(|addon| addon.enabled && addon.available)
                    .map(|addon| nova_providers::MetadataAddon {
                        url: addon.url.clone(),
                        manifest: addon.manifest.clone(),
                    })
                    .collect(),
            )
        };
        let addons: Vec<AddonStore> = self
            .store
            .read("addons")
            .map_err(|error| error.to_string())?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let configs = addons
            .into_iter()
            .map(|addon| (addon.url, addon.capabilities.unwrap_or_default()))
            .collect();
        Ok(nova_providers::OwnedProviderSnapshot::new(
            packages, configs, metadata,
        ))
    }
}

pub(super) struct SessionDirectory {
    runtime_id: u64,
    active: RwLock<Arc<ProfileSession>>,
    draining: Mutex<Vec<Arc<ProfileSession>>>,
}
impl SessionDirectory {
    pub fn new() -> Arc<Self> {
        let runtime_id = NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed);
        let active = ProfileSession::new(
            runtime_id,
            profiles::active_context(),
            Shared {
                chosen_addon: usize::MAX,
                ..Shared::default()
            },
        );
        let _ = active
            .providers
            .set(nova_providers::plugin_settings::capture_settings_capability());
        profiles::attach_session(&active);
        Arc::new(Self {
            runtime_id,
            active: RwLock::new(active),
            draining: Mutex::default(),
        })
    }
    pub fn active(&self) -> Arc<ProfileSession> {
        self.active.read().unwrap().clone()
    }
    pub fn prepare(&self, id: &str, state: Shared) -> Arc<ProfileSession> {
        ProfileSession::new(self.runtime_id, profiles::activation_context(id), state)
    }
    pub fn publish(&self, session: Arc<ProfileSession>) {
        let outgoing = std::mem::replace(&mut *self.active.write().unwrap(), session);
        outgoing.cancelled.store(true, Ordering::Release);
        self.draining.lock().unwrap().push(outgoing);
        self.reap();
    }
    pub fn reap(&self) {
        self.draining
            .lock()
            .unwrap()
            .retain(|session| !session.is_finished());
    }
    pub fn can_start(&self, session: &ProfileSession) -> bool {
        !session.cancelled.load(Ordering::Acquire)
            && !self
                .draining
                .lock()
                .unwrap()
                .iter()
                .any(|old| old.id == session.id && !old.is_finished())
    }
    pub fn resolve(&self) -> Result<Arc<ProfileSession>, &'static str> {
        if !profiles::enabled() {
            return Ok(self.active());
        }
        let context = profiles::capture();
        context
            .session
            .filter(|session| {
                session.runtime_id == self.runtime_id
                    && session.id == context.id
                    && session.epoch == context.epoch
            })
            .ok_or("a durable profile owner cannot access activation state")
    }
    #[cfg(test)]
    pub fn all(&self) -> Vec<Arc<ProfileSession>> {
        let mut result = self.draining.lock().unwrap().clone();
        result.push(self.active());
        result
    }
}

pub(super) type StateGuard = ArcMutexGuard<RawMutex, Shared>;
/// Resolve at the operation boundary. UI callbacks use the selected context;
/// captured workers retain their original allocation even after A → B → A.
#[derive(Clone)]
pub(super) struct SessionState(pub Arc<SessionDirectory>);
impl SessionState {
    pub fn lock(&self) -> Result<StateGuard, &'static str> {
        Ok(self.0.resolve()?.state.lock_arc())
    }
    pub fn try_lock(&self) -> Result<StateGuard, &'static str> {
        self.0
            .resolve()?
            .state
            .try_lock_arc()
            .ok_or("profile state is busy")
    }
}

impl Bridge {
    pub(super) fn retry_profile_services(&self) {
        if super::profile_policy::entry_blocked() {
            return;
        }
        self.sessions.reap();
        let session = self.sessions.active();
        if session.services_requested.load(Ordering::Acquire) && self.sessions.can_start(&session) {
            profiles::with_context(session.context(), || {
                self.initialize_tracking();
                self.start_stremio();
                self.start_list_services();
            });
        }
    }
    pub(super) fn profile_services_can_start(&self) -> bool {
        if super::profile_policy::entry_blocked() {
            return false;
        }
        self.sessions.resolve().is_ok_and(|session| {
            profiles::is_current(&session.context()) && self.sessions.can_start(&session)
        })
    }
}

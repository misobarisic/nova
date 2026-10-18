//! Bridge construction and accessor.
use super::*;

/// Process-wide handle to the UI bridge, installed once `app::run` builds it.
///
/// Android's camera scanner JNI entry (android_qr.rs) runs on the camera's
/// ImageReader thread and must marshal a decoded ticket back through the same
/// bridge the UI callbacks use. The scanner locks this and calls the bridge
/// from inside `crate::app::profiles::invoke_from_event_loop`, so no Slint access happens on
/// the camera thread.
static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

/// Install the process bridge (called once from `app::run`).
pub(super) fn install_global_bridge(bridge: &Bridge) {
    *BRIDGE.lock().unwrap() = Some(bridge.clone());
}

/// Run `f` with the process bridge, if one is installed.
// Desktop poster failures and Android JNI callbacks share this event-loop bridge.
pub(super) fn with_global_bridge<R>(f: impl FnOnce(&Bridge) -> R) -> Option<R> {
    BRIDGE.lock().unwrap().as_ref().map(f)
}

impl Bridge {
    pub(super) fn new(
        app: slint::Weak<AppWindow>,
        #[cfg(feature = "desktop")] poster_tx: PosterTx,
        catalog_gen: Arc<AtomicU64>,
        player: crate::player::Player,
        downloads: DownloadCoordinator,
    ) -> Self {
        Bridge {
            app,
            shared: Arc::new(Mutex::new(Shared {
                chosen_addon: usize::MAX, // default to "All addons"
                ..Shared::default()
            })),
            catalog_gen,
            home_showcase_gen: Arc::new(AtomicU64::new(0)),
            home_catalog_rows_gen: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "desktop")]
            poster_tx,
            player,
            downloads,
            last_stream: Arc::default(),
            tracking_services: Arc::default(),
            stremio_services: Arc::default(),
            profile_recovery_pending: Arc::default(),
            profile_loaded_epoch: Arc::default(),
            downloads_seen: Arc::new(AtomicU64::new(0)),
            image_usage_scan: Arc::default(),
            torrent_usage_scan: Arc::default(),
            #[cfg(feature = "desktop")]
            sync_start_generation: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "desktop")]
            sync_starting: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn app(&self) -> Option<AppWindow> {
        self.app.upgrade()
    }

    /// A worker may inspect live personal state only while it still belongs
    /// to this activation. Validate after taking the lock: profile activation
    /// changes the epoch before replacing Shared under this same lock.
    pub(super) fn personal_state(&self) -> Option<std::sync::MutexGuard<'_, Shared>> {
        let state = self.shared.lock().unwrap();
        profiles::is_current(&profiles::capture()).then_some(state)
    }

    pub(super) fn tracking(&self) -> tracking::StateHandle {
        let context = profiles::capture();
        let mut services = self.tracking_services.lock().unwrap();
        services.retain(|key, handle| *key == context || !handle.is_finished());
        services
            .entry(context)
            .or_insert_with(|| Arc::new(tracking::TrackingHandle::new()))
            .clone()
    }

    pub(super) fn stremio(&self) -> Arc<stremio::Handle> {
        let context = profiles::capture();
        let mut services = self.stremio_services.lock().unwrap();
        services.retain(|key, handle| *key == context || !handle.is_finished());
        services
            .entry(context)
            .or_insert_with(|| Arc::new(stremio::Handle::new()))
            .clone()
    }
}

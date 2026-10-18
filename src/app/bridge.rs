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
        let sessions = profile_session::SessionDirectory::new();
        Bridge {
            app,
            shared: profile_session::SessionState(sessions.clone()),
            runtime: Arc::new(profile_session::DeviceRuntime {
                sessions,
                account: Default::default(),
                catalog_gen,
                home_showcase_gen: Arc::new(AtomicU64::new(0)),
                home_catalog_rows_gen: Arc::new(AtomicU64::new(0)),
                #[cfg(feature = "desktop")]
                poster_tx,
                player,
                downloads,
                last_stream: Arc::default(),
                profile_loaded_epoch: Arc::default(),
                downloads_seen: Arc::new(AtomicU64::new(0)),
                image_usage_scan: Arc::default(),
                torrent_usage_scan: Arc::default(),
            }),
        }
    }

    pub(super) fn app(&self) -> Option<AppWindow> {
        self.app.upgrade()
    }

    /// Live actor work is canceled on switching; its retained allocation is
    /// still available for completion and cleanup through the captured owner.
    pub(super) fn personal_state(&self) -> Option<profile_session::StateGuard> {
        let state = self.shared.lock().unwrap();
        profiles::is_current(&profiles::capture()).then_some(state)
    }

    pub(super) fn tracking(&self) -> tracking::StateHandle {
        self.sessions
            .resolve()
            .expect("tracking requires a profile session")
            .tracking
            .clone()
    }

    pub(super) fn stremio(&self) -> Arc<stremio::Handle> {
        self.sessions
            .resolve()
            .expect("Stremio requires a profile session")
            .stremio
            .clone()
    }
}

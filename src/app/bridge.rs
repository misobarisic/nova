//! Bridge construction and accessor.
use super::*;

/// Process-wide handle to the UI bridge, installed once `app::run` builds it.
///
/// Android's camera scanner JNI entry (android_qr.rs) runs on the camera's
/// ImageReader thread and must marshal a decoded ticket back through the same
/// bridge the UI callbacks use. The scanner locks this and calls the bridge
/// from inside `slint::invoke_from_event_loop`, so no Slint access happens on
/// the camera thread.
static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

/// Install the process bridge (called once from `app::run`).
pub(super) fn install_global_bridge(bridge: &Bridge) {
    *BRIDGE.lock().unwrap() = Some(bridge.clone());
}

/// Run `f` with the process bridge, if one is installed.
// On desktop only the install side is used (no JNI entry point).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(super) fn with_global_bridge<R>(f: impl FnOnce(&Bridge) -> R) -> Option<R> {
    BRIDGE.lock().unwrap().as_ref().map(f)
}

impl Bridge {
    pub(super) fn new(
        app: slint::Weak<AppWindow>,
        #[cfg(feature = "desktop")] poster_tx: PosterTx,
        #[cfg(feature = "desktop")] poster_cache: PosterCache,
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
            #[cfg(feature = "desktop")]
            poster_tx,
            #[cfg(feature = "desktop")]
            poster_cache,
            player,
            downloads,
            tracking: Arc::new(Mutex::new(None)),
            downloads_seen: Arc::new(AtomicU64::new(0)),
            stream_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) fn app(&self) -> Option<AppWindow> {
        self.app.upgrade()
    }
}

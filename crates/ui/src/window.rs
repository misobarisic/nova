use slint::{ComponentHandle, Global, StrongHandle};

/// Main-window handle with the compiled page factories installed at creation.
/// Deref retains the generated getters, setters, callbacks and public functions.
pub struct AppWindow(crate::generated::AppWindow);

impl AppWindow {
    pub fn new() -> Result<Self, slint::PlatformError> {
        let app = crate::generated::AppWindow::new()?;
        crate::settings_factory::install(&app);
        crate::page_factories::install(&app);
        crate::collection_factories::install(&app);
        crate::playback_factories::install(&app);
        crate::remote_bridge::install(&app);
        crate::party_bridge::install(&app);
        crate::completion_bridge::install(&app);
        Ok(Self(app))
    }

    /// Call after publishing an asynchronous model/conditional-tree update.
    /// Retains any live editor or popup and repairs orphaned Back routing.
    pub fn recover_back_focus(&self) {
        crate::page_factories::recover_back_focus(&self.0);
    }
}

impl std::ops::Deref for AppWindow {
    type Target = crate::generated::AppWindow;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl StrongHandle for AppWindow {
    type WeakInner = <crate::generated::AppWindow as StrongHandle>::WeakInner;
    fn upgrade_from_weak_inner(inner: &Self::WeakInner) -> Option<Self> {
        crate::generated::AppWindow::upgrade_from_weak_inner(inner).map(Self)
    }
}

impl ComponentHandle for AppWindow {
    fn as_weak(&self) -> slint::Weak<Self> {
        let inner = self.0.clone_strong().into();
        slint::Weak::new(slint::private_unstable_api::re_exports::VRc::downgrade(
            &inner,
        ))
    }
    fn clone_strong(&self) -> Self {
        Self(self.0.clone_strong())
    }
    fn show(&self) -> Result<(), slint::PlatformError> {
        self.0.show()
    }
    fn hide(&self) -> Result<(), slint::PlatformError> {
        self.0.hide()
    }
    fn window(&self) -> &slint::Window {
        self.0.window()
    }
    fn run(&self) -> Result<(), slint::PlatformError> {
        self.0.run()
    }
    fn global<'a, T: Global<'a, Self>>(&'a self) -> T {
        T::get(self)
    }
}

macro_rules! forward_globals {
    ($($global:ident),* $(,)?) => {
        $(impl<'a> Global<'a, AppWindow> for crate::$global<'a> {
            type StaticSelf = crate::$global<'static>;
            fn get(app: &'a AppWindow) -> Self {
                <Self as Global<'a, crate::generated::AppWindow>>::get(&app.0)
            }
            fn as_weak(&self) -> slint::Weak<Self::StaticSelf> {
                <Self as Global<'a, crate::generated::AppWindow>>::as_weak(self)
            }
        })*
    };
}
forward_globals!(
    SmartCollectionsBridge,
    StreamFiltersBridge,
    BookmarksBridge,
    JournalBridge,
    QueueBridge,
    InterfaceStyle,
    CompletionBridge,
    PartyBridge,
    RemoteBridge,
    Theme,
    Anim,
    NavState,
    SettingsSync,
    SettingsStyle,
    ControlInput,
    SliderGesture,
    SettingsBridge,
    HomeBridge,
    DiscoverBridge,
    CalendarBridge,
    DetailBridge,
    NavMetrics,
    SettingsDestinations,
    PlaybackSpeed,
    LibraryBridge,
    MoreBridge,
    AboutBridge,
    DownloadsBridge,
    PlayerOsdBridge,
    OnboardingBridge
);

//! Local window geometry and playback fullscreen memory. Never part of sync.
use super::*;
use slint::winit_030::WinitWindowAccessor;
use std::{cell::RefCell, time::Instant};

const KEY: &str = "desktop:window:v1";
const SETTLE: Duration = Duration::from_secs(2);

pub(super) fn sync_cursor(app: &AppWindow) {
    // Use the window cursor, including invisible controls whose TouchAreas
    // still participate in hit testing. Pointer activity restores it at once.
    let visible = !app.get_player_open()
        || !app.get_playback_started()
        || app.get_osd_visible()
        || app.get_osd_opacity() > 0.0
        || app.get_next_episode_visible()
        || app.get_playback_end_menu_shown();
    app.window()
        .with_winit_window(|native| native.set_cursor_visible(visible));
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
struct Snapshot {
    width: f32,
    height: f32,
    maximized: bool,
    playback_fullscreen: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            width: 1100.0,
            height: 750.0,
            maximized: true,
            playback_fullscreen: false,
        }
    }
}

impl Snapshot {
    fn valid_size(width: f32, height: f32) -> bool {
        width.is_finite()
            && height.is_finite()
            && (360.0..=16384.0).contains(&width)
            && (240.0..=16384.0).contains(&height)
    }

    fn observe_geometry(&mut self, width: f32, height: f32, maximized: bool) {
        self.maximized = maximized;
        // Maximized dimensions must not become the size restored by Unmaximize.
        if !maximized && Self::valid_size(width, height) {
            self.width = width.round();
            self.height = height.round();
        }
    }
}

struct Memory {
    current: Snapshot,
    saved: Option<Snapshot>,
    save_after: Instant,
    geometry_after: Instant,
}

#[derive(Clone)]
pub(super) struct DesktopWindow(Rc<RefCell<Memory>>);

impl DesktopWindow {
    pub(super) fn restore(app: &AppWindow, player: &crate::player::Player) -> Self {
        let saved = read_json::<Snapshot>(KEY);
        let mut current = saved.unwrap_or_default();
        if !Snapshot::valid_size(current.width, current.height) {
            current.width = 1100.0;
            current.height = 750.0;
        }
        app.window()
            .set_size(slint::LogicalSize::new(current.width, current.height));
        app.window().set_maximized(current.maximized);
        player.set_playback_fullscreen_preference(current.playback_fullscreen);
        let now = Instant::now();
        Self(Rc::new(RefCell::new(Memory {
            current,
            saved,
            save_after: now + SETTLE,
            geometry_after: now + SETTLE,
        })))
    }

    fn observe(&self, app: &AppWindow, player: &crate::player::Player) {
        let now = Instant::now();
        let mut memory = self.0.borrow_mut();
        let previous = memory.current;
        memory.current.playback_fullscreen = player.playback_fullscreen_preference();
        let window = app.window();
        if app.get_player_open() || window.is_fullscreen() || window.is_minimized() {
            // Window-manager fullscreen exit/restore is asynchronous. Wait for
            // the catalog's size to settle before remembering its geometry.
            memory.geometry_after = now + SETTLE;
        } else if now >= memory.geometry_after {
            let size = window.size().to_logical(window.scale_factor());
            memory
                .current
                .observe_geometry(size.width, size.height, window.is_maximized());
        }
        if memory.current != previous {
            memory.save_after = now + SETTLE;
        }
    }

    pub(super) fn tick(&self, app: &AppWindow, player: &crate::player::Player) {
        sync_cursor(app);
        self.observe(app, player);
        if Instant::now() >= self.0.borrow().save_after {
            self.save();
        }
    }

    pub(super) fn flush(&self, app: &AppWindow, player: &crate::player::Player) {
        self.observe(app, player);
        self.save();
    }

    fn save(&self) {
        let mut memory = self.0.borrow_mut();
        if memory.saved == Some(memory.current) || !writable_key(KEY) {
            return;
        }
        // A tiny local KV value: do not route geometry through synced settings
        // or flush unrelated collections whenever a window is resized.
        let result = serde_json::to_string(&memory.current)
            .map_err(|e| storage::Error::new(storage::ErrorKind::Serialization, e))
            .and_then(|raw| storage::try_set_str(KEY, &raw));
        match result {
            Ok(()) => memory.saved = Some(memory.current),
            Err(error) => {
                io::record_write_failure(error);
                memory.save_after = Instant::now() + Duration::from_secs(30);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_playback_choices_restore_from_local_storage() {
        const TEST_DIR: &str = "NOVA_WINDOW_MEMORY_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-window-memory-{}-{}",
                std::process::id(),
                now_secs()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args(["--exact", "app::desktop_window::tests::window_and_playback_choices_restore_from_local_storage", "--nocapture"])
                .output().unwrap();
            fs::remove_dir_all(root).unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&PathBuf::from(root));
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let memory = DesktopWindow::restore(&app, &player);
        assert!(app.window().is_maximized());
        assert!(!player.playback_fullscreen_preference());
        app.window().set_maximized(false);
        app.window()
            .set_size(slint::LogicalSize::new(1400.0, 900.0));
        memory.0.borrow_mut().geometry_after = Instant::now() - SETTLE;
        memory.tick(&app, &player);
        assert!(
            storage::get_str(KEY).is_none(),
            "resize writes are debounced"
        );
        memory.flush(&app, &player);
        let saved = read_json::<Snapshot>(KEY).unwrap();
        assert_eq!((saved.width, saved.height), (1400.0, 900.0));
        assert!(!saved.maximized);

        // A fullscreen player cannot overwrite the last catalog geometry.
        app.set_player_open(true);
        app.window().set_fullscreen(true);
        app.window()
            .set_size(slint::LogicalSize::new(2560.0, 1440.0));
        memory.flush(&app, &player);
        let saved = read_json::<Snapshot>(KEY).unwrap();
        assert_eq!((saved.width, saved.height), (1400.0, 900.0));
        assert!(saved.playback_fullscreen);
        player.close();
        memory.flush(&app, &player);
        player.set_playback_fullscreen_preference(false);
        let _restored = DesktopWindow::restore(&app, &player);
        assert_eq!(
            app.window().size().to_logical(app.window().scale_factor()),
            slint::LogicalSize::new(1400.0, 900.0)
        );
        assert!(!app.window().is_maximized());
        assert!(
            !app.window().is_fullscreen(),
            "catalog does not inherit playback fullscreen"
        );
        assert!(player.playback_fullscreen_preference());

        storage::set_str(KEY, "unreadable snapshot");
        let protected = DesktopWindow::restore(&app, &player);
        protected.flush(&app, &player);
        assert_eq!(
            storage::get_str(KEY).as_deref(),
            Some("unreadable snapshot")
        );
    }

    #[test]
    fn maximized_size_does_not_replace_normal_size_or_playback_choice() {
        let mut state = Snapshot::default();
        assert!(state.maximized, "first desktop launch is maximized");
        state.playback_fullscreen = true;
        state.observe_geometry(1350.0, 850.0, false);
        state.observe_geometry(2560.0, 1440.0, true);
        assert_eq!((state.width, state.height), (1350.0, 850.0));
        assert!(state.maximized);
        assert!(state.playback_fullscreen);
        let restored: Snapshot =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored, state);
        state.observe_geometry(1350.0, 850.0, false);
        assert!(!state.maximized);
    }

    #[test]
    fn invalid_or_minimized_sizes_do_not_replace_normal_size() {
        let mut state = Snapshot::default();
        for (width, height) in [
            (0.0, 0.0),
            (f32::NAN, 800.0),
            (100000.0, 800.0),
            (350.0, 230.0),
        ] {
            state.observe_geometry(width, height, false);
            assert_eq!((state.width, state.height), (1100.0, 750.0));
        }
    }
}

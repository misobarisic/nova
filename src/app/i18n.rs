//! UI language (Settings → Display → Language).
//!
//! The interface strings are Slint's: user-facing literals in
//! `crates/ui/*.slint` are wrapped in `@tr("…")` (the Settings page is
//! translated; the other pages follow), and `crates/ui/build.rs` bundles the
//! gettext catalogs under `crates/ui/translations/<code>/` into the binary.
//! Switching language is therefore a single Slint call — it selects the catalog
//! whose folder name is the language code and marks every translation dirty, so
//! all `@tr` bindings re-evaluate in place. Rust-built model text is different:
//! after changing the backend language, the bridge refreshes those existing UI
//! models from their semantic state without restarting requests or the page.
//!
//! English is the source language, so `"en"` restores the built-in strings and
//! is the fallback whenever a catalog (or a single string) is missing; Croatian
//! (`hr`) is the first real catalog (Settings page + section headers).
//!
//! Applying the language is *not* part of the settings blob being loaded: the
//! catalogs are looked up in the UI crate, so the app mirrors the choice onto
//! the UI the same way `apply_animations` does (startup + after a save) rather
//! than in `nova-config`, which must stay UI-free.
use super::*;
use std::cell::RefCell;

/// Language names for the Settings → Display picker, in `Language::ALL` order.
/// Each is written in the language itself (`label()`), so they are shown as-is
/// in every language.
pub(crate) fn language_labels() -> Vec<SharedString> {
    Language::ALL
        .iter()
        .map(|language| SharedString::from(language.label()))
        .collect()
}

/// State of the bundled catalogs, so `apply_language` can be called from every
/// settings mirror (page open, autosave, sync apply, startup) without
/// re-selecting the same language or re-reporting a build that has none.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Catalogs {
    /// Nothing selected yet.
    #[default]
    Unset,
    /// A language is in place (Slint's selected catalog).
    Selected(&'static str),
    /// `select_bundled_translation` failed: this build has no catalogs at all,
    /// so stop trying — and stop logging — on this thread.
    ///
    /// That is the interpreter-backed `live-preview` UI (`cargo dev`): it
    /// compiles the `.slint` files at runtime and never registers a bundle
    /// (`set_bundled_languages` is only emitted by the ahead-of-time
    /// generator), so the strings stay English there.
    Unavailable,
}

impl Catalogs {
    /// Whether `code` still has to be pushed into Slint.
    fn needs_select(self, code: &str) -> bool {
        match self {
            Catalogs::Unset => true,
            Catalogs::Selected(selected) => selected != code,
            Catalogs::Unavailable => false,
        }
    }
}

thread_local! {
    /// See [`Catalogs`]. Thread-local, like Slint's own context: the selected
    /// catalog lives on the thread that created the window, so a window built
    /// on a fresh UI thread (an Android activity restart) starts from `Unset`
    /// and re-selects the stored language instead of skipping it as "already
    /// applied".
    static CATALOGS: RefCell<Catalogs> = const { RefCell::new(Catalogs::Unset) };
}

impl Bridge {
    /// Point Slint's catalogs at the stored language. Called on startup
    /// (through `settings_to_ui`) and after every settings save, so a pick in
    /// Settings → Display applies as soon as the page's autosave debounce
    /// lands — same cadence as the animation switches.
    pub(super) fn apply_language(&self, settings: &CacheSettings) {
        let code = settings.language.code();
        // Cheap no-op paths: the language is already in place, or this build
        // has no catalogs (never re-log those). Both leave the backend text
        // (`text.rs`) as the selection below set it.
        if !CATALOGS.with(|state| state.borrow().needs_select(code)) {
            return;
        }
        // No component yet (Android headless JNI paths) — nothing to
        // retranslate, but keep the backend text in step with what a window
        // would show.
        if self.app().is_none() {
            text::set_language(settings.language);
            return;
        }
        let mut failure = None;
        CATALOGS.with(|state| match slint::select_bundled_translation(code) {
            Ok(()) => {
                *state.borrow_mut() = Catalogs::Selected(code);
                // The backend-built text follows whatever the UI can show.
                text::set_language(settings.language);
            }
            Err(err) => {
                *state.borrow_mut() = Catalogs::Unavailable;
                text::set_language(Language::English);
                failure = Some(err);
            }
        });
        if let Some(err) = failure {
            eprintln!(
                "nova: UI translations are not bundled in this build ({err}) — \
                 the interface stays in English. The interpreter-backed UI \
                 (`cargo dev` / `live-preview`) has no catalogs; a regular \
                 `cargo build` / `cargo run` bundles them."
            );
        }
        self.refresh_language_dependent_ui();
    }

    /// Rust formats text before passing it to Slint, so existing model values
    /// must be refreshed explicitly when the selected language changes.
    fn refresh_language_dependent_ui(&self) {
        self.apply_category_rows();
        self.update_library_badges();
        self.refresh_home_language_text();
        self.refresh_addon_picker_language_text();
        self.apply_addon_rows();
        self.refresh_catalog_language_text();
        self.refresh_detail_language_text();

        // Settings surfaces whose text is formatted from live state.
        self.refresh_cache_disk_usage();
        self.refresh_torrent_disk_usage();
        self.downloads_list_to_ui();
        self.refresh_download_background_status();
        self.sync_status_to_ui();
        self.refresh_sync_link_notice();

        // Torrent buffering status is normally refreshed by the player tick;
        // update it immediately when the user changes language in Settings.
        self.note_torrent_progress_from_ui();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `apply_language` runs from every settings mirror, so the selection is
    /// skipped unless it actually changes — and a build without catalogs stops
    /// asking (and logging) after the first failure.
    #[test]
    fn catalog_selection_is_skipped_when_nothing_changes() {
        assert!(Catalogs::Unset.needs_select("en"));
        assert!(Catalogs::Unset.needs_select("hr"));

        assert!(!Catalogs::Selected("en").needs_select("en"));
        assert!(Catalogs::Selected("en").needs_select("hr"));
        assert!(Catalogs::Selected("hr").needs_select("en"));

        assert!(!Catalogs::Unavailable.needs_select("en"));
        assert!(!Catalogs::Unavailable.needs_select("hr"));
    }

    #[test]
    fn language_change_refreshes_rust_generated_library_text() {
        const TEST_DIR: &str = "NOVA_I18N_REFRESH_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-i18n-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args([
                    "--exact",
                    "app::i18n::tests::language_change_refreshes_rust_generated_library_text",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(root);
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
        app.window().set_size(slint::PhysicalSize::new(900, 700));
        app.window().show().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(Mutex::new(PosterStore::new(1))),
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );

        bridge.shared.lock().unwrap().entries = vec![LibraryEntry {
            id: "show-1".into(),
            type_: "movie".into(),
            name: "Show".into(),
            year: "2024".into(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: Vec::new(),
            description: String::new(),
            categories: Vec::new(),
            watch_status: WatchStatus::OnHold,
            added_at_secs: 1,
        }];

        let catalogs_available = slint::select_bundled_translation("hr").is_ok();
        let mut settings = CacheSettings {
            language: Language::English,
            ..CacheSettings::default()
        };
        bridge.apply_language(&settings);
        bridge.apply_library_to_ui();
        bridge.apply_category_rows();
        assert_eq!(app.get_library().row_data(0).unwrap().status, "On Hold");
        assert_eq!(app.get_library().row_data(0).unwrap().media_type, "Movie");

        settings.language = Language::Croatian;
        bridge.apply_language(&settings);
        let expected = if catalogs_available {
            "Na čekanju"
        } else {
            "On Hold"
        };
        assert_eq!(app.get_library().row_data(0).unwrap().status, expected);
        assert_eq!(
            app.get_library().row_data(0).unwrap().media_type,
            if catalogs_available { "Film" } else { "Movie" }
        );
        assert_eq!(
            app.get_library_category_labels()
                .row_data(
                    BUILTIN_FILTERS
                        .iter()
                        .position(|name| *name == "On Hold")
                        .unwrap()
                )
                .unwrap(),
            expected
        );

        settings.language = Language::English;
        bridge.apply_language(&settings);
        assert_eq!(app.get_library().row_data(0).unwrap().status, "On Hold");
        assert_eq!(app.get_library().row_data(0).unwrap().media_type, "Movie");
        assert_eq!(
            app.get_library_category_labels()
                .row_data(
                    BUILTIN_FILTERS
                        .iter()
                        .position(|name| *name == "On Hold")
                        .unwrap()
                )
                .unwrap(),
            "On Hold"
        );
    }
}

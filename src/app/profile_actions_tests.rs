//! Real profile activation and delayed ownership, isolated from the user's data.
use super::*;
use std::sync::mpsc;

struct Fixture {
    app: AppWindow,
    bridge: Bridge,
    _writer: Option<persistence::Worker>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Retained callbacks may still own draining sessions. Stop every actor
        // through its original allocation before the isolated database closes.
        let sessions = self.bridge.sessions.all();
        for session in &sessions {
            profiles::with_context(session.context(), || {
                self.bridge.shutdown_tracking();
                self.bridge.shutdown_stremio();
                self.bridge.shutdown_list_services();
            });
        }
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while sessions.iter().any(|session| !session.is_finished()) {
            assert!(
                std::time::Instant::now() < until,
                "profile actors must stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.bridge.sessions.reap();
        persistence::flush().unwrap();
    }
}

fn isolated(name: &str, test: impl FnOnce(&Fixture)) {
    const ROOT: &str = "NOVA_PROFILE_ACTIONS_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-profile-actions-{}-{}-{}",
            std::process::id(),
            nova_config::now_ms(),
            name.rsplit("::").next().unwrap()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ROOT, &root)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("APPDATA", root.join("data"))
            .env("LOCALAPPDATA", root.join("cache"))
            .args([
                "--exact",
                name.strip_prefix("nova::").unwrap_or(name),
                "--nocapture",
            ])
            .output()
            .unwrap();
        let completed = root.join("completed").is_file();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success() && completed,
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = PathBuf::from(root);
    i_slint_backend_testing::init_integration_test_with_mock_time();
    nova_storage::init_at(&root);
    profiles::initialize().unwrap();
    let app = AppWindow::new().unwrap();
    let player = crate::player::Player::setup(&app);
    #[cfg(feature = "desktop")]
    let (hi, _) = mpsc::channel();
    #[cfg(feature = "desktop")]
    let (lo, _) = mpsc::channel();
    #[cfg(feature = "desktop")]
    let bridge = Bridge::new(
        app.as_weak(),
        PosterTx { hi, lo },
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    #[cfg(not(feature = "desktop"))]
    let bridge = {
        Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        )
    };
    bridge::install_global_bridge(&bridge);
    persistence::register_collections();
    let writer = persistence::start();
    bridge.switch_profile(profiles::DEFAULT_ID);
    assert!(app.get_profiles_error().is_empty());
    let fixture = Fixture {
        app,
        bridge,
        _writer: writer,
    };
    test(&fixture);
    drop(fixture);
    std::fs::write(root.join("completed"), name).unwrap();
}

fn select(fixture: &Fixture, id: &str) {
    fixture.bridge.switch_profile(id);
    assert!(fixture.app.get_profiles_error().is_empty());
    assert_eq!(profiles::active_context().id, id);
    assert!(!fixture.app.get_profiles_require_selection());
}

fn save_personal_state(fixture: &Fixture, label: &str, language: nova_config::Language, rate: f32) {
    let entry: LibraryEntry = serde_json::from_value(serde_json::json!({
        "id":"shared-title", "type_":"movie", "name":label,
    }))
    .unwrap();
    let progress = EpisodeProgress {
        series_id: "shared-title".into(),
        episode_id: "shared-episode".into(),
        position_secs: if label == "Alice" { 20.0 } else { 70.0 },
        duration_secs: 100.0,
        updated_at_secs: 100,
        history: Some(PlaybackHistory {
            type_: "movie".into(),
            title: label.into(),
            played_at_secs: 100,
            ..Default::default()
        }),
        ..Default::default()
    };
    let (entries, progress, settings) = {
        let mut state = fixture.bridge.shared.lock().unwrap();
        state.entries = vec![entry];
        state.progress = HashMap::from([("shared-episode".into(), progress)]);
        state.cache_settings.language = language;
        state.cache_settings.playback_speed = rate;
        (
            state.entries.clone(),
            state.progress.clone(),
            state.cache_settings.clone(),
        )
    };
    write_persisted_library(&entries);
    write_progress_map(&progress);
    write_settings(&settings);
    persist_sync_snapshot(
        "plugin_settings",
        &serde_json::json!({"example":{"token":label}}).to_string(),
        false,
    )
    .unwrap();
    persistence::flush().unwrap();
}

fn assert_personal_state(
    fixture: &Fixture,
    label: &str,
    language: nova_config::Language,
    rate: f32,
) {
    let state = fixture.bridge.shared.lock().unwrap();
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.entries[0].name, label);
    assert_eq!(
        state.progress["shared-episode"]
            .history
            .as_ref()
            .unwrap()
            .title,
        label
    );
    assert_eq!(state.cache_settings.language, language);
    assert_eq!(state.cache_settings.playback_speed, rate);
    drop(state);
    let raw = storage::try_get_str("plugin_settings").unwrap().unwrap();
    let values: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(values["example"]["token"], label);
    assert_eq!(fixture.app.get_language_index(), language.index());
}

#[test]
fn create_switch_and_return_restores_personal_state_and_addon_configuration() {
    isolated(
        concat!(
            module_path!(),
            "::create_switch_and_return_restores_personal_state_and_addon_configuration"
        ),
        |fixture| {
            save_personal_state(fixture, "Alice", nova_config::Language::Croatian, 1.25);
            fixture.bridge.create_profile("Bob");
            let bob = profiles::list()
                .unwrap()
                .into_iter()
                .find(|row| row.name == "Bob")
                .unwrap()
                .id;
            fixture.app.set_addon_browser_tab(1);
            fixture.app.set_addon_browser_source(1);
            fixture
                .app
                .set_addon_browser_directory_url("https://alice.example/manifest.json".into());
            fixture.app.set_addon_browser_search("alice".into());
            fixture.app.set_addon_browser_status("alice status".into());
            fixture.app.set_addon_browser_rows(
                Rc::new(VecModel::from(vec![crate::AddonBrowserRow {
                    label: "Alice directory entry".into(),
                    url: "https://alice.example/addon".into(),
                    ..Default::default()
                }]))
                .into(),
            );
            select(fixture, &bob);
            assert_eq!(fixture.app.get_addon_browser_tab(), 0);
            assert_eq!(fixture.app.get_addon_browser_source(), 0);
            assert!(fixture.app.get_addon_browser_directory_url().is_empty());
            assert!(fixture.app.get_addon_browser_search().is_empty());
            assert!(fixture.app.get_addon_browser_status().is_empty());
            assert_eq!(fixture.app.get_addon_browser_rows().row_count(), 0);
            {
                let state = fixture.bridge.shared.lock().unwrap();
                assert!(state.entries.is_empty());
                assert!(state.progress.is_empty());
                assert_eq!(
                    state.cache_settings.language,
                    nova_config::Language::English
                );
                assert_eq!(state.cache_settings.playback_speed, 1.0);
            }
            assert!(
                storage::try_get_str("plugin_settings")
                    .unwrap()
                    .is_none_or(|raw| raw == "{}")
            );
            save_personal_state(fixture, "Bob", nova_config::Language::English, 1.75);
            select(fixture, profiles::DEFAULT_ID);
            assert_personal_state(fixture, "Alice", nova_config::Language::Croatian, 1.25);
            select(fixture, &bob);
            assert_personal_state(fixture, "Bob", nova_config::Language::English, 1.75);

            // Exercise opaque external-addon configuration without installing
            // manifests or starting network requests in this activation test.
            for (id, token) in [(profiles::DEFAULT_ID, "alice"), (bob.as_str(), "bob")] {
                profiles::with_context(profiles::context_for(id), || {
                    write_persisted_addons(&[AddonStore {
                        capabilities: None,
                        url: format!("https://example.invalid/{token}/manifest.json"),
                        enabled: false,
                        configure_ok: Some(true),
                        label: token.into(),
                    }]);
                });
            }
            for (id, token) in [(profiles::DEFAULT_ID, "alice"), (bob.as_str(), "bob")] {
                profiles::with_context(profiles::context_for(id), || {
                    let addons = read_persisted_addons();
                    assert_eq!(addons.len(), 1);
                    assert_eq!(addons[0].label, token);
                    assert!(addons[0].url.contains(&format!("/{token}/")));
                });
            }
        },
    );
}

fn drain_account_queue() {
    let drained = Arc::new(AtomicBool::new(false));
    let receipt = drained.clone();
    profiles::invoke_account(move || {
        receipt.store(true, Ordering::Release);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(drained.load(Ordering::Acquire));
}

#[test]
fn captured_workers_keep_storage_owner_and_stale_ui_callbacks_never_publish() {
    isolated(
        concat!(
            module_path!(),
            "::captured_workers_keep_storage_owner_and_stale_ui_callbacks_never_publish"
        ),
        |fixture| {
            let alice = profiles::capture();
            let bob = profiles::create("Bob").unwrap();
            let (ready, started) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let published = Arc::new(AtomicBool::new(false));
            let stale = published.clone();
            let expected = alice.clone();
            let worker = profiles::thread::spawn(move || {
                ready.send(()).unwrap();
                released.recv().unwrap();
                assert_eq!(profiles::capture(), expected);
                assert!(!profiles::is_current(&expected));
                storage::try_set_str("discover:search_history:v2", "[\"Alice worker\"]").unwrap();
                profiles::invoke_from_event_loop(move || stale.store(true, Ordering::Release))
                    .unwrap();
            });
            started.recv().unwrap();
            select(fixture, &bob);
            release.send(()).unwrap();
            worker.join().unwrap();
            drain_account_queue();
            assert!(!published.load(Ordering::Acquire));
            assert!(
                storage::try_get_str("discover:search_history:v2")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                nova_storage::try_get_str(&profiles::storage_key_for(
                    &alice.id,
                    "discover:search_history:v2"
                ))
                .unwrap()
                .as_deref(),
                Some("[\"Alice worker\"]")
            );
            select(fixture, &alice.id);
            assert_ne!(profiles::active_context().epoch, alice.epoch);
            let stale = published.clone();
            profiles::with_context(alice, || {
                profiles::invoke_from_event_loop(move || stale.store(true, Ordering::Release))
                    .unwrap();
            });
            drain_account_queue();
            assert!(
                !published.load(Ordering::Acquire),
                "returning to the same ID must not revive an old epoch"
            );
        },
    );
}

fn sync_rows() -> Vec<(String, String, String)> {
    let owner = nova_sync::local_store().unwrap();
    let store = owner.lock().unwrap();
    let mut rows = Vec::new();
    for domain in store.domains() {
        for (key, _) in store.records(&domain) {
            rows.push((
                domain.clone(),
                key.clone(),
                format!("{:?}", store.record(&domain, &key).unwrap()),
            ));
        }
    }
    rows.sort();
    rows
}

#[test]
fn failed_selection_write_keeps_outgoing_playback_and_profile_active() {
    isolated(
        concat!(
            module_path!(),
            "::failed_selection_write_keeps_outgoing_playback_and_profile_active"
        ),
        |fixture| {
            let active = profiles::active_context();
            let bob = profiles::create("Bob").unwrap();
            let capture = fixture.bridge.player.screenshot_session();
            fixture.app.set_player_open(true);
            // Reject just the selection write through the real storage schema
            // validator; all outgoing checkpoints must still be allowed.
            nova_storage::register_collection(
                "profiles:device:v1",
                nova_storage::CollectionKind::Array {
                    id_field: "id".into(),
                },
            );
            fixture.bridge.switch_profile(&bob);
            assert!(!fixture.app.get_profiles_error().is_empty());
            assert_eq!(profiles::active_context(), active);
            assert_eq!(profiles::device_preferences().active_profile, active.id);
            assert!(fixture.app.get_player_open());
            assert_eq!(fixture.bridge.player.screenshot_session(), capture);

            nova_storage::register_collection(
                "profiles:device:v1",
                nova_storage::CollectionKind::Object,
            );
            select(fixture, &bob);
            assert!(!fixture.app.get_player_open());
        },
    );
}

#[test]
fn startup_preference_is_device_local_and_survives_profile_switches() {
    isolated(
        concat!(
            module_path!(),
            "::startup_preference_is_device_local_and_survives_profile_switches"
        ),
        |fixture| {
            assert!(profiles::device_preferences().open_last_profile_on_startup);
            let active = profiles::active_context();
            let before = sync_rows();
            let settings = storage::try_get_str("settings").unwrap();
            fixture.bridge.remember_profile_on_startup(false);
            assert!(fixture.app.get_profiles_error().is_empty());
            assert!(!fixture.app.get_profiles_remember_last());
            assert_eq!(profiles::active_context(), active);
            assert_eq!(storage::try_get_str("settings").unwrap(), settings);
            assert_eq!(
                sync_rows(),
                before,
                "local startup preference cannot create sync actions"
            );
            let persisted: profiles::DevicePreferences = serde_json::from_str(
                &nova_storage::try_get_str("profiles:device:v1")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert!(!persisted.open_last_profile_on_startup);
            assert_eq!(persisted.active_profile, active.id);
            let bob = profiles::create("Bob").unwrap();
            select(fixture, &bob);
            assert!(!profiles::device_preferences().open_last_profile_on_startup);
            assert!(!fixture.app.get_profiles_remember_last());
            let restored = profiles::initialize().unwrap();
            assert_eq!(restored.active_profile, bob);
            assert!(!restored.open_last_profile_on_startup);
            assert!(profiles::DevicePreferences::default().open_last_profile_on_startup);
        },
    );
}

#[test]
fn deleted_profiles_reject_late_writes_and_the_last_profile_is_preserved() {
    isolated(
        concat!(
            module_path!(),
            "::deleted_profiles_reject_late_writes_and_the_last_profile_is_preserved"
        ),
        |fixture| {
            let alice = profiles::capture();
            let bob = profiles::create("Bob").unwrap();
            storage::try_set_str("plugin_settings", r#"{"example":{"token":"private"}}"#).unwrap();
            let (release, released) = mpsc::channel();
            let worker = profiles::thread::spawn(move || {
                released.recv().unwrap();
                storage::try_set_str("plugin_settings", r#"{"example":{"token":"late"}}"#)
            });
            select(fixture, &bob);
            fixture.bridge.delete_profile(&alice.id);
            assert!(fixture.app.get_profiles_error().is_empty());
            release.send(()).unwrap();
            assert!(worker.join().unwrap().is_err());
            assert!(
                nova_storage::try_scan_prefix(&format!("profile:{}:", alice.id))
                    .unwrap()
                    .is_empty()
            );
            let owner = nova_sync::local_store().unwrap();
            assert!(
                owner
                    .lock()
                    .unwrap()
                    .record("profile_deletions", &alice.id)
                    .is_some_and(|record| !record.is_deleted())
            );
            assert_eq!(profiles::list().unwrap().len(), 1);
            fixture.bridge.delete_profile(&bob);
            assert!(!fixture.app.get_profiles_error().is_empty());
            assert_eq!(profiles::list().unwrap()[0].id, bob);
            assert_eq!(profiles::active_context().id, bob);
            storage::try_set_str("plugin_settings", "{}").unwrap();
        },
    );
}

#[test]
fn captured_session_keeps_its_allocation_across_switch_and_same_id_return() {
    isolated(
        concat!(
            module_path!(),
            "::captured_session_keeps_its_allocation_across_switch_and_same_id_return"
        ),
        |fixture| {
            let old = profiles::capture();
            let weak = Arc::downgrade(old.session.as_ref().unwrap());
            fixture.bridge.shared.lock().unwrap().search = "old Alice allocation".into();
            let bob = profiles::create("Bob").unwrap();
            select(fixture, &bob);
            assert!(
                old.session
                    .as_ref()
                    .unwrap()
                    .cancelled
                    .load(Ordering::Acquire)
            );
            assert!(!Arc::ptr_eq(
                old.session.as_ref().unwrap(),
                &fixture.bridge.sessions.active()
            ));
            profiles::with_context(old.clone(), || {
                assert_eq!(
                    fixture.bridge.shared.lock().unwrap().search,
                    "old Alice allocation"
                );
                fixture.bridge.shared.lock().unwrap().search = "old Alice callback".into();
            });
            assert!(fixture.bridge.shared.lock().unwrap().search.is_empty());
            select(fixture, &old.id);
            let current = fixture.bridge.sessions.active();
            assert!(!Arc::ptr_eq(old.session.as_ref().unwrap(), &current));
            assert_ne!(current.epoch, old.epoch);
            assert!(fixture.bridge.shared.lock().unwrap().search.is_empty());
            let callback: Box<dyn Fn()> = {
                let owner = old.clone();
                let bridge = fixture.bridge.clone();
                Box::new(move || {
                    profiles::with_context(owner.clone(), || {
                        assert_eq!(bridge.shared.lock().unwrap().search, "old Alice callback");
                        bridge.shared.lock().unwrap().search = "retained callback finished".into();
                    });
                })
            };
            drop(old);
            callback();
            assert!(fixture.bridge.shared.lock().unwrap().search.is_empty());
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while !weak.upgrade().unwrap().is_finished() {
                assert!(
                    std::time::Instant::now() < until,
                    "outgoing session actors must drain"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            fixture.bridge.sessions.reap();
            drain_account_queue();
            assert!(
                weak.upgrade().is_some(),
                "the retained callback owns the old allocation"
            );
            drop(callback);
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while weak.upgrade().is_some() {
                assert!(
                    std::time::Instant::now() < until,
                    "dropping the last callback must release the old session"
                );
                drain_account_queue();
                std::thread::sleep(Duration::from_millis(10));
            }
        },
    );
}

#[test]
fn durable_only_context_can_write_its_store_but_cannot_access_live_shared_state() {
    isolated(
        concat!(
            module_path!(),
            "::durable_only_context_can_write_its_store_but_cannot_access_live_shared_state"
        ),
        |fixture| {
            let active = fixture.bridge.sessions.active();
            let owner = profiles::ProfileStore::for_profile(&active.id);
            owner.run(|| {
                assert!(profiles::capture().session.is_none());
                assert!(fixture.bridge.shared.lock().is_err());
                storage::try_set_str("tracking:test:durable-owner", "retained").unwrap();
            });
            assert_eq!(
                owner
                    .read("tracking:test:durable-owner")
                    .unwrap()
                    .as_deref(),
                Some("retained")
            );
            assert!(Arc::ptr_eq(&active, &fixture.bridge.sessions.active()));
            assert!(fixture.bridge.shared.lock().is_ok());
        },
    );
}

#[test]
fn activation_preflight_keeps_the_active_session_when_incoming_data_is_corrupt() {
    isolated(
        concat!(
            module_path!(),
            "::activation_preflight_keeps_the_active_session_when_incoming_data_is_corrupt"
        ),
        |fixture| {
            save_personal_state(fixture, "Alice", nova_config::Language::English, 1.25);
            fixture.bridge.apply_library_to_ui();
            fixture.app.set_profiles_open(true);
            fixture.app.set_more_open(true);
            let active = fixture.bridge.sessions.active();
            let device = nova_storage::try_get_str("profiles:device:v1").unwrap();
            let library = fixture.app.get_library().row_data(0).unwrap();
            let generation = nova_providers::plugin_settings::settings_generation();
            for key in ["library", "plugin_settings"] {
                let incoming = profiles::create(&format!("Broken {key}")).unwrap();
                let owner = profiles::ProfileStore::for_profile(&incoming);
                owner.write(key, "{broken JSON").unwrap();
                fixture.bridge.switch_profile(&incoming);
                assert!(
                    !fixture.app.get_profiles_error().is_empty(),
                    "{key} must fail preflight"
                );
                assert!(Arc::ptr_eq(&active, &fixture.bridge.sessions.active()));
                assert_eq!(profiles::active_context(), active.context());
                assert!(!active.cancelled.load(Ordering::Acquire));
                assert_eq!(
                    nova_storage::try_get_str("profiles:device:v1").unwrap(),
                    device
                );
                assert!(fixture.app.get_profiles_open() && fixture.app.get_more_open());
                assert!(!fixture.app.get_profiles_require_selection());
                let current = fixture.app.get_library().row_data(0).unwrap();
                // Slint image handles have identity-based equality, including
                // placeholder handles. Compare the visible row's semantic data.
                assert_eq!(fixture.app.get_library().row_count(), 1);
                assert_eq!(current.id, library.id);
                assert_eq!(current.title, library.title);
                assert_eq!(current.status, library.status);
                assert_eq!(current.badge, library.badge);
                assert_eq!(current.poster_path, library.poster_path);
                assert_eq!(
                    fixture.bridge.shared.lock().unwrap().entries[0].name,
                    "Alice"
                );
                assert_eq!(
                    fixture
                        .bridge
                        .shared
                        .lock()
                        .unwrap()
                        .cache_settings
                        .playback_speed,
                    1.25
                );
                assert_eq!(
                    nova_providers::plugin_settings::settings_generation(),
                    generation
                );
                assert_eq!(
                    owner.read(key).unwrap().as_deref(),
                    Some("{broken JSON"),
                    "unreadable incoming data is retained"
                );
            }
        },
    );
}

fn packaged_addon_fixture() -> Vec<u8> {
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};
    let plugin = serde_json::json!({
        "formatVersion":1,"id":"profilefixture","name":"Profile fixture","version":"1.0.0",
        "entry":"index.js","permissions":{},
    });
    let addon =
        serde_json::json!({"id":"nova.profilefixture","name":"Profile fixture","version":"1.0.0"});
    let mut archive = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&plugin).unwrap()),
        ("stremio-manifest.json", serde_json::to_vec(&addon).unwrap()),
        ("LICENSE", b"fixture license".to_vec()),
        ("NOTICE.md", b"fixture notice".to_vec()),
        (
            "index.js",
            b"globalThis.novaProvider = {handle(request) { return request.manifest || []; }};"
                .to_vec(),
        ),
    ] {
        archive
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(&bytes).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

#[test]
fn packaged_addon_registry_is_restored_before_reinstalling_its_profile_rows() {
    isolated(
        concat!(
            module_path!(),
            "::packaged_addon_registry_is_restored_before_reinstalling_its_profile_rows"
        ),
        |fixture| {
            let url = "nova-provider://profilefixture";
            fixture.bridge.import_addon_zip(packaged_addon_fixture());
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while fixture.app.get_addon_zip_import_busy() {
                assert!(
                    std::time::Instant::now() < until,
                    "package import must finish"
                );
                std::thread::sleep(Duration::from_millis(10));
                drain_account_queue();
            }
            assert!(nova_providers::has_installed_provider(url));
            assert!(
                fixture
                    .bridge
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .any(|addon| addon.url == url && addon.available)
            );
            let bob = profiles::create("Bob").unwrap();
            select(fixture, &bob);
            assert!(!nova_providers::has_installed_provider(url));
            assert!(fixture.bridge.shared.lock().unwrap().installed.is_empty());
            select(fixture, profiles::DEFAULT_ID);
            assert!(
                nova_providers::has_installed_provider(url),
                "profile activation restores the immutable package registry"
            );
            let state = fixture.bridge.shared.lock().unwrap();
            assert_eq!(state.installed.len(), 1);
            assert_eq!(state.installed[0].url, url);
            assert!(
                state.installed[0].available,
                "restored package rows must not start as unavailable"
            );
            assert!(state.loaded_packages.contains_key("profilefixture"));
        },
    );
}

#[test]
fn incoming_unsupported_setting_protection_stays_with_its_durable_profile() {
    isolated(
        concat!(
            module_path!(),
            "::incoming_unsupported_setting_protection_stays_with_its_durable_profile"
        ),
        |fixture| {
            fixture.bridge.persist_settings();
            let alice = profiles::ProfileStore::for_profile(profiles::DEFAULT_ID);
            let bob = profiles::create("Bob").unwrap();
            let incoming = profiles::ProfileStore::for_profile(&bob);
            let mut raw = serde_json::to_value(CacheSettings::default()).unwrap();
            raw["language"] = "future-language".into();
            raw["date_relative"] = false.into();
            incoming.write("settings", &raw.to_string()).unwrap();
            // Recovery records protected fields before the later library
            // validation fails. The active profile must never inherit them.
            incoming.write("library", "{broken JSON").unwrap();
            fixture.bridge.switch_profile(&bob);
            assert!(!fixture.app.get_profiles_error().is_empty());
            assert_eq!(profiles::active_context().id, profiles::DEFAULT_ID);
            fixture
                .bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .date_relative = true;
            fixture.bridge.persist_settings();
            let saved: serde_json::Value =
                serde_json::from_str(&alice.read("settings").unwrap().unwrap()).unwrap();
            assert_eq!(
                saved["language"],
                serde_json::to_value(nova_config::Language::English).unwrap()
            );
            assert_eq!(saved["date_relative"], true);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    &incoming.read("settings").unwrap().unwrap()
                )
                .unwrap()["language"],
                "future-language"
            );

            incoming.write("library", "[]").unwrap();
            select(fixture, &bob);
            assert_eq!(
                fixture
                    .bridge
                    .shared
                    .lock()
                    .unwrap()
                    .cache_settings
                    .language,
                nova_config::Language::English
            );
            assert!(
                !fixture
                    .bridge
                    .shared
                    .lock()
                    .unwrap()
                    .cache_settings
                    .date_relative
            );
            fixture
                .bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .date_relative = true;
            fixture.bridge.persist_settings();
            let saved: serde_json::Value =
                serde_json::from_str(&incoming.read("settings").unwrap().unwrap()).unwrap();
            assert_eq!(
                saved["language"], "future-language",
                "unrelated edits preserve incoming unsupported intent"
            );
            assert_eq!(saved["date_relative"], true);
            select(fixture, profiles::DEFAULT_ID);
            let saved: serde_json::Value =
                serde_json::from_str(&alice.read("settings").unwrap().unwrap()).unwrap();
            assert_eq!(
                saved["language"],
                serde_json::to_value(nova_config::Language::English).unwrap()
            );
        },
    );
}

#[test]
fn stale_network_work_completes_synchronously_without_transport_and_releases_permits() {
    isolated(
        concat!(
            module_path!(),
            "::stale_network_work_completes_synchronously_without_transport_and_releases_permits"
        ),
        |fixture| {
            struct Permit(Arc<AtomicUsize>);
            impl Drop for Permit {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!(
                "http://{}/must-not-be-requested",
                listener.local_addr().unwrap()
            );
            let old = profiles::capture();
            fixture.bridge.shared.lock().unwrap().search = "outgoing network owner".into();
            let bob = profiles::create("Bob").unwrap();
            select(fixture, &bob);
            let pending = Arc::new(AtomicUsize::new(3));
            let completions = Arc::new(AtomicUsize::new(0));
            let progress = Arc::new(AtomicUsize::new(0));
            let publications = Arc::new(AtomicUsize::new(0));
            profiles::with_context(old.clone(), || {
                let permit = Permit(pending.clone());
                let count = completions.clone();
                let publish = publications.clone();
                let expected = old.clone();
                let bridge = fixture.bridge.clone();
                profile_net::fetch_bytes(url.clone(), move |result| {
                    let _permit = permit;
                    let error = result.unwrap_err();
                    assert_eq!(error.status, None);
                    assert_eq!(error.message, "Profile session ended");
                    assert_eq!(profiles::capture(), expected);
                    assert_eq!(
                        bridge.shared.lock().unwrap().search,
                        "outgoing network owner"
                    );
                    count.fetch_add(1, Ordering::AcqRel);
                    profiles::invoke_from_event_loop(move || {
                        publish.fetch_add(1, Ordering::AcqRel);
                    })
                    .unwrap();
                });
                assert_eq!(
                    pending.load(Ordering::Acquire),
                    2,
                    "byte completion releases its permit before fetch returns"
                );

                let permit = Permit(pending.clone());
                let count = completions.clone();
                let publish = publications.clone();
                let expected = old.clone();
                let received = progress.clone();
                profile_net::fetch_bytes_with_progress(
                    url.clone(),
                    Arc::new(move |_| {
                        received.fetch_add(1, Ordering::AcqRel);
                    }),
                    move |result| {
                        let _permit = permit;
                        assert!(result.is_err());
                        assert_eq!(profiles::capture(), expected);
                        count.fetch_add(1, Ordering::AcqRel);
                        profiles::invoke_from_event_loop(move || {
                            publish.fetch_add(1, Ordering::AcqRel);
                        })
                        .unwrap();
                    },
                );
                assert_eq!(
                    pending.load(Ordering::Acquire),
                    1,
                    "progress-fetch completion releases its permit synchronously"
                );

                let permit = Permit(pending.clone());
                let count = completions.clone();
                let publish = publications.clone();
                let expected = old.clone();
                profile_net::fetch_image(
                    url,
                    nova_media::cache::ImageUse::DetailPoster,
                    move |result| {
                        let _permit = permit;
                        assert!(result.is_none());
                        assert_eq!(profiles::capture(), expected);
                        count.fetch_add(1, Ordering::AcqRel);
                        profiles::invoke_from_event_loop(move || {
                            publish.fetch_add(1, Ordering::AcqRel);
                        })
                        .unwrap();
                    },
                );
                assert_eq!(
                    pending.load(Ordering::Acquire),
                    0,
                    "image completion releases its permit before fetch returns"
                );
            });
            assert_eq!(completions.load(Ordering::Acquire), 3);
            assert_eq!(progress.load(Ordering::Acquire), 0);
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
            drain_account_queue();
            assert_eq!(
                publications.load(Ordering::Acquire),
                0,
                "stale completion bookkeeping must not publish into the selected UI"
            );
            assert_eq!(profiles::active_context().id, bob);
            assert!(fixture.bridge.shared.lock().unwrap().search.is_empty());
        },
    );
}

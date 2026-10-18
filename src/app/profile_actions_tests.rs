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
        // Activation retains stopped service generations until their workers
        // exit. Shut down every captured owner, not merely the current one.
        let owners: Vec<_> = self
            .bridge
            .tracking_services
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .chain(self.bridge.stremio_services.lock().unwrap().keys().cloned())
            .collect();
        for context in owners {
            profiles::with_context(context, || {
                self.bridge.shutdown_tracking();
                self.bridge.shutdown_stremio();
            });
        }
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let tracking = self
                .bridge
                .tracking_services
                .lock()
                .unwrap()
                .values()
                .all(|handle| handle.is_finished());
            let stremio = self
                .bridge
                .stremio_services
                .lock()
                .unwrap()
                .values()
                .all(|handle| handle.is_finished());
            if tracking && stremio {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "profile actors must stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
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
            select(fixture, &bob);
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

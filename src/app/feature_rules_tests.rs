//! Production settings/sync regressions for Library and stream rule fields.
//! A fresh subprocess owns Slint, profile selection and redb for this case.
use super::*;
use nova_config::stream_filters::{StreamFilterRules, VideoCodec};
use nova_config::{SmartCollection, SmartMediaType, SmartWatchState};

fn collection(id: &str, name: &str, genre: &str) -> SmartCollection {
    SmartCollection {
        id: id.into(),
        name: name.into(),
        media_type: SmartMediaType::Movies,
        watch_state: SmartWatchState::Unwatched,
        genre: genre.into(),
        max_runtime_minutes: Some(120),
    }
}
fn filters(height: u16, codec: VideoCodec, language: &str) -> StreamFilterRules {
    StreamFilterRules {
        enabled: true,
        max_height: Some(height),
        max_bytes: Some(4_000_000_000),
        video_codecs: vec![codec],
        audio_languages: vec![language.into()],
        require_audio_language: true,
        retain_unknown: false,
    }
}
fn settings_values(settings: &CacheSettings) -> [(String, String); 2] {
    [
        (
            "smart_collections".into(),
            serde_json::to_string(&settings.smart_collections).unwrap(),
        ),
        (
            "stream_filters".into(),
            serde_json::to_string(&settings.stream_filters).unwrap(),
        ),
    ]
}
fn read_owned(owner: &profiles::ProfileStore) -> CacheSettings {
    serde_json::from_str(
        &owner
            .read("settings")
            .unwrap()
            .expect("profile settings persisted"),
    )
    .unwrap()
}
fn assert_rules(actual: &CacheSettings, expected: &CacheSettings) {
    assert_eq!(actual.smart_collections, expected.smart_collections);
    assert_eq!(actual.stream_filters, expected.stream_filters);
}
fn records(domain: &str) -> Vec<(String, nova_sync::Record)> {
    let owner = nova_sync::local_store().unwrap();
    let store = owner.lock().unwrap();
    ["smart_collections", "stream_filters"]
        .into_iter()
        .map(|field| {
            let mut record = store.record(domain, field).unwrap().clone();
            // Settings publication canonicalizes object keys. Compare semantic
            // payloads while retaining the exact record version/clock checks.
            record.value = record.value.map(|raw| {
                let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                serde_json::to_string(&value).unwrap()
            });
            (field.into(), record)
        })
        .collect()
}
fn apply_remote(domain: &str, settings: &CacheSettings, offset_ms: u64) {
    let owner = nova_sync::local_store().unwrap();
    let mut store = owner.lock().unwrap();
    let version = nova_sync::Version::new(nova_config::now_ms() + offset_ms, 0, 999_999, false);
    for (field, value) in settings_values(settings) {
        store.apply(
            domain,
            &field,
            nova_sync::Record {
                value: Some(value),
                version,
            },
        );
    }
    store.save().unwrap();
}

#[test]
fn feature_rules_are_profile_owned_and_keep_device_overrides_across_remote_apply() {
    const ENV: &str = "NOVA_FEATURE_RULES_SYNC_TEST_ROOT";
    let Some(root) = std::env::var_os(ENV) else {
        let root = std::env::temp_dir().join(format!(
            "nova-feature-rules-sync-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ENV, &root)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .args(["--exact", "app::feature_rules_tests::feature_rules_are_profile_owned_and_keep_device_overrides_across_remote_apply", "--nocapture", "--test-threads=1"])
            .output().unwrap();
        let receipt = std::fs::read_to_string(root.join("case-complete")).ok();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            receipt.as_deref(),
            Some("ok"),
            "the exact production regression must run"
        );
        return;
    };
    let root = PathBuf::from(root);
    i_slint_backend_testing::init_integration_test_with_mock_time();
    nova_storage::init_at(&root.join("store"));
    profiles::initialize().unwrap();
    let alice_id = profiles::current_id();
    let alice_owner = profiles::ProfileStore::capture();
    let bob_id = profiles::create("Bob").unwrap();
    let bob_owner = profiles::ProfileStore::for_profile(&bob_id);
    let alice_domain = profiles::domain_for(&alice_id, sync::DOMAIN_SETTINGS);
    let bob_domain = profiles::domain_for(&bob_id, sync::DOMAIN_SETTINGS);
    assert_ne!(alice_owner.key("settings"), bob_owner.key("settings"));

    let app = AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
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

    let initial = CacheSettings {
        smart_collections: vec![collection("alice-scifi", "Alice sci-fi", "Sci-Fi")],
        stream_filters: filters(1080, VideoCodec::Avc, "en"),
        ..CacheSettings::default()
    };
    bridge.shared.lock().unwrap().cache_settings = initial.clone();
    set_active_cache_settings(initial.clone());
    bridge.settings_to_ui();
    bridge.persist_settings();
    let initial_records = records(&alice_domain);
    for (field, expected) in settings_values(&initial) {
        assert!(
            sync::setting_can_override(&field),
            "new field participates in normal override policy"
        );
        let actual = initial_records
            .iter()
            .find(|(name, _)| name == &field)
            .unwrap()
            .1
            .value
            .as_deref()
            .expect("live shared rule record");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(actual).unwrap(),
            serde_json::from_str::<serde_json::Value>(&expected).unwrap(),
            "{field} shared payload matches the effective setting"
        );
    }

    // An unrelated ordinary Settings capture must keep rule fields whose
    // editors live outside Settings, rather than reconstructing defaults.
    app.set_cache_quality(83.0);
    bridge.capture_settings();
    bridge.persist_settings();
    assert_rules(&bridge.shared.lock().unwrap().cache_settings, &initial);
    assert_rules(&read_owned(&alice_owner), &initial);
    assert_eq!(
        records(&alice_domain),
        initial_records,
        "unrelated capture must not restamp either rule field"
    );

    // Another profile owns a distinct settings snapshot and account domain.
    let bob_initial = CacheSettings {
        smart_collections: vec![collection("bob-drama", "Bob drama", "Drama")],
        stream_filters: filters(720, VideoCodec::Hevc, "hr"),
        ..CacheSettings::default()
    };
    bob_owner.run(|| write_settings(&bob_initial));
    assert_rules(&read_owned(&bob_owner), &bob_initial);
    assert_rules(&read_owned(&alice_owner), &initial);
    assert_eq!(profiles::current_id(), alice_id);
    assert_eq!(records(&alice_domain), initial_records);

    for field in ["smart_collections", "stream_filters"] {
        bridge.set_setting_override(field, true);
    }
    let local = CacheSettings {
        smart_collections: vec![collection("local-comedy", "Only this device", "Comedy")],
        stream_filters: filters(480, VideoCodec::Av1, "ja"),
        ..initial.clone()
    };
    {
        let mut state = bridge.shared.lock().unwrap();
        state.cache_settings.smart_collections = local.smart_collections.clone();
        state.cache_settings.stream_filters = local.stream_filters.clone();
    }
    bridge.settings_to_ui();
    bridge.capture_settings();
    bridge.persist_settings();
    assert_rules(&read_owned(&alice_owner), &local);
    assert!(
        read_owned(&bob_owner).sync_overrides.is_empty(),
        "override policy is profile-specific"
    );
    assert_eq!(
        records(&alice_domain),
        initial_records,
        "device override values must never publish or change shared record clocks"
    );

    let shared = CacheSettings {
        smart_collections: vec![collection(
            "remote-thriller",
            "From paired device",
            "Thriller",
        )],
        stream_filters: filters(2160, VideoCodec::Hevc, "fr"),
        ..initial.clone()
    };
    apply_remote(&alice_domain, &shared, 60_000);
    let received_records = records(&alice_domain);
    bridge.sync_apply(vec![alice_domain.clone()]);
    assert_rules(&bridge.shared.lock().unwrap().cache_settings, &local);
    let effective = read_owned(&alice_owner);
    assert_rules(&effective, &local);
    assert_eq!(
        effective.sync_overrides["smart_collections"],
        serde_json::to_value(&shared.smart_collections).unwrap()
    );
    assert_eq!(
        effective.sync_overrides["stream_filters"],
        serde_json::to_value(&shared.stream_filters).unwrap()
    );
    assert_eq!(
        records(&alice_domain),
        received_records,
        "remote apply must not echo either feature as a new local action"
    );
    assert_rules(&read_owned(&bob_owner), &bob_initial);

    // Disabling the override restores the latest received account value,
    // rather than the older baseline captured when the override was enabled.
    for field in ["smart_collections", "stream_filters"] {
        bridge.set_setting_override(field, false);
    }
    let restored = read_owned(&alice_owner);
    assert_rules(&restored, &shared);
    assert!(!restored.sync_overrides.contains_key("smart_collections"));
    assert!(!restored.sync_overrides.contains_key("stream_filters"));
    assert_eq!(records(&alice_domain), received_records);
    bridge.capture_settings();
    bridge.persist_settings();
    assert_rules(&read_owned(&alice_owner), &shared);
    assert_eq!(records(&alice_domain), received_records);

    // Incoming inactive-profile records project to that profile's durable
    // store without replacing the current profile's effective values or UI.
    let bob_remote = CacheSettings {
        smart_collections: vec![collection("bob-remote", "Bob mysteries", "Mystery")],
        stream_filters: filters(1080, VideoCodec::Vp9, "de"),
        ..bob_initial
    };
    apply_remote(&bob_domain, &bob_remote, 120_000);
    let bob_received = records(&bob_domain);
    let owner = nova_sync::local_store().unwrap();
    let digest = owner.lock().unwrap().digest();
    bridge.sync_apply(vec![bob_domain.clone()]);
    assert_rules(&read_owned(&bob_owner), &bob_remote);
    assert_rules(&read_owned(&alice_owner), &shared);
    assert_rules(&bridge.shared.lock().unwrap().cache_settings, &shared);
    assert_eq!(profiles::current_id(), alice_id);
    assert_eq!(records(&alice_domain), received_records);
    assert_eq!(records(&bob_domain), bob_received);
    assert_eq!(
        owner.lock().unwrap().digest(),
        digest,
        "inactive projection must not create sync actions"
    );
    assert_eq!(
        app.global::<nova_ui_shared::SmartCollectionsBridge>()
            .get_rows()
            .row_data(0)
            .unwrap()
            .id,
        "remote-thriller"
    );
    std::fs::write(root.join("case-complete"), "ok").unwrap();
}

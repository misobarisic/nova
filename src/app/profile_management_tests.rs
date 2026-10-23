//! Real user callbacks must enforce management grants before changing device state.
use super::*;

#[test]
fn restricted_device_actions_require_pin_and_forget_the_management_grant() {
    const ROOT: &str = "NOVA_MANAGEMENT_GATE_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-management-gate-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ROOT, &root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .args(["--exact", "app::profile_management_tests::restricted_device_actions_require_pin_and_forget_the_management_grant", "--nocapture"])
            .output().unwrap();
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
    storage::init_at(&root);
    profiles::initialize().unwrap();
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = AppWindow::new().unwrap();
    let player = crate::player::Player::setup(&app);
    let (hi, _) = mpsc::channel();
    let (lo, _) = mpsc::channel();
    let bridge = Bridge::new(
        app.as_weak(),
        PosterTx { hi, lo },
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    let owner = profiles::ProfileStore::capture();
    let mut policy = profile_policy::ProfilePolicy {
        pin: "0012".into(),
        restricted: true,
        ..Default::default()
    };
    owner
        .write(
            profile_policy::KEY,
            &serde_json::to_string(&policy).unwrap(),
        )
        .unwrap();
    let original = nova_sync::read_settings().device_name;
    bridge.sync_device_name_edited("blocked");
    assert_eq!(nova_sync::read_settings().device_name, original);
    assert!(app.global::<crate::MoreBridge>().get_pin_open());
    bridge.submit_profile_pin("1234");
    assert!(app.global::<crate::MoreBridge>().get_pin_open());
    assert_eq!(nova_sync::read_settings().device_name, original);
    bridge.submit_profile_pin("0012");
    bridge.sync_device_name_edited("allowed");
    assert_eq!(nova_sync::read_settings().device_name, "allowed");

    // Remote/import policy changes invalidate an already accepted local grant.
    policy.pin = "0034".into();
    owner
        .write(
            profile_policy::KEY,
            &serde_json::to_string(&policy).unwrap(),
        )
        .unwrap();
    bridge.sync_device_name_edited("stale grant");
    assert_eq!(nova_sync::read_settings().device_name, "allowed");
    bridge.submit_profile_pin("0034");
    bridge.sync_device_name_edited("new grant");
    assert_eq!(nova_sync::read_settings().device_name, "new grant");
    bridge.close_profile_management();
    bridge.sync_device_name_edited("closed flow");
    assert_eq!(nova_sync::read_settings().device_name, "new grant");
    bridge.cancel_profile_pin();
}

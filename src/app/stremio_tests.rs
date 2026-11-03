use super::*;

#[test]
fn automatic_latest_setting_survives_saturation_without_replacing_other_controls() {
    let handle = Arc::new(Handle::new());
    for _ in 0..32 {
        assert!(handle.tx.try_send(Command::Sync(7)).is_ok());
    }
    handle.send(Command::Mode(SyncMode::SelectedTitles, 7));
    handle.send(Command::Disconnect(7));
    handle.send(Command::Review(selection::Request::Confirm(true, 3), 7));
    let sender = handle.clone();
    let (done, waiting) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let started = Instant::now();
        for _ in 0..10_000 {
            for enabled in [false, true, false] {
                sender.send(Command::Automatic(enabled, 7));
            }
        }
        done.send(started.elapsed()).unwrap();
    });
    let elapsed = waiting
        .recv_timeout(Duration::from_secs(5))
        .expect("control admission blocked on the paused full receiver");
    task.join().unwrap();
    println!(
        "STATE08: ordinary=32 automatic_updates=30000 retained=1 coalesced=29999 admission_elapsed_ns={}",
        elapsed.as_nanos()
    );
    assert!(matches!(
        handle.automatic_change.lock().unwrap().take(),
        Some(Command::Automatic(false, 7))
    ));
    assert!(matches!(
        handle.mode_change.lock().unwrap().take(),
        Some(Command::Mode(SyncMode::SelectedTitles, 7))
    ));
    assert!(matches!(
        handle.urgent.lock().unwrap().take(),
        Some(Command::Disconnect(7))
    ));
    assert!(matches!(
        handle.selection_change.lock().unwrap().take(),
        Some(Command::Review(selection::Request::Confirm(true, 3), 7))
    ));
    assert!(handle.automatic_change.lock().unwrap().is_none());
    let rx = handle.rx.lock().unwrap().take().unwrap();
    for _ in 0..32 {
        assert!(matches!(rx.try_recv(), Ok(Command::Sync(7))));
    }
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

struct NoAutomaticNetwork;
impl nova_stremio::Transport for NoAutomaticNetwork {
    fn send(&self, _: nova_stremio::Request) -> Result<nova_stremio::Response, ApiError> {
        panic!("automatic setting dispatch must not issue HTTP")
    }
}

fn automatic_fixture(name: &str, body: impl FnOnce(Bridge, Connection)) {
    const ROOT: &str = "NOVA_STREMIO_AUTOMATIC_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-stremio-automatic-{}-{}",
            std::process::id(),
            name.rsplit("::").next().unwrap()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ROOT, &root)
            .args([
                "--exact",
                name.strip_prefix("nova::").unwrap_or(name),
                "--nocapture",
            ])
            .output()
            .unwrap();
        let completed = root.join("completed").is_file();
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success() && completed,
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = PathBuf::from(root);
    storage::init_at(&root);
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = AppWindow::new().unwrap();
    let player = crate::player::Player::setup(&app);
    #[cfg(feature = "desktop")]
    let bridge = Bridge::new(
        app.as_weak(),
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    #[cfg(not(feature = "desktop"))]
    let bridge = Bridge::new(
        app.as_weak(),
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    let connection = Connection {
        version: 1,
        account_id: "account".into(),
        email: "test@example.invalid".into(),
        auth_key: Secret::new("test-token"),
        epoch: "first-session".into(),
        automatic: true,
        started: true,
        bootstrap_done: true,
        mode: SyncMode::default(),
    };
    let owner = nova_sync::local_store().unwrap();
    let mut store = owner.lock().unwrap();
    write_connection_record(
        &mut store,
        Some(&connection),
        nova_sync::local_device().unwrap(),
    )
    .unwrap();
    store.save().unwrap();
    drop(store);
    body(bridge, connection);
    std::fs::write(root.join("completed"), name).unwrap();
}

#[test]
fn automatic_dispatch_persists_latest_false_before_ordinary_work() {
    automatic_fixture(
        concat!(
            module_path!(),
            "::automatic_dispatch_persists_latest_false_before_ordinary_work"
        ),
        |bridge, connection| {
            let handle = bridge.stremio();
            for _ in 0..32 {
                assert!(handle.tx.try_send(Command::Sync(0)).is_ok());
            }
            bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: "title".into(),
                type_: "movie".into(),
                request_id: "title".into(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: "Title".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            });
            bridge.stremio_title_unlink();
            for enabled in [false, true, false] {
                handle.send(Command::Automatic(enabled, 0));
            }
            assert!(matches!(
                handle.selection_change.lock().unwrap().take(),
                Some(Command::Review(selection::Request::Unlink(_, _), _))
            ));
            let mut worker = Worker::new(bridge.clone(), Client::new(NoAutomaticNetwork));
            worker.connection = Some(connection.clone());
            assert!(worker.dispatch_controls());
            assert!(!worker.connection.as_ref().unwrap().automatic);
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            let key = session_key(&connection, "automatic");
            assert_eq!(
                store
                    .record(&crate::app::sync::profile_domain(DOMAIN), &key)
                    .unwrap()
                    .value
                    .as_deref(),
                Some("false")
            );
            // Reload persisted bytes into a fresh Store: this is a durable flag.
            drop(store);
            let reloaded = nova_sync::Store::try_load().unwrap();
            assert_eq!(
                reloaded
                    .record(&crate::app::sync::profile_domain(DOMAIN), &key)
                    .unwrap()
                    .value
                    .as_deref(),
                Some("false")
            );
            let rx = handle.rx.lock().unwrap().take().unwrap();
            for _ in 0..32 {
                assert!(matches!(rx.try_recv(), Ok(Command::Sync(0))));
            }
        },
    );
}

#[test]
fn stale_automatic_slot_cannot_change_reconnected_account_and_shutdown_stays_first() {
    automatic_fixture(
        concat!(
            module_path!(),
            "::stale_automatic_slot_cannot_change_reconnected_account_and_shutdown_stays_first"
        ),
        |bridge, connection| {
            let handle = bridge.stremio();
            handle.send(Command::Automatic(false, 0));
            handle.generation.store(1, Ordering::Release);
            let mut replacement = connection;
            replacement.account_id = "replacement".into();
            replacement.epoch = "second-session".into();
            let mut worker = Worker::new(bridge.clone(), Client::new(NoAutomaticNetwork));
            worker.write_connection(Some(&replacement)).unwrap();
            worker.connection = Some(replacement.clone());
            assert!(worker.dispatch_controls());
            assert!(worker.connection.as_ref().unwrap().automatic);
            let owner = nova_sync::local_store().unwrap();
            assert!(
                owner
                    .lock()
                    .unwrap()
                    .record(
                        &crate::app::sync::profile_domain(DOMAIN),
                        &session_key(&replacement, "automatic")
                    )
                    .is_none()
            );
            handle.send(Command::Automatic(false, 1));
            handle.send(Command::Shutdown);
            assert!(!worker.dispatch_controls());
            assert!(worker.connection.as_ref().unwrap().automatic);
            assert!(matches!(
                handle.automatic_change.lock().unwrap().take(),
                Some(Command::Automatic(false, 1))
            ));
        },
    );
}

#[test]
fn disconnect_remains_deliverable_when_the_normal_queue_is_full() {
    let handle = Handle::new();
    for _ in 0..32 {
        assert!(handle.tx.try_send(Command::Sync(0)).is_ok());
    }
    handle.send(Command::Disconnect(7));
    assert!(matches!(
        handle.urgent.lock().unwrap().take(),
        Some(Command::Disconnect(7))
    ));
}

#[test]
fn membership_only_preserves_remote_state() {
    let mut item: LibraryItem = serde_json::from_value(serde_json::json!({
        "_id":"tt1","type":"series","removed":false,"temp":false,
        "state":{"watched":"opaque","timeOffset":42,"video_id":"ep","future":123}
    }))
    .unwrap();
    let before = item.0["state"].clone();
    item.apply_membership(false, 200);
    assert_eq!(item.0["state"], before);
    assert!(!membership_projection(&item).saved);
}

#[test]
fn shared_field_versions_preserve_original_time_and_remote_tie_priority() {
    let mut store = nova_sync::Store::default();
    let put = |store: &mut nova_sync::Store, key: &str, value: bool, stamp: u64, remote: bool| {
        let raw = serde_json::to_string(&Stamped {
            value,
            changed_at: stamp,
        })
        .unwrap();
        store.apply(
            DOMAIN,
            key,
            nova_sync::Record {
                version: nova_sync::Version::new(
                    stamp,
                    u32::from(remote),
                    nova_config::fnv1a(raw.as_bytes()),
                    false,
                ),
                value: Some(raw),
            },
        );
    };
    put(&mut store, "saved", true, 200, false);
    put(&mut store, "saved", false, 100, true);
    let value: Stamped<bool> = serde_json::from_str(
        store
            .record(DOMAIN, "saved")
            .unwrap()
            .value
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value,
        Stamped {
            value: true,
            changed_at: 200
        }
    );
    put(&mut store, "saved", false, 200, true);
    put(&mut store, "saved", true, 200, false);
    let value: Stamped<bool> = serde_json::from_str(
        store
            .record(DOMAIN, "saved")
            .unwrap()
            .value
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value,
        Stamped {
            value: false,
            changed_at: 200
        }
    );
    assert_eq!(store.record(DOMAIN, "saved").unwrap().version.ts, 200);
}

#[test]
fn deferred_progress_merge_marker_survives_restart_and_old_rows() {
    let mut title = Title::new(LibraryItem::new("tt1", "Title", "series", 100));
    assert!(!title.progress_merged);
    let mut raw = serde_json::to_value(&title).unwrap();
    raw.as_object_mut().unwrap().remove("progress_merged");
    let old: Title = serde_json::from_value(raw).unwrap();
    assert!(!old.progress_merged);
    title.progress_merged = true;
    let loaded: Title = serde_json::from_str(&serde_json::to_string(&title).unwrap()).unwrap();
    assert!(loaded.progress_merged);
}

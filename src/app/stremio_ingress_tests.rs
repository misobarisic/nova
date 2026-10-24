//! Real persistence/Bridge ingress regressions; each test owns its globals.
use super::*;

#[test]
fn materialization_without_an_ordered_writer_remains_unapplied() {
    isolated(
        concat!(
            module_path!(),
            "::materialization_without_an_ordered_writer_remains_unapplied"
        ),
        |fixture| {
            assert!(!persistence::running());
            let prepared = materialize::prepare(
                &fixture.bridge,
                &fixture.connection,
                &incoming(true),
                true,
                0,
            )
            .unwrap()
            .unwrap();
            assert!(matches!(
                materialize::accept(&fixture.bridge, prepared, 0).unwrap(),
                materialize::Accepted::Retry
            ));
            let state = fixture.bridge.shared.lock().unwrap();
            assert!(state.entries.is_empty());
            assert!(state.progress.is_empty());
            assert_eq!(storage::get_str("library").unwrap(), "[]");
        },
    );
}

#[test]
fn hidden_surfaces_wait_for_reveal_and_refresh_the_visible_calendar() {
    isolated(
        concat!(
            module_path!(),
            "::hidden_surfaces_wait_for_reveal_and_refresh_the_visible_calendar"
        ),
        |fixture| {
            let app = &fixture._app;
            app.set_show_home(false);
            app.set_show_calendar(true);
            app.set_modal_visible(true);
            fixture
                .bridge
                .stremio()
                .surfaces_dirty
                .store(true, Ordering::Release);
            fixture.bridge.refresh_stremio_surfaces_if_visible();
            assert!(
                fixture
                    .bridge
                    .stremio()
                    .surfaces_dirty
                    .load(Ordering::Acquire)
            );
            assert_eq!(app.get_calendar_cells().row_count(), 0);
            app.set_modal_visible(false);
            fixture.bridge.refresh_stremio_surfaces_if_visible();
            assert!(
                !fixture
                    .bridge
                    .stremio()
                    .surfaces_dirty
                    .load(Ordering::Acquire)
            );
            assert_eq!(app.get_calendar_cells().row_count(), 42);
        },
    );
}

#[test]
fn stalled_old_profile_worker_cannot_snapshot_or_save_the_replacement_library() {
    isolated_profiles(
        concat!(
            module_path!(),
            "::stalled_old_profile_worker_cannot_snapshot_or_save_the_replacement_library"
        ),
        |fixture| {
            struct DeferredAccount {
                entered: mpsc::SyncSender<()>,
                resume: Mutex<mpsc::Receiver<()>>,
            }
            impl nova_stremio::Transport for DeferredAccount {
                fn send(
                    &self,
                    request: nova_stremio::Request,
                ) -> Result<nova_stremio::Response, ApiError> {
                    let result = match request.url.rsplit('/').next().unwrap() {
                        "addonCollectionGet" => {
                            self.entered.send(()).unwrap();
                            self.resume
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(10))
                                .unwrap();
                            serde_json::json!({"addons":[]})
                        }
                        "datastoreMeta" => serde_json::json!([]),
                        endpoint => {
                            panic!("cancelled worker must not export replacement data: {endpoint}")
                        }
                    };
                    Ok(nova_stremio::Response {
                        status: 200,
                        retry_after: None,
                        body: serde_json::to_vec(&serde_json::json!({"result":result}))
                            .unwrap()
                            .into(),
                    })
                }
            }
            let outgoing = profiles::capture();
            let title = incoming(true);
            let prepared =
                materialize::prepare(&fixture.bridge, &fixture.connection, &title, true, 0)
                    .unwrap()
                    .unwrap();
            let before =
                nova_storage::try_scan_prefix(&profiles::storage_key_for(&outgoing.id, ""))
                    .unwrap();
            let (entered, waiting) = mpsc::sync_channel(1);
            let (release, resume) = mpsc::sync_channel(1);
            let bridge = fixture.bridge.clone();
            let connection = fixture.connection.clone();
            let worker = profiles::thread::spawn(move || {
                let mut worker = Worker::new(
                    bridge,
                    Client::new(DeferredAccount {
                        entered,
                        resume: Mutex::new(resume),
                    }),
                );
                worker.synchronize(&connection)
            });
            waiting.recv_timeout(Duration::from_secs(10)).unwrap();
            let replacement = profiles::create("Bob").unwrap();
            fixture.bridge.switch_profile(&replacement);
            assert_eq!(profiles::active_context().id, replacement);
            let mut private_entry = ordered_ingress_entry("bob-private");
            private_entry.type_ = "movie".into();
            fixture.bridge.shared.lock().unwrap().entries = vec![private_entry];
            write_persisted_library(&fixture.bridge.shared.lock().unwrap().entries);
            let replacement_before = read_persisted_library();
            // Keep service generations identical: captured profile ownership,
            // rather than an incidental shutdown generation, must reject this.
            assert_eq!(
                fixture.bridge.stremio().generation.load(Ordering::Acquire),
                0
            );
            release.send(()).unwrap();
            assert!(matches!(worker.join().unwrap(), Err(ApiError::Rejected)));
            profiles::with_context(outgoing.clone(), || {
                assert!(fixture.bridge.personal_state().is_none());
                assert!(matches!(
                    materialize::prepare(&fixture.bridge, &fixture.connection, &title, true, 0),
                    Err(ApiError::Rejected)
                ));
                assert!(matches!(
                    materialize::accept(&fixture.bridge, prepared, 0),
                    Err(ApiError::Rejected)
                ));
            });
            assert_eq!(
                nova_storage::try_scan_prefix(&profiles::storage_key_for(&outgoing.id, ""))
                    .unwrap(),
                before
            );
            assert_eq!(read_persisted_library(), replacement_before);
            assert_eq!(
                fixture.bridge.shared.lock().unwrap().entries[0].id,
                "bob-private"
            );
        },
    );
}

#[test]
fn accepted_upload_after_bridge_switch_is_checkpointed_only_for_issuing_profile() {
    isolated_profiles(
        concat!(
            module_path!(),
            "::accepted_upload_after_bridge_switch_is_checkpointed_only_for_issuing_profile"
        ),
        |fixture| {
            struct DeferredUpload {
                entered: mpsc::SyncSender<()>,
                resume: Mutex<mpsc::Receiver<()>>,
            }
            impl nova_stremio::Transport for DeferredUpload {
                fn send(
                    &self,
                    request: nova_stremio::Request,
                ) -> Result<nova_stremio::Response, ApiError> {
                    assert!(
                        request.url.ends_with("/datastorePut"),
                        "canceled actor issued fresh HTTP: {}",
                        request.url
                    );
                    self.entered.send(()).unwrap();
                    self.resume
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap();
                    Ok(nova_stremio::Response {
                        status: 200,
                        retry_after: None,
                        body: br#"{"result":{"success":true}}"#.to_vec().into(),
                    })
                }
            }
            let issuing = profiles::ProfileStore::capture();
            let connection = fixture.connection.clone();
            let mut title = incoming(true);
            title.pending = true;
            let key = title_storage_key(&connection.account_id, &title);
            let (entered, waiting) = mpsc::sync_channel(1);
            let (release, resume) = mpsc::sync_channel(1);
            let bridge = fixture.bridge.clone();
            let original_handle = fixture.bridge.stremio();
            let receiver = original_handle.rx.lock().unwrap().take().unwrap();
            original_handle.finished.store(false, Ordering::Release);
            let done = profile_actions::WorkerDone(original_handle.finished.clone());
            let worker = profiles::thread::spawn(move || {
                let _done = done;
                let _receiver = receiver;
                let worker = Worker::new(
                    bridge,
                    Client::new(DeferredUpload {
                        entered,
                        resume: Mutex::new(resume),
                    }),
                );
                worker.upload_title(&connection, &mut title, 0)
            });
            waiting.recv_timeout(Duration::from_secs(10)).unwrap();
            let replacement = profiles::create("Bob").unwrap();
            fixture.bridge.switch_profile(&replacement);
            assert_eq!(profiles::active_context().id, replacement);
            let selected = profiles::ProfileStore::capture();
            assert!(selected.read(&key).unwrap().is_none());
            fixture.bridge.switch_profile(issuing.id());
            assert_eq!(profiles::active_context().id, issuing.id());
            let returning_handle = fixture.bridge.stremio();
            assert!(!Arc::ptr_eq(&original_handle, &returning_handle));
            assert!(
                !original_handle.is_finished(),
                "the actual upload is still blocked"
            );
            assert!(returning_handle.is_finished());
            assert!(returning_handle.rx.lock().unwrap().is_some());
            fixture.bridge.retry_profile_services();
            assert!(
                returning_handle.rx.lock().unwrap().is_some(),
                "returning A started a second actor before the first drained"
            );
            release.send(()).unwrap();
            assert!(matches!(worker.join().unwrap(), Err(ApiError::Rejected)));
            let saved: Title = serde_json::from_str(&issuing.read(&key).unwrap().unwrap()).unwrap();
            assert!(
                saved.accepted_upload.is_some(),
                "the accepted external mutation lost its original-profile receipt"
            );
            assert!(
                saved.pending,
                "acceptance still requires later server verification"
            );
            assert!(selected.read(&key).unwrap().is_none());
            assert!(
                original_handle.is_finished(),
                "WorkerDone must release the real actor drain barrier"
            );
            assert!(
                returning_handle.rx.lock().unwrap().is_some(),
                "finishing the old actor alone must not consume the new receiver"
            );
            // This fixture's credential is intentionally fake. Disconnect it
            // before starting the replacement actor so no real HTTP is issued.
            {
                let owner = nova_sync::local_store().unwrap();
                let mut store = owner.lock().unwrap();
                store.set(
                    &issuing.domain(DOMAIN),
                    CONNECTION,
                    None,
                    0,
                    nova_sync::local_device().unwrap(),
                );
                store.save().unwrap();
            }
            fixture.bridge.retry_profile_services();
            assert!(
                returning_handle.rx.lock().unwrap().is_none(),
                "the retained start request must run once the old actor finishes"
            );
            assert!(!returning_handle.is_finished());
            assert_eq!(profiles::active_context().id, issuing.id());
            fixture.bridge.shutdown_tracking();
            fixture.bridge.shutdown_stremio();
        },
    );
}

struct Fixture {
    _app: AppWindow,
    bridge: Bridge,
    connection: Connection,
}

struct NeverRequest;
impl nova_stremio::Transport for NeverRequest {
    fn send(&self, _: nova_stremio::Request) -> Result<nova_stremio::Response, ApiError> {
        panic!("credential updates must not issue network requests")
    }
}

#[test]
fn queued_controls_from_an_old_connection_cannot_start_or_change_a_new_one() {
    isolated(
        concat!(
            module_path!(),
            "::queued_controls_from_an_old_connection_cannot_start_or_change_a_new_one"
        ),
        |fixture| {
            fixture
                .bridge
                .stremio()
                .generation
                .store(1, Ordering::Release);
            let mut worker = Worker::new(fixture.bridge, Client::new(NeverRequest));
            worker.connection = Some(fixture.connection);
            worker.verified = true;
            worker.preview_ready = true;
            for command in [
                Command::Automatic(false, 0),
                Command::Start(0),
                Command::Sync(0),
            ] {
                worker.command(command);
            }
            assert!(!worker.force);
            let owner = nova_sync::local_store().unwrap();
            assert_eq!(owner.lock().unwrap().records(DOMAIN).len(), 1);
        },
    );
}

#[test]
fn changing_automatic_sync_cannot_restore_a_tombstoned_connection() {
    isolated(
        concat!(
            module_path!(),
            "::changing_automatic_sync_cannot_restore_a_tombstoned_connection"
        ),
        |fixture| {
            let owner = nova_sync::local_store().unwrap();
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN,
                CONNECTION,
                None,
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            let mut worker = Worker::new(fixture.bridge, Client::new(NeverRequest));
            let connection = fixture.connection;
            assert!(
                worker
                    .save_flag(&connection, SessionFlag::Automatic(false))
                    .is_err()
            );
            assert!(
                owner
                    .lock()
                    .unwrap()
                    .record(DOMAIN, CONNECTION)
                    .unwrap()
                    .is_deleted()
            );
        },
    );
}

fn isolated(name: &str, body: impl FnOnce(Fixture)) {
    isolated_with_clock(name, false, body);
}

fn isolated_profiles(name: &str, body: impl FnOnce(Fixture)) {
    isolated_with_options(name, false, true, body);
}

fn isolated_with_clock(name: &str, system_time: bool, body: impl FnOnce(Fixture)) {
    isolated_with_options(name, system_time, false, body);
}

fn isolated_with_options(
    name: &str,
    system_time: bool,
    profiles_enabled: bool,
    body: impl FnOnce(Fixture),
) {
    const ROOT: &str = "NOVA_STREMIO_INGRESS_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-stremio-ingress-{}-{}-{}",
            std::process::id(),
            nova_config::now_ms(),
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
    storage::init_at(&root);
    if system_time {
        i_slint_backend_testing::init_integration_test_with_system_time();
    } else {
        i_slint_backend_testing::init_integration_test_with_mock_time();
    }
    let app = AppWindow::new().unwrap();
    let player = crate::player::Player::setup(&app);
    let (hi, _) = mpsc::channel();
    let (lo, _) = mpsc::channel();
    persist_sync_snapshot("library", "[]", true).unwrap();
    persist_sync_snapshot(EPISODE_PROGRESS_KEY, "{}", true).unwrap();
    let connection = Connection {
        version: 1,
        epoch: "test-epoch".into(),
        account_id: "test-account".into(),
        email: "test@example.invalid".into(),
        auth_key: Secret::new("test-token"),
        automatic: true,
        started: true,
        bootstrap_done: true,
        mode: SyncMode::default(),
    };
    let owner = nova_sync::local_store().unwrap();
    let mut store = owner.lock().unwrap();
    store.set(
        DOMAIN,
        CONNECTION,
        Some(serde_json::to_string(&connection).unwrap()),
        0,
        nova_sync::local_device().unwrap(),
    );
    store.save().unwrap();
    drop(store);
    if profiles_enabled {
        sync::migrate_legacy_profiles().unwrap();
        profiles::initialize().unwrap();
    }
    // Like production, establish durable profile ownership before any session
    // allocation captures its store. A legacy adapter must never be upgraded
    // by attaching it to a profile-enabled Context afterward.
    let bridge = Bridge::new(
        app.as_weak(),
        PosterTx { hi, lo },
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    body(Fixture {
        _app: app,
        bridge,
        connection,
    });
    std::fs::write(root.join("completed"), name).unwrap();
}

fn incoming(watched: bool) -> Title {
    let mut title = Title::new(LibraryItem::new("tt100", "Remote series", "series", 1_000));
    title.mapping = Mapping {
        media_type: "series".into(),
        id: "tt100".into(),
        videos: vec![nova_stremio::Video {
            id: "tt100:1:1".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        }],
        episode_ids: [("tt100:1:1".into(), "tt100:1:1".into())].into(),
        checked_at: 1_000,
        ..Default::default()
    };
    title.desired.saved = Stamped {
        value: true,
        changed_at: 1_000,
    };
    title.desired.watched.insert(
        "tt100:1:1".into(),
        Stamped {
            value: watched,
            changed_at: 1_000,
        },
    );
    title
}

#[test]
fn external_ingress_publishes_mesh_records_without_source_or_tracker_upload_authority() {
    isolated(
        concat!(
            module_path!(),
            "::external_ingress_publishes_mesh_records_without_source_or_tracker_upload_authority"
        ),
        |fixture| {
            let title = incoming(true);
            let result = apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            assert!(result.saved.value);
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            assert!(
                store
                    .record(DOMAIN_LIBRARY, &library::library_key("series", "tt100"))
                    .and_then(|r| r.value.as_ref())
                    .is_some()
            );
            let raw = store
                .record(DOMAIN_PROGRESS, &progress_map_key("tt100", "tt100:1:1"))
                .and_then(|r| r.value.as_ref())
                .unwrap();
            assert!(
                serde_json::from_str::<EpisodeProgress>(raw)
                    .unwrap()
                    .watched
            );
            assert!(
                stremio_events::load(&store, &fixture.connection.account_id, "series", "tt100")
                    .is_none()
            );
            drop(store);
            let journal = storage::scan_prefix(nova_tracking::EVENT_PREFIX);
            assert!(
                !journal.is_empty(),
                "external history must advance tracker observation cursors"
            );
            for (_, raw) in journal {
                let event: nova_tracking::WatchEvent = serde_json::from_str(&raw).unwrap();
                // The tracker consumes these as observations and cannot create a
                // MAL/AniList progress upload from an external history apply.
                assert_eq!(event.origin, nova_tracking::EventOrigin::PairedDevice);
            }
        },
    );
}

#[test]
fn newer_local_remove_and_unwatch_survive_stale_worker_projection() {
    isolated(
        concat!(
            module_path!(),
            "::newer_local_remove_and_unwatch_survive_stale_worker_projection"
        ),
        |fixture| {
            let title = incoming(true);
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            let progress = {
                let mut state = fixture.bridge.shared.lock().unwrap();
                state.entries.clear();
                let progress = state
                    .progress
                    .get_mut(&progress_map_key("tt100", "tt100:1:1"))
                    .unwrap();
                progress.watched = false;
                progress.updated_at_secs = now_secs();
                progress.unwatched_at_secs = now_secs();
                state.progress.clone()
            };
            write_persisted_library(&[]);
            write_progress_map(&progress);
            let result = apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            assert!(!result.saved.value);
            assert!(!result.watched["tt100:1:1"].value);
            assert!(fixture.bridge.shared.lock().unwrap().entries.is_empty());
            assert!(
                !fixture.bridge.shared.lock().unwrap().progress
                    [&progress_map_key("tt100", "tt100:1:1")]
                    .watched
            );
        },
    );
}

#[test]
fn unwatched_import_does_not_invent_playback_checkpoints() {
    isolated(
        concat!(
            module_path!(),
            "::unwatched_import_does_not_invent_playback_checkpoints"
        ),
        |fixture| {
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &incoming(false),
                true,
                0,
            )
            .unwrap();
            assert_eq!(fixture.bridge.shared.lock().unwrap().entries.len(), 1);
            assert!(fixture.bridge.shared.lock().unwrap().progress.is_empty());
            assert!(read_progress_map().is_empty());
            let owner = nova_sync::local_store().unwrap();
            assert!(owner.lock().unwrap().records(DOMAIN_PROGRESS).is_empty());
            assert!(storage::scan_prefix(nova_tracking::EVENT_PREFIX).is_empty());
        },
    );
}

#[test]
fn pending_title_and_baselines_restore_from_durable_rows() {
    isolated(
        concat!(
            module_path!(),
            "::pending_title_and_baselines_restore_from_durable_rows"
        ),
        |fixture| {
            let mut title = incoming(true);
            title.pending = true;
            title.local.saved = Stamped {
                value: false,
                changed_at: 500,
            };
            title.remote.saved = Stamped {
                value: true,
                changed_at: 1_000,
            };
            title.item.0.insert(
                "unknownFutureField".into(),
                serde_json::json!({"keep":true}),
            );
            save_title(&fixture.connection, &mut title).unwrap();
            drop(title);
            let restored = load_titles(&fixture.connection.account_id).unwrap();
            let title = &restored["tt100"];
            assert!(title.pending);
            assert_eq!(title.local.saved.changed_at, 500);
            assert_eq!(title.remote.saved.changed_at, 1_000);
            assert!(title.desired.watched["tt100:1:1"].value);
            assert_eq!(title.item.0["unknownFutureField"]["keep"], true);
            assert_eq!(
                read_shared(&fixture.connection, "tt100").unwrap(),
                title.desired
            );
            assert!(load_titles("another-account").unwrap().is_empty());
        },
    );
}

#[test]
fn disconnect_generation_and_tombstone_reject_old_ingress() {
    isolated(
        concat!(
            module_path!(),
            "::disconnect_generation_and_tombstone_reject_old_ingress"
        ),
        |fixture| {
            let title = incoming(true);
            fixture
                .bridge
                .stremio()
                .generation
                .fetch_add(1, Ordering::AcqRel);
            assert_eq!(
                apply_title(
                    &fixture.bridge,
                    &fixture.connection.account_id,
                    &title,
                    true,
                    0
                ),
                Err(ApiError::Rejected)
            );
            let owner = nova_sync::local_store().unwrap();
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN,
                CONNECTION,
                None,
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            assert_eq!(
                apply_title(
                    &fixture.bridge,
                    &fixture.connection.account_id,
                    &title,
                    true,
                    1
                ),
                Err(ApiError::Authentication)
            );
            assert!(fixture.bridge.shared.lock().unwrap().entries.is_empty());
            assert!(fixture.bridge.shared.lock().unwrap().progress.is_empty());
        },
    );
}

#[test]
fn newer_mesh_actions_survive_stale_worker_projection_and_return_the_winner() {
    isolated(
        concat!(
            module_path!(),
            "::newer_mesh_actions_survive_stale_worker_projection_and_return_the_winner"
        ),
        |fixture| {
            let mut old = incoming(true);
            old.item.0.remove("_ctime");
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &old,
                true,
                0,
            )
            .unwrap();
            // Display ordering must inherit the imported membership action's
            // clock; dating it at ingress would forge a newer local add when
            // another device has no acknowledged native baseline yet.
            assert_eq!(
                fixture.bridge.shared.lock().unwrap().entries[0].added_at_secs,
                1,
            );
            let mut newer = old.clone();
            newer.desired.saved = Stamped {
                value: false,
                changed_at: 2_000,
            };
            newer.desired.watched.insert(
                "tt100:1:1".into(),
                Stamped {
                    value: false,
                    changed_at: 2_000,
                },
            );
            save_title(&fixture.connection, &mut newer).unwrap();
            let result = apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &old,
                true,
                0,
            )
            .unwrap();
            assert_eq!(result.saved, newer.desired.saved);
            assert_eq!(
                result.watched["tt100:1:1"],
                newer.desired.watched["tt100:1:1"]
            );
            assert!(fixture.bridge.shared.lock().unwrap().entries.is_empty());
            assert!(
                !fixture.bridge.shared.lock().unwrap().progress
                    [&progress_map_key("tt100", "tt100:1:1")]
                    .watched
            );
        },
    );
}

#[test]
fn source_events_keep_field_clocks_and_remap_resume_before_generic_checkpoints() {
    isolated(
        concat!(
            module_path!(),
            "::source_events_keep_field_clocks_and_remap_resume_before_generic_checkpoints"
        ),
        |fixture| {
            let mut title = incoming(true);
            title.mapping.id = "native-series".into();
            title.mapping.episode_ids = [("tt100:1:1".into(), "native-episode".into())].into();
            title.local.watched.insert(
                "tt100:1:1".into(),
                Stamped {
                    value: true,
                    changed_at: 100,
                },
            );
            title.local.resume = Stamped {
                value: None,
                changed_at: 100,
            };
            let owner = nova_sync::local_store().unwrap();
            let mut store = owner.lock().unwrap();
            let prefix = stremio_events::event_key(
                &fixture.connection.account_id,
                "series",
                "native-series",
            );
            let watched_key = format!(
                "{prefix}:watched:{}",
                serde_json::to_string("native-episode").unwrap()
            );
            let resume_key = format!("{prefix}:resume");
            store.set(
                DOMAIN,
                &watched_key,
                Some(
                    serde_json::to_string(&Stamped {
                        value: true,
                        changed_at: 100,
                    })
                    .unwrap(),
                ),
                0,
                nova_sync::local_device().unwrap(),
            );
            let native_resume = Resume {
                video_id: "native-episode".into(),
                position_ms: 5_000,
                duration_ms: 60_000,
            };
            store.set(
                DOMAIN,
                &resume_key,
                Some(
                    serde_json::to_string(&Stamped {
                        value: Some(native_resume),
                        changed_at: 200,
                    })
                    .unwrap(),
                ),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            let progress = [(
                progress_map_key("native-series", "native-episode"),
                EpisodeProgress {
                    series_id: "native-series".into(),
                    episode_id: "native-episode".into(),
                    watched: true,
                    updated_at_secs: 300,
                    position_secs: 9.0,
                    duration_secs: 60.0,
                    ..Default::default()
                },
            )]
            .into();
            let projection = local_projection(&title.mapping, &[], &progress);
            let state =
                source_canonical(&fixture.connection, &title, &projection, &[], &progress).unwrap();
            assert_eq!(
                state.watched["tt100:1:1"].changed_at, 100,
                "generic playback save must not refresh watched action time"
            );
            assert_eq!(
                state.resume,
                Stamped {
                    value: Some(Resume {
                        video_id: "tt100:1:1".into(),
                        position_ms: 5_000,
                        duration_ms: 60_000
                    }),
                    changed_at: 200
                }
            );
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN,
                &resume_key,
                Some(
                    serde_json::to_string(&Stamped::<Option<Resume>> {
                        value: None,
                        changed_at: 400,
                    })
                    .unwrap(),
                ),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            let cleared =
                source_canonical(&fixture.connection, &title, &projection, &[], &progress).unwrap();
            assert_eq!(
                cleared.resume,
                Stamped {
                    value: None,
                    changed_at: 400
                },
                "explicit clear must outrank retained native positions"
            );
        },
    );
}

#[test]
fn save_title_returns_store_winners_and_preserves_acknowledged_source_clocks() {
    isolated(
        concat!(
            module_path!(),
            "::save_title_returns_store_winners_and_preserves_acknowledged_source_clocks"
        ),
        |fixture| {
            let mut stale = incoming(true);
            let mut newer = stale.clone();
            newer.desired.saved = Stamped {
                value: false,
                changed_at: 2_000,
            };
            newer.desired.watched.insert(
                "tt100:1:1".into(),
                Stamped {
                    value: false,
                    changed_at: 2_000,
                },
            );
            save_title(&fixture.connection, &mut newer).unwrap();
            let observed = stale.item.projection(&stale.mapping.videos).unwrap();
            stale.remote = Canonical::from_projection(&observed, 100);
            save_title(&fixture.connection, &mut stale).unwrap();
            assert_eq!(stale.desired.saved, newer.desired.saved);
            assert_eq!(
                stale.desired.watched["tt100:1:1"],
                newer.desired.watched["tt100:1:1"]
            );
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            let raw = store
                .record(
                    DOMAIN,
                    &format!(
                        "{}remote",
                        shared_prefix(&fixture.connection.account_id, "tt100")
                    ),
                )
                .and_then(|record| record.value.as_ref())
                .unwrap();
            let (revision, clocks): (u64, Canonical) = serde_json::from_str(raw).unwrap();
            assert_eq!(revision, stale.item.modified_ms());
            assert_eq!(clocks.saved.changed_at, 100);
            assert_eq!(clocks.watched["tt100:1:1"].changed_at, 100);
        },
    );
}

#[test]
fn deleting_imported_unsaved_history_retains_native_identity_for_explicit_clear() {
    isolated(
        concat!(
            module_path!(),
            "::deleting_imported_unsaved_history_retains_native_identity_for_explicit_clear"
        ),
        |fixture| {
            let mut title = incoming(true);
            title.desired.saved = Stamped {
                value: false,
                changed_at: 1_000,
            };
            title.desired.resume = Stamped {
                value: Some(Resume {
                    video_id: "tt100:1:1".into(),
                    position_ms: 10_000,
                    duration_ms: 60_000,
                }),
                changed_at: 1_000,
            };
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            {
                let state = fixture.bridge.shared.lock().unwrap();
                assert!(state.entries.is_empty());
                let imported = &state.progress[&progress_map_key("tt100", "tt100:1:1")];
                assert_eq!(
                    imported
                        .history
                        .as_ref()
                        .map(|history| history.type_.as_str()),
                    Some("series")
                );
            }
            fixture.bridge.shared.lock().unwrap().progress.clear();
            write_progress_map(&HashMap::new());
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            let events =
                stremio_events::load(&store, &fixture.connection.account_id, "series", "tt100")
                    .unwrap();
            assert!(!events.watched["tt100:1:1"].value);
            assert!(events.watched["tt100:1:1"].changed_at > 1_000);
            assert_eq!(events.resume.value, None);
            assert!(events.resume.changed_at > 1_000);
        },
    );
}

#[test]
fn partial_native_coverage_preserves_unowned_resume_until_a_mapped_start() {
    isolated(
        concat!(
            module_path!(),
            "::partial_native_coverage_preserves_unowned_resume_until_a_mapped_start"
        ),
        |fixture| {
            let mut title = incoming(true);
            title.mapping.id = "native-season-two".into();
            title.mapping.episode_ids = [("tt100:2:1".into(), "native-episode".into())].into();
            let unowned = Stamped {
                value: Some(Resume {
                    video_id: "tt100:1:1".into(),
                    position_ms: 10_000,
                    duration_ms: 60_000,
                }),
                changed_at: 1_000,
            };
            title.local.resume = unowned.clone();
            let progress = [(
                progress_map_key("native-season-two", "native-episode"),
                EpisodeProgress {
                    series_id: "native-season-two".into(),
                    episode_id: "native-episode".into(),
                    watched: true,
                    updated_at_secs: 2,
                    ..Default::default()
                },
            )]
            .into();
            let projection = local_projection(&title.mapping, &[], &progress);
            assert!(projection.resume.is_none());
            let source =
                source_canonical(&fixture.connection, &title, &projection, &[], &progress).unwrap();
            assert_eq!(
                source.resume, unowned,
                "a newer watched checkpoint in another season is not authority to clear the global remote cursor"
            );

            let owner = nova_sync::local_store().unwrap();
            let key = format!(
                "{}:resume",
                stremio_events::event_key(
                    &fixture.connection.account_id,
                    "series",
                    "native-season-two"
                )
            );
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN,
                &key,
                Some(
                    serde_json::to_string(&Stamped::<Option<Resume>> {
                        value: None,
                        changed_at: 3_000,
                    })
                    .unwrap(),
                ),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            let source =
                source_canonical(&fixture.connection, &title, &projection, &[], &progress).unwrap();
            assert_eq!(
                source.resume, unowned,
                "clearing this native season's cursor cannot clear an unowned season's cursor"
            );

            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN,
                &key,
                Some(
                    serde_json::to_string(&Stamped {
                        value: Some(Resume {
                            video_id: "native-episode".into(),
                            position_ms: 5_000,
                            duration_ms: 60_000,
                        }),
                        changed_at: 4_000,
                    })
                    .unwrap(),
                ),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
            drop(store);
            let source =
                source_canonical(&fixture.connection, &title, &projection, &[], &progress).unwrap();
            assert_eq!(
                source.resume,
                Stamped {
                    value: Some(Resume {
                        video_id: "tt100:2:1".into(),
                        position_ms: 5_000,
                        duration_ms: 60_000,
                    }),
                    changed_at: 4_000
                },
                "an explicit mapped playback start may replace the global cursor"
            );
        },
    );
}

/// Exercise the actor's actual UI-thread handoff and durable writers, rather
/// than calling apply_title directly as the focused ingress tests above do.
#[test]
fn initial_worker_merge_imports_fifty_nine_titles_and_confirms_local_uploads() {
    isolated_with_clock(
        concat!(
            module_path!(),
            "::initial_worker_merge_imports_fifty_nine_titles_and_confirms_local_uploads"
        ),
        true,
        |fixture| {
            struct Datastore {
                items: Mutex<BTreeMap<String, LibraryItem>>,
                writes: AtomicU64,
                confirmations: AtomicU64,
            }
            struct MockDatastore(Arc<Datastore>);
            impl nova_stremio::Transport for MockDatastore {
                fn send(
                    &self,
                    request: nova_stremio::Request,
                ) -> Result<nova_stremio::Response, ApiError> {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(body["authKey"], "test-token");
                    let mut items = self.0.items.lock().unwrap();
                    let result = match request.url.rsplit('/').next().unwrap() {
                        "getUser" => {
                            serde_json::json!({"_id":"test-account","email":"test@example.invalid"})
                        }
                        "addonCollectionGet" => serde_json::json!({"addons":[]}),
                        "datastoreMeta" => serde_json::json!(
                            items
                                .values()
                                .map(|item| (item.id(), item.modified_ms()))
                                .collect::<Vec<_>>()
                        ),
                        "datastoreGet" => {
                            if body["all"] == true {
                                serde_json::json!(items.values().collect::<Vec<_>>())
                            } else {
                                let ids: Vec<String> =
                                    serde_json::from_value(body["ids"].clone()).unwrap();
                                if self.0.writes.load(Ordering::Acquire)
                                    > self.0.confirmations.load(Ordering::Acquire)
                                {
                                    self.0.confirmations.fetch_add(1, Ordering::AcqRel);
                                }
                                serde_json::json!(
                                    ids.iter()
                                        .filter_map(|id| items.get(id))
                                        .collect::<Vec<_>>()
                                )
                            }
                        }
                        "datastorePut" => {
                            let changes: Vec<LibraryItem> =
                                serde_json::from_value(body["changes"].clone()).unwrap();
                            assert!(!changes.is_empty());
                            for item in changes {
                                items.insert(item.id().into(), item);
                            }
                            self.0.writes.fetch_add(1, Ordering::AcqRel);
                            serde_json::json!({"success":true})
                        }
                        endpoint => panic!("unexpected account endpoint {endpoint}"),
                    };
                    Ok(nova_stremio::Response {
                        status: 200,
                        retry_after: None,
                        body: serde_json::to_vec(&serde_json::json!({"result":result}))
                            .unwrap()
                            .into(),
                    })
                }
            }
            let series_id = "tt9000000";
            let videos: Vec<_> = (1..=2)
                .map(|episode| nova_stremio::Video {
                    id: format!("{series_id}:1:{episode}"),
                    season: Some(1),
                    episode: Some(episode),
                    ..Default::default()
                })
                .collect();
            let mut series = LibraryItem::new(series_id, "Remote series", "series", 1_000);
            series
                .apply_projection(
                    &Projection {
                        saved: true,
                        watched: [(videos[0].id.clone(), true), (videos[1].id.clone(), false)]
                            .into(),
                        resume: Some(Resume {
                            video_id: videos[1].id.clone(),
                            position_ms: 10_000,
                            duration_ms: 60_000,
                        }),
                    },
                    &videos,
                    1_000,
                )
                .unwrap();
            let mut series_title = Title::new(series.clone());
            series_title.mapping = Mapping {
                version: MAPPING_VERSION,
                media_type: "series".into(),
                id: series_id.into(),
                videos: videos.clone(),
                episode_ids: videos
                    .iter()
                    .map(|video| (video.id.clone(), video.id.clone()))
                    .collect(),
                checked_at: nova_config::now_ms(),
                inventory_revision: nova_providers::metadata_revision(),
                descriptor_revision: nova_config::fnv1a(b"[]"),
                local_inventory_revision: 0,
            };
            let datastore = Arc::new(Datastore {
                items: Mutex::new(
                    (0..58)
                        .map(|index| {
                            let id = format!("tt{index:07}");
                            let mut item =
                                LibraryItem::new(&id, &format!("Movie {index}"), "movie", 1_000);
                            // A local saved title must be restored on the remote side
                            // by the initial union, exercising Put and its readback.
                            if index == 0 {
                                item.apply_membership(false, 1_000);
                            }
                            if index % 2 == 1 {
                                item.0["state"]["timesWatched"] = serde_json::json!(1);
                            }
                            // An older account entry can legitimately have no
                            // progress fields yet.
                            if index == 2 {
                                item.0.insert("state".into(), serde_json::json!({}));
                            }
                            (id, item)
                        })
                        .chain([(series_id.into(), series)])
                        .collect(),
                ),
                writes: AtomicU64::new(0),
                confirmations: AtomicU64::new(0),
            });
            let mut connection = fixture.connection;
            connection.started = false;
            connection.bootstrap_done = false;
            let mut entries: Vec<LibraryEntry> = (0..3)
                .map(|index| LibraryEntry {
                    id: format!("tt{index:07}"),
                    type_: "movie".into(),
                    name: format!("Movie {index}"),
                    year: String::new(),
                    poster_url: String::new(),
                    background_url: String::new(),
                    genres: Vec::new(),
                    description: String::new(),
                    categories: Vec::new(),
                    watch_status: WatchStatus::Auto,
                    imported_tracking_status: None,
                    added_at_secs: 2,
                })
                .collect();
            let mut series_entry = entries[0].clone();
            series_entry.id = series_id.into();
            series_entry.type_ = "series".into();
            series_entry.name = "Local series".into();
            entries.push(series_entry);
            let progress: HashMap<_, _> = [
                (
                    progress_map_key(series_id, &videos[0].id),
                    EpisodeProgress {
                        series_id: series_id.into(),
                        episode_id: videos[0].id.clone(),
                        position_secs: 20.0,
                        duration_secs: 60.0,
                        updated_at_secs: 3,
                        ..Default::default()
                    },
                ),
                (
                    progress_map_key(series_id, &videos[1].id),
                    EpisodeProgress {
                        series_id: series_id.into(),
                        episode_id: videos[1].id.clone(),
                        watched: true,
                        updated_at_secs: 2,
                        ..Default::default()
                    },
                ),
            ]
            .into();
            {
                let mut state = fixture.bridge.shared.lock().unwrap();
                state.entries = entries.clone();
                state.progress = progress.clone();
            }
            write_persisted_library(&entries);
            write_progress_map(&progress);
            let persistence_worker = persistence::start().unwrap();
            let bridge = fixture.bridge.clone();
            let remote = datastore.clone();
            let task = crate::app::profiles::thread::spawn(move || {
                let mut worker = Worker::new(bridge, Client::new(MockDatastore(remote)));
                worker.write_connection(Some(&connection)).unwrap();
                worker.connection = Some(connection.clone());
                worker.verified = true;
                // Seed a current confirmed inventory to exercise episode
                // reconciliation without depending on an addon network call.
                worker
                    .titles
                    .insert(series_title.item.id().into(), series_title);
                let result = (|| {
                    worker.prepare_preview(&connection)?;
                    worker.command(Command::Start(0));
                    assert!(worker.force, "Start must schedule the initial merge");
                    for _ in 0..4 {
                        worker.tick()?;
                        if !worker.force {
                            break;
                        }
                    }
                    assert!(
                        !worker.force,
                        "initial merge must converge after refreshed source clocks"
                    );
                    persistence::flush().map_err(|_| ApiError::Rejected)?;
                    Ok::<_, ApiError>(worker.connection.as_ref().unwrap().bootstrap_done)
                })();
                // Queue this after the actor's status updates so assertions see
                // the completed UI state, including the real invoke callback.
                crate::app::profiles::invoke_from_event_loop(|| slint::quit_event_loop().unwrap())
                    .unwrap();
                result
            });
            let watchdog = slint::Timer::default();
            watchdog.start(
                slint::TimerMode::SingleShot,
                Duration::from_secs(45),
                || {
                    slint::quit_event_loop().unwrap();
                },
            );
            slint::run_event_loop().unwrap();
            watchdog.stop();
            assert_eq!(task.join().unwrap(), Ok(true));
            assert_eq!(fixture.bridge.shared.lock().unwrap().entries.len(), 59);
            assert_eq!(read_persisted_library().len(), 59);
            let restored = load_titles("test-account").unwrap();
            assert_eq!(restored.len(), 59);
            for (id, title) in &restored {
                assert!(
                    title.progress_merged && !title.pending,
                    "{id}: progress_merged={}, pending={}, desired={:?}, remote={:?}",
                    title.progress_merged,
                    title.pending,
                    title.desired.projection(),
                    title.remote.projection()
                );
            }
            let progress = read_progress_map();
            assert!(progress[&progress_map_key(series_id, &videos[0].id)].watched);
            assert!(progress[&progress_map_key(series_id, &videos[1].id)].watched);
            assert_eq!(
                progress[&progress_map_key(series_id, &videos[0].id)].position_secs,
                20.0
            );
            let remote_series = datastore.items.lock().unwrap()[series_id]
                .projection(&videos)
                .unwrap();
            assert!(remote_series.watched[&videos[0].id]);
            assert!(remote_series.watched[&videos[1].id]);
            assert_eq!(remote_series.resume.unwrap().video_id, videos[0].id);
            assert!(datastore.writes.load(Ordering::Acquire) > 0);
            assert_eq!(
                datastore.confirmations.load(Ordering::Acquire),
                datastore.writes.load(Ordering::Acquire)
            );
            assert!(
                datastore.items.lock().unwrap()["tt0000000"]
                    .projection(&[])
                    .unwrap()
                    .saved
            );
            assert_eq!(fixture._app.get_stremio_status(), "Stremio sync complete");
            assert!(!fixture._app.get_stremio_merge_ready());
            assert!(!fixture._app.get_stremio_busy());
            drop(persistence_worker);
        },
    );
}

fn rejected_ingress_preserves_pending_baseline(fixture: Fixture, key: &str) {
    let mut title = incoming(true);
    title.pending = true;
    save_title(&fixture.connection, &mut title).unwrap();
    let before = storage::get_str(key).unwrap();
    io::block_unreadable(key);
    assert_eq!(
        apply_title(
            &fixture.bridge,
            &fixture.connection.account_id,
            &title,
            true,
            0,
        ),
        Err(ApiError::Rejected),
    );
    assert_eq!(storage::get_str(key).unwrap(), before);
    let restored = load_titles(&fixture.connection.account_id).unwrap();
    let restored = &restored[title.item.id()];
    assert!(restored.pending, "a failed snapshot must remain retryable");
    assert!(!restored.progress_merged);
    assert_eq!(restored.local, title.local);
    assert_eq!(restored.remote, title.remote);
}

#[test]
fn blocked_library_snapshot_rejects_ingress_without_acknowledging_delivery() {
    isolated(
        concat!(
            module_path!(),
            "::blocked_library_snapshot_rejects_ingress_without_acknowledging_delivery"
        ),
        |fixture| rejected_ingress_preserves_pending_baseline(fixture, "library"),
    );
}

#[test]
fn blocked_progress_snapshot_rejects_ingress_without_acknowledging_delivery() {
    isolated(
        concat!(
            module_path!(),
            "::blocked_progress_snapshot_rejects_ingress_without_acknowledging_delivery"
        ),
        |fixture| rejected_ingress_preserves_pending_baseline(fixture, EPISODE_PROGRESS_KEY),
    );
}

#[test]
fn same_parent_remote_history_uses_confirmed_native_episode_ids_without_replacing_cache() {
    isolated(
        concat!(
            module_path!(),
            "::same_parent_remote_history_uses_confirmed_native_episode_ids_without_replacing_cache"
        ),
        |fixture| {
            let native_id = "native:opaque-episode";
            let remote_id = "tt100:1:1";
            let native = Video {
                id: native_id.into(),
                season: Some(1),
                episode: Some(1),
                extra: [
                    ("novaStreamIds".into(), serde_json::json!([remote_id])),
                    (
                        "novaMetadataRevision".into(),
                        serde_json::json!(nova_providers::metadata_revision()),
                    ),
                ]
                .into(),
                ..Default::default()
            };
            write_episodes_cache_for("series", "tt100", &[native]);
            let cache_key = episodes_key("series", "tt100");
            let cached = storage::get_str(&cache_key).unwrap();
            let mut membership = incoming(false);
            membership.desired.watched.clear();
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &membership,
                false,
                0,
            )
            .unwrap();

            let mut remote = LibraryItem::new("tt100", "Remote series", "series", 1_000);
            let remote_videos = vec![nova_stremio::Video {
                id: remote_id.into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }];
            remote
                .apply_projection(
                    &Projection {
                        saved: true,
                        watched: [(remote_id.into(), true)].into(),
                        resume: Some(Resume {
                            video_id: remote_id.into(),
                            position_ms: 10_000,
                            duration_ms: 60_000,
                        }),
                    },
                    &remote_videos,
                    1_000,
                )
                .unwrap();
            let descriptors = vec![serde_json::json!({
                "transportUrl":"https://v3-cinemeta.strem.io/manifest.json",
                "manifest":{"id":"com.linvo.cinemeta","name":"Cinemeta","version":"3.0.14",
                    "types":["series"],"resources":["meta"],"idPrefixes":["tt"]}
            })];
            let body = serde_json::to_vec(&serde_json::json!({"meta":{
                "id":"tt100","type":"series","name":"Remote series",
                "videos":[{"id":remote_id,"season":1,"episode":1}]
            }}))
            .unwrap();
            let (entries, progress) = {
                let state = fixture.bridge.shared.lock().unwrap();
                (state.entries.clone(), state.progress.clone())
            };
            let locals = local_titles(&entries, &progress);
            let resolved =
                stremio_metadata::resolve(&remote, &locals, &descriptors, &|url, _, _| {
                    assert_eq!(url, "https://v3-cinemeta.strem.io/meta/series/tt100.json");
                    Ok(body.clone())
                })
                .unwrap();
            assert!(resolved.native_metadata().is_none());
            let mut title = Title::new(remote);
            title.mapping = mapping(
                resolved,
                nova_config::fnv1a(&serde_json::to_vec(&descriptors).unwrap()),
            );
            assert_eq!(title.mapping.episode_ids[remote_id], native_id);
            assert_eq!(storage::get_str(&cache_key).unwrap(), cached);
            title.desired = Canonical::from_projection(
                &title.item.projection(&title.mapping.videos).unwrap(),
                1_000,
            );
            title.local = apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            let mut progress = read_progress_map();
            let native_key = progress_map_key("tt100", native_id);
            assert!(progress[&native_key].watched);
            assert_eq!(progress[&native_key].position_secs, 10.0);
            assert!(!progress.contains_key(&progress_map_key("tt100", remote_id)));
            assert_eq!(storage::get_str(&cache_key).unwrap(), cached);

            // A subsequent edit made under Nova's native episode ID must be
            // projected back onto the account's episode, not an orphan row.
            let checkpoint = progress.get_mut(&native_key).unwrap();
            checkpoint.watched = false;
            checkpoint.unwatched_at_secs = 3;
            checkpoint.position_secs = 20.0;
            checkpoint.updated_at_secs = 3;
            fixture.bridge.shared.lock().unwrap().progress = progress.clone();
            write_progress_map(&progress);
            let local = local_projection(&title.mapping, &entries, &progress);
            let source =
                source_canonical(&fixture.connection, &title, &local, &entries, &progress).unwrap();
            assert!(!source.watched[remote_id].value);
            assert!(
                source.watched[remote_id].changed_at > title.local.watched[remote_id].changed_at
            );
            assert_eq!(
                source.resume.value.unwrap(),
                Resume {
                    video_id: remote_id.into(),
                    position_ms: 20_000,
                    duration_ms: 60_000,
                }
            );
        },
    );
}

/// A canonical catalog row and two source seasons all synchronize through one
/// account row, while retaining separate native progress and acknowledgements.
#[test]
fn worker_retains_independent_source_bindings_for_one_remote_title() {
    exercise_native_bindings(
        concat!(
            module_path!(),
            "::worker_retains_independent_source_bindings_for_one_remote_title"
        ),
        true,
    );
}

#[test]
fn worker_reuses_existing_native_titles_without_importing_a_canonical_duplicate() {
    exercise_native_bindings(
        concat!(
            module_path!(),
            "::worker_reuses_existing_native_titles_without_importing_a_canonical_duplicate"
        ),
        false,
    );
}

fn exercise_native_bindings(test_name: &'static str, include_canonical: bool) {
    isolated_with_clock(test_name, true, |fixture| {
        use std::io::{Read, Write};
        let metadata = serde_json::json!({"meta":{
            "id":"tt100", "type":"series", "name":"Canonical title", "videos":[
                {"id":"tt100:1:1","season":1,"episode":1},
                {"id":"tt100:1:2","season":1,"episode":2},
                {"id":"tt100:2:1","season":2,"episode":1},
                {"id":"tt100:2:2","season":2,"episode":2}
            ]
        }});
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_http = stop.clone();
        let requests = Arc::new(AtomicU64::new(0));
        let http_requests = requests.clone();
        let body = serde_json::to_vec(&metadata).unwrap();
        let http = crate::app::profiles::thread::spawn(move || {
            while !stop_http.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("metadata accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 2048];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("GET /meta/series/tt100.json "));
                assert!(!request.contains("test-token"));
                http_requests.fetch_add(1, Ordering::AcqRel);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let descriptors = vec![serde_json::json!({
            "transportUrl":format!("http://{address}/manifest.json"),
            "manifest":{"id":"fixture","name":"Fixture","version":"1.0.0",
                "types":["series"],"resources":["meta"],"idPrefixes":["tt"]}
        })];
        let canonical: MetaItem = serde_json::from_value(metadata["meta"].clone()).unwrap();
        write_episodes_cache_for("series", "tt100", &canonical.videos);
        let remote_videos: Vec<_> = canonical
            .videos
            .iter()
            .map(|video| nova_stremio::Video {
                id: video.id.clone(),
                season: video.season,
                episode: video.episode,
                ..Default::default()
            })
            .collect();
        let mut remote_item = LibraryItem::new("tt100", "Canonical title", "series", 5_000);
        remote_item.0.insert(
            "unknownAccountField".into(),
            serde_json::json!({"keep":[1,2,3]}),
        );
        remote_item
            .apply_projection(
                &Projection {
                    saved: true,
                    watched: remote_videos
                        .iter()
                        .enumerate()
                        .map(|(index, video)| (video.id.clone(), index % 2 == 0))
                        .collect(),
                    ..Default::default()
                },
                &remote_videos,
                5_000,
            )
            .unwrap();
        struct Account {
            item: Mutex<LibraryItem>,
            descriptors: Vec<serde_json::Value>,
            writes: AtomicU64,
        }
        struct MockAccount(Arc<Account>);
        impl nova_stremio::Transport for MockAccount {
            fn send(
                &self,
                request: nova_stremio::Request,
            ) -> Result<nova_stremio::Response, ApiError> {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["authKey"], "test-token");
                let mut item = self.0.item.lock().unwrap();
                let result = match request.url.rsplit('/').next().unwrap() {
                    "getUser" => {
                        serde_json::json!({"_id":"test-account","email":"test@example.invalid"})
                    }
                    "addonCollectionGet" => serde_json::json!({"addons":self.0.descriptors}),
                    "datastoreMeta" => serde_json::json!([[item.id(), item.modified_ms()]]),
                    "datastoreGet" => serde_json::json!([*item]),
                    "datastorePut" => {
                        let changes: Vec<LibraryItem> =
                            serde_json::from_value(body["changes"].clone()).unwrap();
                        assert_eq!(changes.len(), 1);
                        assert_eq!(changes[0].id(), "tt100");
                        *item = changes.into_iter().next().unwrap();
                        self.0.writes.fetch_add(1, Ordering::AcqRel);
                        serde_json::json!({"success":true})
                    }
                    endpoint => panic!("unexpected account endpoint {endpoint}"),
                };
                Ok(nova_stremio::Response {
                    status: 200,
                    retry_after: None,
                    body: serde_json::to_vec(&serde_json::json!({"result":result}))
                        .unwrap()
                        .into(),
                })
            }
        }
        let mut entries = Vec::new();
        let mut progress = HashMap::new();
        let mut caches = Vec::new();
        for (id, media_type, season) in [
            ("tt100", "series", 0),
            ("native-season-a", "anime", 1),
            ("native-season-b", "anime", 2),
        ] {
            if season == 0 && !include_canonical {
                continue;
            }
            entries.push(LibraryEntry {
                id: id.into(),
                type_: media_type.into(),
                name: id.into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                genres: Vec::new(),
                description: String::new(),
                categories: Vec::new(),
                watch_status: WatchStatus::Auto,
                imported_tracking_status: None,
                added_at_secs: 2,
            });
            if season == 0 {
                continue;
            }
            let mut native = Vec::new();
            for episode in 1..=2 {
                let native_id = format!("{id}-episode-{episode}");
                let remote_id = format!("tt100:{season}:{episode}");
                native.push(Video {
                    id: native_id.clone(),
                    season: Some(season),
                    episode: Some(episode),
                    extra: [
                        ("novaStreamIds".into(), serde_json::json!([remote_id])),
                        (
                            "novaMetadataRevision".into(),
                            serde_json::json!(nova_providers::metadata_revision()),
                        ),
                    ]
                    .into(),
                    ..Default::default()
                });
                if episode == 2 {
                    progress.insert(
                        progress_map_key(id, &native_id),
                        EpisodeProgress {
                            series_id: id.into(),
                            episode_id: native_id,
                            watched: true,
                            updated_at_secs: 2,
                            ..Default::default()
                        },
                    );
                }
            }
            // Equal season/episode numbers alone cannot create coverage.
            native.push(Video {
                id: format!("{id}-unconfirmed"),
                season: Some(season),
                episode: Some(1),
                ..Default::default()
            });
            write_episodes_cache_for(media_type, id, &native);
            let key = episodes_key(media_type, id);
            caches.push((key.clone(), storage::get_str(&key).unwrap()));
        }
        {
            let mut state = fixture.bridge.shared.lock().unwrap();
            state.entries = entries.clone();
            state.progress = progress.clone();
        }
        write_persisted_library(&entries);
        write_progress_map(&progress);
        let account = Arc::new(Account {
            item: Mutex::new(remote_item),
            descriptors,
            writes: AtomicU64::new(0),
        });
        let persistence_worker = persistence::start().unwrap();
        let bridge = fixture.bridge.clone();
        let connection = fixture.connection;
        let remote = account.clone();
        let task = crate::app::profiles::thread::spawn(move || {
            let result = (|| {
                let mut worker =
                    Worker::new(bridge.clone(), Client::new(MockAccount(remote.clone())));
                worker.connection = Some(connection.clone());
                worker.verified = true;
                for _ in 0..6 {
                    worker.force = true;
                    worker.tick()?;
                }
                assert_eq!(worker.titles.len(), 3);
                assert!(
                    worker.pending.is_empty(),
                    "all source bindings must resolve"
                );
                let before = worker.titles.clone();
                assert_eq!(
                    before
                        .values()
                        .filter(|title| title.binding.is_none())
                        .count(),
                    1
                );
                for title in before.values() {
                    assert!(title.progress_merged && !title.pending);
                    assert!(title.desired.watched.values().all(|field| field.value));
                    assert_eq!(
                        title.mapping.episode_ids.len(),
                        if title.binding.is_none() { 4 } else { 2 }
                    );
                    if let Some(binding) = &title.binding {
                        assert_eq!(binding.media_type, "anime");
                        assert_eq!(binding.id, title.mapping.id);
                        assert!(
                            title
                                .mapping
                                .episode_ids
                                .values()
                                .all(|id| id.starts_with(&binding.id))
                        );
                    }
                }
                persistence::flush().map_err(|_| ApiError::Rejected)?;
                let restored = load_titles(&connection.account_id).unwrap();
                assert_eq!(restored.len(), 3);
                for (slot, title) in &restored {
                    let previous = &before[slot];
                    assert_eq!(title.local, previous.local);
                    assert_eq!(title.remote, previous.remote);
                    assert_eq!(title.desired, previous.desired);
                    assert_eq!(title.mapping.episode_ids, previous.mapping.episode_ids);
                    assert_eq!(title_slot(title), *slot);
                }
                // Run the restored actor too: renewing metadata must resolve
                // each binding against its own source, not all aliases at once.
                let mut restarted = Worker::new(bridge, Client::new(MockAccount(remote)));
                restarted.connection = Some(connection);
                restarted.force = true;
                restarted.tick()?;
                assert_eq!(restarted.titles.len(), 3);
                assert!(restarted.pending.is_empty());
                Ok::<_, ApiError>(())
            })();
            crate::app::profiles::invoke_from_event_loop(|| slint::quit_event_loop().unwrap())
                .unwrap();
            result
        });
        let watchdog = slint::Timer::default();
        watchdog.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(45),
            || slint::quit_event_loop().unwrap(),
        );
        slint::run_event_loop().unwrap();
        watchdog.stop();
        let result = task.join();
        stop.store(true, Ordering::Release);
        http.join().unwrap();
        assert_eq!(result.unwrap(), Ok(()));
        assert!(requests.load(Ordering::Acquire) > 0);
        assert!(account.writes.load(Ordering::Acquire) > 0);
        let progress = read_progress_map();
        for (id, season) in [("native-season-a", 1), ("native-season-b", 2)] {
            for episode in 1..=2 {
                assert!(
                    progress[&progress_map_key(id, &format!("{id}-episode-{episode}"))].watched
                );
                // The ordinary Home projection retains proven aliases under
                // the same native parent. They are read views, not a second
                // canonical title or a fresh playback action.
                if let Some(alias) =
                    progress.get(&progress_map_key(id, &format!("tt100:{season}:{episode}")))
                {
                    let native =
                        &progress[&progress_map_key(id, &format!("{id}-episode-{episode}"))];
                    assert_eq!(alias.watched, native.watched);
                    assert_eq!(alias.position_secs, native.position_secs);
                    assert_eq!(alias.duration_secs, native.duration_secs);
                    assert_eq!(alias.updated_at_secs, native.updated_at_secs);
                    assert_eq!(alias.unwatched_at_secs, native.unwatched_at_secs);
                    assert_eq!(alias.play_count, native.play_count);
                }
            }
            assert!(!progress.contains_key(&progress_map_key(id, &format!("{id}-unconfirmed"))));
        }
        for video in &remote_videos {
            let canonical = progress.get(&progress_map_key("tt100", &video.id));
            if include_canonical {
                assert!(canonical.unwrap().watched);
            } else {
                assert!(
                    canonical.is_none(),
                    "native history must not be cloned into a new identity"
                );
            }
        }
        let entries = fixture.bridge.shared.lock().unwrap().entries.clone();
        let local = local_titles(&entries, &progress);
        assert!(
            local
                .iter()
                .filter(|title| title.id.starts_with("native-season-"))
                .all(|title| title.media_type == "anime"),
            "derived canonical history must not invent series-typed native parents"
        );
        assert_eq!(entries.len(), if include_canonical { 3 } else { 2 });
        assert_eq!(
            entries.iter().any(|entry| entry.id == "tt100"),
            include_canonical
        );
        for (key, cached) in caches {
            assert_eq!(storage::get_str(&key).unwrap(), cached);
        }
        let item = account.item.lock().unwrap();
        assert!(
            item.projection(&remote_videos)
                .unwrap()
                .watched
                .values()
                .all(|watched| *watched)
        );
        assert_eq!(
            item.0["unknownAccountField"],
            serde_json::json!({"keep":[1,2,3]})
        );
        drop(persistence_worker);
    });
}

#[test]
fn legacy_source_title_migrates_without_losing_pending_baselines() {
    isolated(
        concat!(
            module_path!(),
            "::legacy_source_title_migrates_without_losing_pending_baselines"
        ),
        |fixture| {
            let account = &fixture.connection.account_id;
            let mut legacy = incoming(true);
            legacy.mapping.media_type = "anime".into();
            legacy.mapping.id = "native-season".into();
            legacy.mapping.episode_ids = [("tt100:1:1".into(), "native-episode".into())].into();
            legacy.pending = true;
            legacy.local.saved = Stamped {
                value: false,
                changed_at: 500,
            };
            legacy.remote = Canonical::from_projection(
                &Projection {
                    saved: true,
                    watched: [("tt100:1:1".into(), false)].into(),
                    ..Default::default()
                },
                800,
            );
            let old_key = title_key(account, "tt100");
            let mut raw = serde_json::to_value(&legacy).unwrap();
            raw.as_object_mut().unwrap().remove("binding");
            storage::try_write_batch(&[(old_key.clone(), Some(raw.to_string()))]).unwrap();
            let migrated = load_titles(account).unwrap();
            assert_eq!(migrated.len(), 1);
            let migrated = migrated.values().next().unwrap();
            let binding = migrated.binding.as_ref().unwrap();
            assert_eq!(binding.media_type, "anime");
            assert_eq!(binding.id, "native-season");
            assert!(migrated.pending);
            assert_eq!(migrated.local, legacy.local);
            assert_eq!(migrated.remote, legacy.remote);
            assert_eq!(migrated.desired, legacy.desired);
            assert_eq!(migrated.mapping.episode_ids, legacy.mapping.episode_ids);
            assert_eq!(migrated.mapping.checked_at, 0);
            let new_key = title_storage_key(account, migrated);
            assert_ne!(old_key, new_key);
            assert!(storage::get_str(&old_key).is_none());
            let persisted: Title =
                serde_json::from_str(&storage::get_str(&new_key).unwrap()).unwrap();
            assert!(persisted.pending);
            assert_eq!(persisted.local, legacy.local);
            assert_eq!(persisted.remote, legacy.remote);
            assert_eq!(persisted.desired, legacy.desired);
            assert_eq!(storage::scan_prefix(&title_prefix(account)).len(), 1);

            // An interrupted old process can leave its obsolete copy behind.
            // Loading must remove it without overwriting the newer binding.
            let mut winner = persisted.clone();
            winner.pending = false;
            winner.progress_merged = true;
            winner.local.saved.changed_at = 2_000;
            winner
                .desired
                .watched
                .get_mut("tt100:1:1")
                .unwrap()
                .changed_at = 3_000;
            storage::try_write_batch(&[
                (
                    new_key.clone(),
                    Some(serde_json::to_string(&winner).unwrap()),
                ),
                (old_key.clone(), Some(raw.to_string())),
            ])
            .unwrap();
            let loaded = load_titles(account).unwrap();
            assert_eq!(loaded.len(), 1);
            let loaded = &loaded[&title_slot(&winner)];
            assert!(!loaded.pending);
            assert!(loaded.progress_merged);
            assert_eq!(loaded.local, winner.local);
            assert_eq!(loaded.remote, winner.remote);
            assert_eq!(loaded.desired, winner.desired);
            assert!(storage::get_str(&old_key).is_none());
            let retained: Title =
                serde_json::from_str(&storage::get_str(&new_key).unwrap()).unwrap();
            assert_eq!(retained.desired, winner.desired);
            assert_eq!(storage::scan_prefix(&title_prefix(account)).len(), 1);
        },
    );
}

#[test]
fn initial_binding_union_keeps_source_clocks_and_yields_to_fresh_unwatch() {
    isolated(
        concat!(
            module_path!(),
            "::initial_binding_union_keeps_source_clocks_and_yields_to_fresh_unwatch"
        ),
        |fixture| {
            let connection = &fixture.connection;
            let episode = "tt100:1:1";
            let watched_key = format!(
                "{}watched:{}",
                shared_prefix(&connection.account_id, "tt100"),
                serde_json::to_string(episode).unwrap()
            );
            let saved_key = format!("{}saved", shared_prefix(&connection.account_id, "tt100"));
            let negative = Stamped {
                value: false,
                changed_at: 5_000,
            };
            let owner = nova_sync::local_store().unwrap();
            {
                let mut store = owner.lock().unwrap();
                for key in [&watched_key, &saved_key] {
                    let raw = serde_json::to_string(&negative).unwrap();
                    store.apply(
                        DOMAIN,
                        key,
                        nova_sync::Record {
                            value: Some(raw),
                            version: nova_sync::Version::new(5_000, 7, 42, false),
                        },
                    );
                }
                store.save().unwrap();
            }
            let mut title = incoming(true);
            title.binding = Some(NativeBinding {
                media_type: "anime".into(),
                id: "native-season".into(),
            });
            title.mapping.media_type = "anime".into();
            title.mapping.id = "native-season".into();
            title.mapping.episode_ids = [(episode.into(), "native-episode".into())].into();
            title.remote.saved = negative.clone();
            title.remote.watched.insert(episode.into(), negative);
            assert!(!title.progress_merged);
            save_title(connection, &mut title).unwrap();
            let positive = Stamped {
                value: true,
                changed_at: 1_000,
            };
            assert_eq!(title.desired.saved, positive);
            assert_eq!(title.desired.watched[episode], positive);
            {
                let store = owner.lock().unwrap();
                for key in [&watched_key, &saved_key] {
                    let row = store.record(DOMAIN, key).unwrap();
                    assert_eq!(
                        row.version.ts, 5_000,
                        "delivery must retain the observed outer timestamp"
                    );
                    assert_eq!(row.version.counter, 8);
                    assert_eq!(
                        serde_json::from_str::<Stamped<bool>>(row.value.as_ref().unwrap()).unwrap(),
                        positive
                    );
                }
            }
            let stale_union = title.clone();

            // A source action arriving while metadata/HTTP is in flight has its
            // own event clock, even when the native snapshot is still watched.
            let fresh = Stamped {
                value: false,
                changed_at: 6_000,
            };
            {
                let mut store = owner.lock().unwrap();
                let event = format!(
                    "{}:watched:{}",
                    stremio_events::event_key(&connection.account_id, "anime", "native-season"),
                    serde_json::to_string("native-episode").unwrap()
                );
                store.apply(
                    DOMAIN,
                    &event,
                    nova_sync::Record {
                        value: Some(serde_json::to_string(&fresh).unwrap()),
                        version: nova_sync::Version::new(6_000, 0, 43, false),
                    },
                );
                store.save().unwrap();
            }
            let still_watched: HashMap<_, _> = [(
                progress_map_key("native-season", "native-episode"),
                EpisodeProgress {
                    series_id: "native-season".into(),
                    episode_id: "native-episode".into(),
                    watched: true,
                    updated_at_secs: 1,
                    ..Default::default()
                },
            )]
            .into();
            let projection = local_projection(&title.mapping, &[], &still_watched);
            let source =
                source_canonical(connection, &title, &projection, &[], &still_watched).unwrap();
            assert_eq!(source.watched[episode], fresh);
            title
                .desired
                .watched
                .insert(episode.into(), source.watched[episode].clone());
            title.desired.saved = fresh.clone();
            save_title(connection, &mut title).unwrap();
            assert_eq!(
                read_shared(connection, "tt100").unwrap().watched[episode],
                fresh
            );
            // A stale initial union only has authority over the exact server
            // negative it observed, never a subsequently received native edit.
            let mut stale_union = stale_union;
            save_title(connection, &mut stale_union).unwrap();
            assert_eq!(stale_union.desired.watched[episode], fresh);
            assert_eq!(stale_union.desired.saved, fresh);
            let store = owner.lock().unwrap();
            for key in [&watched_key, &saved_key] {
                let row = store.record(DOMAIN, key).unwrap();
                assert_eq!(row.version.ts, 6_000);
                assert_eq!(row.version.counter, 0);
            }
            assert_eq!(
                stremio_events::load(&store, &connection.account_id, "anime", "native-season")
                    .unwrap()
                    .watched["native-episode"],
                fresh
            );
        },
    );
}

#[test]
fn unchanged_materialization_has_no_receipt_or_persistence_side_effects() {
    isolated(
        concat!(
            module_path!(),
            "::unchanged_materialization_has_no_receipt_or_persistence_side_effects"
        ),
        |fixture| {
            let _writer = persistence::start().unwrap();
            let title = incoming(true);
            apply_title(
                &fixture.bridge,
                &fixture.connection.account_id,
                &title,
                true,
                0,
            )
            .unwrap();
            persistence::flush().unwrap();
            let owner = nova_sync::local_store().unwrap();
            let clock = owner.lock().unwrap().clock();
            let journal = storage::scan_prefix(nova_tracking::EVENT_PREFIX);
            let library = storage::get_str("library");
            let progress = storage::get_str(EPISODE_PROGRESS_KEY);
            for _ in 0..3 {
                let prepared =
                    materialize::prepare(&fixture.bridge, &fixture.connection, &title, true, 0)
                        .unwrap()
                        .unwrap();
                match materialize::accept(&fixture.bridge, prepared, 0).unwrap() {
                    materialize::Accepted::Ready(desired, receipt) => {
                        assert!(desired.saved.value);
                        assert!(desired.watched["tt100:1:1"].value);
                        assert!(
                            receipt.is_none(),
                            "unchanged records must not enqueue a save"
                        );
                    }
                    materialize::Accepted::Retry => {
                        panic!("unchanged preparation must remain current")
                    }
                }
            }
            persistence::flush().unwrap();
            assert_eq!(owner.lock().unwrap().clock(), clock);
            assert_eq!(storage::scan_prefix(nova_tracking::EVENT_PREFIX), journal);
            assert_eq!(storage::get_str("library"), library);
            assert_eq!(storage::get_str(EPISODE_PROGRESS_KEY), progress);
        },
    );
}

#[test]
fn prepared_materialization_retries_after_a_local_history_edit() {
    isolated(
        concat!(
            module_path!(),
            "::prepared_materialization_retries_after_a_local_history_edit"
        ),
        |fixture| {
            let prepared = materialize::prepare(
                &fixture.bridge,
                &fixture.connection,
                &incoming(true),
                true,
                0,
            )
            .unwrap()
            .unwrap();
            let key = progress_map_key("tt100", "tt100:1:1");
            let local = EpisodeProgress {
                series_id: "tt100".into(),
                episode_id: "tt100:1:1".into(),
                watched: false,
                unwatched_at_secs: 20,
                updated_at_secs: 20,
                ..Default::default()
            };
            fixture
                .bridge
                .shared
                .lock()
                .unwrap()
                .progress
                .insert(key.clone(), local.clone());
            assert!(matches!(
                materialize::accept(&fixture.bridge, prepared, 0).unwrap(),
                materialize::Accepted::Retry
            ));
            let state = fixture.bridge.shared.lock().unwrap();
            assert_eq!(state.progress[&key], local);
            assert!(state.entries.is_empty());
            assert!(read_progress_map().is_empty());
        },
    );
}

#[test]
fn prepared_materialization_retries_after_a_newer_mesh_action() {
    isolated(
        concat!(
            module_path!(),
            "::prepared_materialization_retries_after_a_newer_mesh_action"
        ),
        |fixture| {
            let mut title = incoming(true);
            let prepared =
                materialize::prepare(&fixture.bridge, &fixture.connection, &title, true, 0)
                    .unwrap()
                    .unwrap();
            title.desired.saved = Stamped {
                value: false,
                changed_at: 2_000,
            };
            title.desired.watched.insert(
                "tt100:1:1".into(),
                Stamped {
                    value: false,
                    changed_at: 2_000,
                },
            );
            save_title(&fixture.connection, &mut title).unwrap();
            assert!(matches!(
                materialize::accept(&fixture.bridge, prepared, 0).unwrap(),
                materialize::Accepted::Retry
            ));
            assert!(fixture.bridge.shared.lock().unwrap().entries.is_empty());
            assert!(fixture.bridge.shared.lock().unwrap().progress.is_empty());
            assert_eq!(
                read_shared(&fixture.connection, "tt100").unwrap(),
                title.desired
            );
        },
    );
}

#[test]
fn busy_sync_owner_does_not_block_materialization_or_the_event_loop() {
    isolated_with_clock(
        concat!(
            module_path!(),
            "::busy_sync_owner_does_not_block_materialization_or_the_event_loop"
        ),
        true,
        |fixture| {
            let prepared = materialize::prepare(
                &fixture.bridge,
                &fixture.connection,
                &incoming(true),
                true,
                0,
            )
            .unwrap()
            .unwrap();
            let owner = nova_sync::local_store().unwrap();
            let (locked, ready) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let holder = thread::spawn(move || {
                let _lock = owner.lock().unwrap();
                locked.send(()).unwrap();
                // Bound a regression so a blocking accept fails instead of hanging the suite.
                let _ = released.recv_timeout(Duration::from_secs(3));
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            let accepted = Rc::new(std::cell::Cell::new(false));
            let result = accepted.clone();
            let bridge = fixture.bridge.clone();
            slint::Timer::single_shot(Duration::from_millis(1), move || {
                let start = Instant::now();
                let retry = matches!(
                    materialize::accept(&bridge, prepared, 0).unwrap(),
                    materialize::Accepted::Retry
                );
                result.set(retry && start.elapsed() < Duration::from_millis(250));
            });
            let responsive = Rc::new(std::cell::Cell::new(false));
            let heartbeat = responsive.clone();
            let start = Instant::now();
            slint::Timer::single_shot(Duration::from_millis(20), move || {
                heartbeat.set(start.elapsed() < Duration::from_millis(500));
                slint::quit_event_loop().unwrap();
            });
            slint::run_event_loop().unwrap();
            let _ = release.send(());
            holder.join().unwrap();
            assert!(accepted.get(), "busy owner must return Retry promptly");
            assert!(responsive.get(), "an unrelated UI timer must keep running");
            assert!(fixture.bridge.shared.lock().unwrap().entries.is_empty());
        },
    );
}

fn ordered_ingress_entry(id: &str) -> LibraryEntry {
    LibraryEntry {
        id: id.into(),
        type_: "series".into(),
        name: id.into(),
        year: String::new(),
        poster_url: String::new(),
        background_url: String::new(),
        genres: Vec::new(),
        description: String::new(),
        categories: Vec::new(),
        watch_status: WatchStatus::Auto,
        imported_tracking_status: None,
        added_at_secs: 20,
    }
}

#[test]
fn ordered_external_writer_preserves_surrounding_local_saves_and_remote_origin() {
    isolated(
        concat!(
            module_path!(),
            "::ordered_external_writer_preserves_surrounding_local_saves_and_remote_origin"
        ),
        |fixture| {
            let writer = persistence::start().unwrap();
            let owner = nova_sync::local_store().unwrap();
            let store = owner.lock().unwrap();
            // Keep the real writer busy while the local save and remote patch are queued.
            // The remote patch must merge against the preceding committed save.
            let before = ordered_ingress_entry("local-before");
            let imported = ordered_ingress_entry("tt100");
            write_persisted_library(std::slice::from_ref(&before));
            let remote_progress = EpisodeProgress {
                series_id: "tt100".into(),
                episode_id: "tt100:1:1".into(),
                watched: true,
                updated_at_secs: 1,
                ..Default::default()
            };
            let receipt = persistence::try_external(persistence::ExternalPatch {
                library: Some(persistence::ExternalLibraryEdit {
                    id: imported.id.clone(),
                    media_type: imported.type_.clone(),
                    entry: Some(imported.clone()),
                }),
                progress: [(
                    progress_map_key("tt100", "tt100:1:1"),
                    remote_progress.clone(),
                )]
                .into(),
            })
            .unwrap();
            drop(store);
            materialize::wait(receipt).unwrap();
            persistence::flush().unwrap();
            let committed = read_persisted_library();
            assert_eq!(committed.len(), 2);
            assert!(
                committed.contains(&before),
                "remote patch must retain the preceding local save"
            );
            assert!(committed.contains(&imported));
            // A subsequent unrelated edit must retain the accepted remote entry.
            let after = ordered_ingress_entry("local-after");
            let mut subsequent = committed;
            subsequent.push(after.clone());
            write_persisted_library(&subsequent);
            persistence::flush().unwrap();
            drop(writer);
            let entries = read_persisted_library();
            assert_eq!(entries.len(), 3);
            for entry in [before, imported, after] {
                assert!(entries.contains(&entry));
            }
            assert_eq!(
                read_progress_map()[&progress_map_key("tt100", "tt100:1:1")],
                remote_progress
            );
            let store = owner.lock().unwrap();
            assert!(
                store
                    .record(DOMAIN_LIBRARY, &library::library_key("series", "tt100"))
                    .unwrap()
                    .value
                    .is_some()
            );
            assert!(
                store
                    .record(DOMAIN_PROGRESS, &progress_map_key("tt100", "tt100:1:1"))
                    .unwrap()
                    .value
                    .is_some()
            );
            assert!(
                stremio_events::load(&store, &fixture.connection.account_id, "series", "tt100")
                    .is_none(),
                "writer-thread guards must prevent native source upload authority"
            );
            drop(store);
            let events: Vec<nova_tracking::WatchEvent> =
                storage::scan_prefix(nova_tracking::EVENT_PREFIX)
                    .into_iter()
                    .map(|(_, raw)| serde_json::from_str(&raw).unwrap())
                    .collect();
            let remote: Vec<_> = events
                .iter()
                .filter(|event| {
                    event
                        .changes
                        .iter()
                        .any(|change| change.series_id == "tt100")
                })
                .collect();
            assert!(!remote.is_empty());
            assert!(
                remote
                    .iter()
                    .all(|event| event.origin == nova_tracking::EventOrigin::PairedDevice),
                "remote observations must not authorize tracker uploads"
            );
        },
    );
}

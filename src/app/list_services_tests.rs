use super::*;
use serde_json::json;
fn selected() -> Selection {
    Selection {
        enabled: true,
        since: 100,
        include_history: false,
        grant: "grant".into(),
        event_boundary: 9,
    }
}
#[test]
fn selected_scope_is_opt_in_and_explicit_exclusions_win_full_scope() {
    assert!(effective_selection(false, None).is_none());
    assert!(effective_selection(true, None).unwrap().include_history);
    assert!(
        effective_selection(true, Some(selected()))
            .unwrap()
            .include_history
    );
    let mut excluded = selected();
    excluded.enabled = false;
    assert!(effective_selection(true, Some(excluded)).is_none());
    assert_eq!(
        effective_selection(false, Some(selected())),
        Some(selected())
    );
}
#[test]
fn semantic_actions_allow_same_second_watches_and_rewatches_without_history_replay() {
    let selection = selected();
    let mut progress = EpisodeProgress {
        watched: true,
        updated_at_secs: 100,
        episode_id: "one".into(),
        ..Default::default()
    };
    assert!(!history_allowed(&selection, &progress, 9));
    assert!(history_allowed(&selection, &progress, 10));
    progress.updated_at_secs = 1_000;
    assert!(
        !history_allowed(&selection, &progress, 9),
        "position updates do not authorize old history"
    );
    progress.watched = false;
    assert!(!history_allowed(&selection, &progress, 10));
    let mut sharing = selection;
    sharing.include_history = true;
    progress.watched = true;
    assert!(history_allowed(&sharing, &progress, 0));
}
#[test]
fn shared_grants_never_copy_device_journal_counters() {
    let source = selected();
    let decoded: Selection =
        serde_json::from_str(&serde_json::to_string(&source).unwrap()).unwrap();
    assert_eq!(decoded.grant, source.grant);
    assert_eq!(decoded.event_boundary, 0);
}
#[test]
fn scope_and_selection_revocations_survive_a_full_read_queue() {
    let h = Arc::new(Handle::new());
    for _ in 0..32 {
        h.send(Command::Sync(0, 0));
    }
    h.control(0, Command::Scope(0, false, 0));
    let mut excluded = selected();
    excluded.enabled = false;
    h.control(
        0,
        Command::Select(
            Review {
                service: 0,
                media_type: "movie".into(),
                id: "tt1234567".into(),
            },
            excluded,
            0,
        ),
    );
    h.control(0, Command::Automatic(0, false, 0));
    let commands = h.urgent.lock().unwrap();
    assert_eq!(commands.len(), 3);
    assert!(matches!(commands[0], Command::Scope(0, false, 0)));
    assert!(matches!(&commands[1], Command::Select(_, s, _) if !s.enabled));
    assert!(matches!(commands[2], Command::Automatic(0, false, 0)));
    drop(commands);
    let transport = SessionTransport {
        inner: Arc::new(Fake {
            requests: Mutex::new(Vec::new()),
        }),
        handle: h,
    };
    assert!(
        matches!(
            nova_list_services::Transport::send(
                &transport,
                nova_list_services::Request {
                    method: "GET",
                    url: "https://api.simkl.com/sync/activities".into(),
                    bearer: None,
                    body: None,
                }
            ),
            Err(ApiError::Rejected)
        ),
        "a cancelled library pass must not fetch another page"
    );
}
#[test]
fn resume_requires_real_finite_duration_and_unfinished_playback() {
    let mut progress = EpisodeProgress {
        position_secs: 30.0,
        duration_secs: 120.0,
        ..Default::default()
    };
    assert_eq!(resume_percent(&progress), Some(25.0));
    progress.duration_secs = 0.0;
    assert_eq!(resume_percent(&progress), None);
    progress.duration_secs = f64::NAN;
    assert_eq!(resume_percent(&progress), None);
    progress.duration_secs = 120.0;
    progress.watched = true;
    assert_eq!(resume_percent(&progress), None);
}
#[test]
fn conflicting_shared_catalog_claims_do_not_authorize_a_merge() {
    let a = MediaIds {
        imdb: Some("tt1234567".into()),
        tmdb: Some(10),
        ..Default::default()
    };
    let b = MediaIds {
        imdb: Some("tt7654321".into()),
        tmdb: Some(10),
        ..Default::default()
    };
    assert!(!safe_match(&a, &b));
    assert!(!compatible_kind(MediaType::Movie, MediaType::Show));
}
struct Fake {
    requests: Mutex<Vec<(String, serde_json::Value)>>,
}
#[test]
fn episode_mapping_requires_current_canonical_proof_and_unique_coverage() {
    let unproven: Video =
        serde_json::from_value(json!({"id":"opaque","season":1,"episode":1})).unwrap();
    assert!(episode_mapping(Some("tt1234567"), &[unproven]).is_empty());
    let canonical: Video = serde_json::from_value(json!({"id":"tt1234567:2:3"})).unwrap();
    assert_eq!(
        episode_mapping(Some("tt1234567"), &[canonical])[0].1,
        Episode {
            season: 2,
            number: 3
        }
    );
    let proven: Video = serde_json::from_value(json!({"id":"native-a","novaStreamIds":["tt1234567:2:3"],"novaMetadataRevision":nova_providers::metadata_revision()})).unwrap();
    let mut duplicate = proven.clone();
    duplicate.id = "native-b".into();
    assert!(episode_mapping(Some("tt1234567"), &[proven.clone(), duplicate]).is_empty());
    let mut stale = proven;
    stale.extra.insert(
        "novaMetadataRevision".into(),
        json!(nova_providers::metadata_revision().wrapping_add(1)),
    );
    assert!(episode_mapping(Some("tt1234567"), &[stale]).is_empty());
}
impl nova_list_services::Transport for Fake {
    fn send(
        &self,
        request: nova_list_services::Request,
    ) -> Result<nova_list_services::Response, ApiError> {
        let path = request
            .url
            .strip_prefix("https://api.mdblist.com")
            .unwrap()
            .split('?')
            .next()
            .unwrap();
        if request.method == "POST" {
            self.requests
                .lock()
                .unwrap()
                .push((path.to_owned(), request.body.clone().unwrap()));
        }
        let body = if request.method == "POST" {
            json!({"added":{"movies":1}})
        } else if path == "/sync/playback" {
            json!([])
        } else if path == "/sync/watched" {
            json!({"movies":[{"movie":{"ids":{"imdb":"tt2222222"},"title":"Unrelated"}}]})
        } else {
            json!({"movies":[],"shows":[]})
        };
        Ok(nova_list_services::Response {
            status: 200,
            retry_after: None,
            has_more: false,
            body,
        })
    }
}
fn account() -> Connection {
    Connection {
        version: 1,
        account: Account {
            id: "owner".into(),
            name: "Owner".into(),
        },
        epoch: "epoch".into(),
        credentials: Credentials {
            service: Service::MdbList,
            client_id: String::new(),
            token: Secret::new("test-api-key".into()).unwrap(),
        },
    }
}
fn movie(id: &str) -> LibraryEntry {
    imported_entry(&RemoteTitle {
        ids: MediaIds {
            imdb: Some(id.into()),
            ..Default::default()
        },
        media_type: MediaType::Movie,
        title: "Movie".into(),
        year: None,
        status: ListStatus::Planning,
        watched: false,
        episodes: Vec::new(),
        progress: None,
        playback_episode: None,
    })
    .unwrap()
}
#[test]
fn actor_filters_remote_imports_and_exports_by_account_title_and_current_consent() {
    const ROOT: &str = "NOVA_LIST_SERVICES_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-list-services-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
            .args(["--exact", "app::list_services::tests::actor_filters_remote_imports_and_exports_by_account_title_and_current_consent", "--nocapture"]).output().unwrap();
        let completed = root.join("completed").exists();
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
    let (hi, _) = mpsc::channel();
    let (lo, _) = mpsc::channel();
    let bridge = Bridge::new(
        app.as_weak(),
        PosterTx { hi, lo },
        Arc::new(AtomicU64::new(0)),
        player,
        DownloadCoordinator::new(root.join("downloads")),
    );
    let fake = Arc::new(Fake {
        requests: Mutex::new(Vec::new()),
    });
    let mut worker = Worker::new(bridge.clone(), Client::new(fake.clone()));
    let connection = account();
    worker.connections[1] = Some(connection.clone());
    worker.verified[1] = true;
    worker
        .set_record("connection:1", Some(&connection))
        .unwrap();
    let mut selection = selected();
    selection.include_history = true;
    worker
        .set_record(
            &selection_key(1, &connection, "movie", "tt1111111"),
            Some(&selection),
        )
        .unwrap();
    {
        let mut state = bridge.shared.lock().unwrap();
        state.entries = vec![movie("tt1111111"), movie("tt2222222")];
        state.progress.insert(
            progress_map_key("tt1111111", "tt1111111"),
            EpisodeProgress {
                watched: true,
                series_id: "tt1111111".into(),
                episode_id: "tt1111111".into(),
                ..Default::default()
            },
        );
    }
    assert_eq!(worker.merge(1, 0, None).unwrap(), 0);
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (_, body) in requests.iter() {
        let raw = body.to_string();
        assert!(raw.contains("tt1111111"));
        assert!(!raw.contains("tt2222222"));
    }
    drop(requests);
    assert!(
        !bridge
            .shared
            .lock()
            .unwrap()
            .progress
            .contains_key(&progress_map_key("tt2222222", "tt2222222")),
        "unselected remote history must not import"
    );
    let other = Connection {
        account: Account {
            id: "other".into(),
            name: "Other".into(),
        },
        ..connection.clone()
    };
    assert!(
        worker
            .enabled(1, &other, "movie", "tt1111111")
            .unwrap()
            .is_none()
    );
    worker
        .set_record::<Connection>("connection:1", None)
        .unwrap();
    assert_eq!(worker.merge(1, 0, None), Err(ApiError::Authentication));
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
    std::fs::write(root.join("completed"), "ok").unwrap();
}

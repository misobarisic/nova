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
                    trakt_client_id: None,
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
            pagination: None,
            body,
        })
    }
}
fn account() -> Connection {
    Connection {
        version: 1,
        refresh: None,
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
    trakt_actor_regressions(&bridge);
    bulk_item_failure_regressions(&bridge);
    std::fs::write(root.join("completed"), "ok").unwrap();
}

struct ItemFailureFake {
    path: &'static str,
    failure: ApiError,
    requests: Mutex<Vec<(String, String)>>,
}
impl nova_list_services::Transport for ItemFailureFake {
    fn send(
        &self,
        request: nova_list_services::Request,
    ) -> Result<nova_list_services::Response, ApiError> {
        let url = url::Url::parse(&request.url).unwrap();
        let body = if request.method == "POST" {
            let body = request.body.as_ref().unwrap().to_string();
            self.requests
                .lock()
                .unwrap()
                .push((url.path().into(), body.clone()));
            if url.path() == self.path && body.contains("tt4444444") {
                if self.failure != ApiError::ItemRejected {
                    return Err(self.failure);
                }
                // Exercise the real API receipt parser, not a preclassified
                // fake error: this is a successful HTTP response for a bad ID.
                json!({"not_found":{"movies":[{"ids":{"imdb":"tt4444444"}}]}})
            } else {
                json!({"added":{"movies":1}})
            }
        } else if url.path() == "/sync/playback" {
            json!([])
        } else {
            json!({"movies":[],"shows":[]})
        };
        Ok(nova_list_services::Response {
            status: 200,
            retry_after: None,
            has_more: false,
            pagination: None,
            body,
        })
    }
}

fn bulk_item_failure_regressions(bridge: &Bridge) {
    let connection = account();
    for path in ["/watchlist/items/add", "/sync/watched", "/scrobble/pause"] {
        let fake = Arc::new(ItemFailureFake {
            path,
            failure: ApiError::ItemRejected,
            requests: Mutex::new(Vec::new()),
        });
        let mut worker = Worker::new(bridge.clone(), Client::new(fake.clone()));
        worker.connections[1] = Some(connection.clone());
        worker.verified[1] = true;
        worker.full[1] = true;
        worker
            .set_record("connection:1", Some(&connection))
            .unwrap();
        worker
            .set_record(&flag_key(1, &connection, "full"), Some(&true))
            .unwrap();
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![movie("tt4444444"), movie("tt5555555")];
            state.progress.clear();
            for id in ["tt4444444", "tt5555555"] {
                state.progress.insert(
                    progress_map_key(id, id),
                    EpisodeProgress {
                        watched: path != "/scrobble/pause",
                        series_id: id.into(),
                        episode_id: id.into(),
                        position_secs: 30.0,
                        duration_secs: 120.0,
                        updated_at_secs: 1,
                        ..Default::default()
                    },
                );
            }
        }
        for _ in 0..2 {
            fake.requests.lock().unwrap().clear();
            assert_eq!(worker.merge(1, 0, None), Ok(1), "pending title at {path}");
            assert!(
                fake.requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(p, body)| { p == path && body.contains("tt5555555") }),
                "a rejected title must not starve later titles at {path}"
            );
        }
    }
    // A service-wide failure or local transport cancellation is not a bad
    // title. It must stop this pass before another mutation is attempted.
    for failure in [
        ApiError::Rejected,
        ApiError::Authentication,
        ApiError::Offline,
        ApiError::RateLimited { retry_after: 120 },
        ApiError::InvalidResponse,
    ] {
        let fake = Arc::new(ItemFailureFake {
            path: "/watchlist/items/add",
            failure,
            requests: Mutex::new(Vec::new()),
        });
        let mut worker = Worker::new(bridge.clone(), Client::new(fake.clone()));
        worker.connections[1] = Some(connection.clone());
        worker.full[1] = true;
        assert_eq!(worker.merge(1, 0, None), Err(failure));
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }
}

/// A transport that can deliver a peer change while a refresh is in flight.
#[derive(Default)]
struct TraktFake {
    requests: Mutex<Vec<(String, Option<serde_json::Value>)>>,
    responses: Mutex<std::collections::VecDeque<(u16, serde_json::Value)>>,
    replacement: Mutex<Option<Option<Connection>>>,
}
impl TraktFake {
    fn push(&self, status: u16, body: serde_json::Value) {
        self.responses.lock().unwrap().push_back((status, body));
    }
}
impl nova_list_services::Transport for TraktFake {
    fn send(
        &self,
        request: nova_list_services::Request,
    ) -> Result<nova_list_services::Response, ApiError> {
        if request.url.ends_with("/oauth/token")
            && let Some(replacement) = self.replacement.lock().unwrap().take()
        {
            let owner = nova_sync::local_store().unwrap();
            let mut store = owner.lock().unwrap();
            store.set(
                &sync::profile_domain(DOMAIN),
                "connection:2",
                replacement.map(|c| serde_json::to_string(&c).unwrap()),
                0,
                7,
            );
            store.save().unwrap();
        }
        self.requests
            .lock()
            .unwrap()
            .push((request.url.clone(), request.body.clone()));
        let (status, body) =
            if let Some(imdb) = request.url.strip_prefix("https://api.trakt.tv/movies/") {
                (
                    200,
                    json!({"title":"Canonical movie","year":2024,"ids":{"trakt":100,"imdb":imdb}}),
                )
            } else {
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(ApiError::Offline)?
            };
        Ok(nova_list_services::Response {
            status,
            retry_after: None,
            has_more: false,
            pagination: None,
            body,
        })
    }
}
fn trakt_actor_regressions(bridge: &Bridge) {
    let fake = Arc::new(TraktFake::default());
    let mut worker = Worker::new(bridge.clone(), Client::new(fake.clone()));
    let mut connection = account();
    connection.credentials.service = Service::Trakt;
    connection.credentials.client_id = "nova-test-client".into();
    connection.refresh = Some(trakt::Refresh::test_expired());
    worker.connections[2] = Some(connection.clone());
    worker.verified[2] = true;
    worker
        .set_record("connection:2", Some(&connection))
        .unwrap();
    fake.push(
        400,
        json!({"error":"invalid_grant","error_description":"session not found"}),
    );
    assert_eq!(worker.refresh_trakt(), Err(ApiError::Pending));
    worker.failed(2, ApiError::Pending);
    assert!(
        !worker.paused[2],
        "a rejected shared refresh must wait for peer rotation"
    );
    assert!(!worker.verified[2]);
    assert!(same_session(
        &worker
            .record::<Connection>("connection:2")
            .unwrap()
            .unwrap(),
        &connection
    ));
    let mut peer = connection.clone();
    peer.credentials.token = Secret::new("peer-rotated-access".into()).unwrap();
    peer.refresh = Some(trakt::Refresh::test_fresh());
    worker.set_record("connection:2", Some(&peer)).unwrap();
    fake.push(
        200,
        json!({"user":{"username":"Owner","ids":{"uuid":"owner"}}}),
    );
    worker.restore(2).unwrap();
    assert!(
        worker.verified[2],
        "new peer credentials recover without reconnecting"
    );
    assert_eq!(
        worker.connections[2].as_ref().unwrap().credentials.token,
        peer.credentials.token
    );

    // A refresh response must not resurrect a concurrent disconnect.
    worker.connections[2] = Some(connection.clone());
    worker
        .set_record("connection:2", Some(&connection))
        .unwrap();
    worker.next[2] = Instant::now();
    worker.retry[2] = 5;
    *fake.replacement.lock().unwrap() = Some(None);
    fake.push(200,json!({"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer","expires_in":604800}));
    assert_eq!(worker.refresh_trakt(), Err(ApiError::Authentication));
    assert!(
        worker
            .record::<Connection>("connection:2")
            .unwrap()
            .is_none()
    );

    // Playback events require the same explicit per-title scope as history.
    connection.refresh = None;
    worker.connections[2] = Some(connection.clone());
    worker.verified[2] = true;
    worker
        .set_record("connection:2", Some(&connection))
        .unwrap();
    worker.retry[2] = 5;
    worker.next[2] = Instant::now();
    bridge.shared.lock().unwrap().entries = vec![movie("tt1111111")];
    let generation = bridge.list_services().generation[2].load(Ordering::Acquire);
    let mut observation = trakt::Observation::test_movie("tt1111111", ScrobbleAction::Start);
    let before = fake.requests.lock().unwrap().len();
    worker.trakt_observation(&observation, generation).unwrap();
    assert_eq!(fake.requests.lock().unwrap().len(), before);
    let mut choice = selected();
    choice.include_history = true;
    worker
        .set_record(
            &selection_key(2, &connection, "movie", "tt1111111"),
            Some(&choice),
        )
        .unwrap();
    for action in [
        ScrobbleAction::Start,
        ScrobbleAction::Pause,
        ScrobbleAction::Stop,
    ] {
        observation.action = action;
        fake.push(200, json!({"action":"accepted"}));
        worker.trakt_observation(&observation, generation).unwrap();
    }
    let requests = fake.requests.lock().unwrap();
    let calls = requests[before..]
        .iter()
        .filter(|(url, _)| url.contains("/scrobble/"))
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        requests.len(),
        before + 4,
        "one canonical movie hydration serves all three events"
    );
    for (request, action) in calls.iter().zip(["start", "pause", "stop"]) {
        assert!(request.0.ends_with(&format!("/scrobble/{action}")));
    }
    drop(requests);
    choice.enabled = false;
    worker
        .set_record(
            &selection_key(2, &connection, "movie", "tt1111111"),
            Some(&choice),
        )
        .unwrap();
    worker.trakt_observation(&observation, generation).unwrap();
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 4,
        "exclusions suppress live scrobbles too"
    );
    choice.enabled = true;
    worker
        .set_record(
            &selection_key(2, &connection, "movie", "tt1111111"),
            Some(&choice),
        )
        .unwrap();
    let handle = bridge.list_services();
    observation.action = ScrobbleAction::Start;
    *handle.playback.lock().unwrap() = Some(observation.clone());
    worker.last_scrobble = None;
    worker.verified[2] = false;
    let before = fake.requests.lock().unwrap().len();
    worker.flush_trakt_observations();
    assert_eq!(fake.requests.lock().unwrap().len(), before);
    worker.verified[2] = true;
    fake.push(200, json!({"action":"start"}));
    worker.flush_trakt_observations();
    worker.flush_trakt_observations();
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 1,
        "verification replays the live Start exactly once"
    );
    observation.action = ScrobbleAction::Stop;
    *handle.playback.lock().unwrap() = None;
    handle
        .scrobbles
        .lock()
        .unwrap()
        .push_back((observation.clone(), generation));
    fake.push(503, json!({}));
    worker.flush_trakt_observations();
    assert_eq!(
        handle.scrobbles.lock().unwrap().len(),
        1,
        "terminal stop survives a transient failure"
    );
    worker.flush_trakt_observations();
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 2,
        "cooldown blocks immediate retries"
    );
    worker.next[2] = Instant::now();
    fake.push(200, json!({"action":"pause"}));
    worker.flush_trakt_observations();
    worker.flush_trakt_observations();
    assert!(handle.scrobbles.lock().unwrap().is_empty());
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 3,
        "successful terminal retry is not repeated"
    );
    assert!(
        handle.urgent.lock().unwrap().is_empty(),
        "player events never occupy the cancellation lane"
    );

    // Even if local completion precedes close, the active scrobble owns the
    // eventual watch. A concurrent bulk pass may add membership, never plays.
    worker.retry[2] = 5;
    worker.next[2] = Instant::now();
    observation.action = ScrobbleAction::Start;
    *handle.playback.lock().unwrap() = Some(observation);
    for _ in 0..5 {
        fake.push(200, json!([]));
    }
    fake.push(200, json!({"added":{"movies":1}}));
    let before = fake.requests.lock().unwrap().len();
    worker.merge(2, generation, None).unwrap();
    assert!(
        fake.requests.lock().unwrap()[before..]
            .iter()
            .all(|r| !r.0.ends_with("/sync/history"))
    );
    *handle.playback.lock().unwrap() = None;
    trakt_playback_regressions(bridge, &mut worker, &connection, &fake, generation);
    trakt_typed_playback_regressions(bridge, &mut worker, &connection, &fake, generation);
    worker
        .set_record::<Connection>("connection:2", None)
        .unwrap();
}

fn trakt_playback_regressions(
    bridge: &Bridge,
    worker: &mut Worker,
    connection: &Connection,
    fake: &Arc<TraktFake>,
    generation: u64,
) {
    let handle = bridge.list_services();
    let mut show = movie("tt0903747");
    show.type_ = "series".into();
    bridge.shared.lock().unwrap().entries.push(show);
    let video: Video =
        serde_json::from_value(json!({"id":"tt0903747:2:3","season":2,"episode":3})).unwrap();
    write_episodes_cache_for("series", "tt0903747", &[video]);
    worker
        .set_record(
            &selection_key(2, connection, "series", "tt0903747"),
            Some(&selected()),
        )
        .unwrap();
    let mut bad = trakt::Observation::test_movie("tt0903747", ScrobbleAction::Start);
    bad.media_type = "series".into();
    bad.episode_id = "tt0903747:2:3".into();
    let good = trakt::Observation::test_movie("tt1111111", ScrobbleAction::Start);
    *handle.playback.lock().unwrap() = Some(bad.clone());
    handle
        .scrobbles
        .lock()
        .unwrap()
        .extend([(bad.clone(), generation), (good.clone(), generation)]);
    fake.push(404, serde_json::Value::Null); // exact remote episode no longer exists
    fake.push(200, json!({"action":"start"}));
    let before = fake.requests.lock().unwrap().len();
    worker.flush_trakt_observations();
    assert!(handle.scrobbles.lock().unwrap().is_empty());
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 2,
        "one unresolved episode must not block a valid movie behind it"
    );
    assert!(
        fake.requests.lock().unwrap()[before + 1]
            .0
            .ends_with("/scrobble/start")
    );
    for _ in 0..3 {
        worker.flush_trakt_observations();
    }
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 2,
        "the rejected live episode must not retry every actor tick"
    );
    bad.action = ScrobbleAction::Pause;
    let mut good = good;
    good.action = ScrobbleAction::Pause;
    handle
        .scrobbles
        .lock()
        .unwrap()
        .extend([(bad, generation), (good, generation)]);
    fake.push(200, json!({"action":"pause"}));
    worker.flush_trakt_observations();
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 3,
        "later actions for a quarantined item are skipped while valid titles proceed"
    );

    // The UI retains its final native frame and end menu. EOF must stop once,
    // release the live-history suppression, and ignore later pause/close ticks.
    let target = PlaybackTarget {
        series_id: "tt1111111".into(),
        episode_id: "tt1111111".into(),
        session: 901,
        history: Some(PlaybackHistory {
            type_: "movie".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let before = fake.requests.lock().unwrap().len();
    bridge.list_services_playback(&target, 95.0, 100.0, false, false);
    fake.push(200, json!({"action":"start"}));
    worker.flush_trakt_observations();
    bridge.list_services_playback(&target, 100.0, 100.0, true, true);
    fake.push(200, json!({"action":"scrobble"}));
    worker.flush_trakt_observations();
    for _ in 0..10 {
        bridge.list_services_playback(&target, 100.0, 100.0, true, true);
        bridge.list_services_playback(&target, 100.0, 100.0, true, false);
        worker.flush_trakt_observations();
    }
    bridge.list_services_playback(&target, 100.0, 100.0, false, true); // later close
    worker.flush_trakt_observations();
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        before + 2,
        "retained EOF and close must not send Pause 100% or a second Stop"
    );
    assert!(
        !handle.has_trakt_observation("movie", "tt1111111", None),
        "the open end screen must not suppress bulk history forever"
    );
    // A real seek/replay on the same native load rearms a new viewing cycle.
    bridge.list_services_playback(&target, 25.0, 100.0, true, false);
    fake.push(200, json!({"action":"pause"}));
    worker.flush_trakt_observations();
    bridge.list_services_playback(&target, 25.0, 100.0, false, false);
    fake.push(200, json!({"action":"start"}));
    worker.flush_trakt_observations();
    bridge.list_services_playback(&target, 100.0, 100.0, true, true);
    fake.push(200, json!({"action":"scrobble"}));
    worker.flush_trakt_observations();
    let native = bridge.player.screenshot_session();
    bridge.player.close();
    assert_ne!(
        bridge.player.screenshot_session(),
        native,
        "native close invalidates telemetry ownership"
    );
    bridge.list_services_playback(&target, 100.0, 100.0, false, true);
    worker.flush_trakt_observations();
    let requests = fake.requests.lock().unwrap();
    let actions = requests[before..]
        .iter()
        .map(|(url, _)| url.rsplit('/').next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["start", "stop", "pause", "start", "stop"]);
}

fn trakt_typed_playback_regressions(
    bridge: &Bridge,
    worker: &mut Worker,
    connection: &Connection,
    fake: &Arc<TraktFake>,
    generation: u64,
) {
    let id = "catalog:42";
    for (kind, imdb) in [("movie", "tt7777777"), ("series", "tt8888888")] {
        let preview = serde_json::from_value(json!({
            "id":id, "type":kind, "name":"Collision", "imdb_id":imdb
        }))
        .unwrap();
        tracking::remember_source(&preview);
    }
    let film = LibraryEntry {
        id: id.into(),
        ..movie("tt7777777")
    };
    let show = LibraryEntry {
        type_: "series".into(),
        ..film.clone()
    };
    let video = serde_json::from_value(json!({"id":"tt8888888:2:3"})).unwrap();
    write_episodes_cache_for("series", id, &[video]);
    worker
        .set_record(&flag_key(2, connection, "full"), Some(&true))
        .unwrap();
    worker.full[2] = true;
    let mut observation = trakt::Observation::test_movie(id, ScrobbleAction::Stop);
    observation.media_type = "series".into();
    observation.episode_id = "tt8888888:2:3".into();
    observation.test_percent(100.0);
    let before = fake.requests.lock().unwrap().len();
    for entries in [vec![film.clone(), show.clone()], vec![show, film]] {
        bridge.shared.lock().unwrap().entries = entries;
        fake.push(200, json!({"season":2,"number":3,"ids":{"trakt":888}}));
        fake.push(200, json!({"action":"scrobble"}));
        assert!(worker.trakt_observation(&observation, generation).unwrap());
    }
    let requests = fake.requests.lock().unwrap();
    let calls = &requests[before..];
    assert_eq!(calls.len(), 4);
    for pair in calls.chunks_exact(2) {
        assert!(pair[0].0.ends_with("/shows/tt8888888/seasons/2/episodes/3"));
        assert!(pair[1].0.ends_with("/scrobble/stop"));
        let body = pair[1].1.as_ref().unwrap();
        assert_eq!(body["episode"]["ids"]["trakt"], 888);
        assert!(body.get("movie").is_none());
    }
    drop(requests);
    // Bulk history suppression uses the same typed identity.
    let handle = bridge.list_services();
    *handle.playback.lock().unwrap() = Some(observation);
    assert!(handle.has_trakt_observation("series", id, None));
    assert!(!handle.has_trakt_observation("movie", id, None));
    *handle.playback.lock().unwrap() = None;
}

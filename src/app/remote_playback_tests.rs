//! Coordinator behavior against a real Bridge, with one storage/backend per child.
use super::*;

struct Fixture {
    app: AppWindow,
    bridge: Bridge,
}

fn isolated(name: &str, body: impl FnOnce(Fixture)) {
    isolated_prepared(name, |_| {}, body);
}

fn isolated_with_downloads(name: &str, body: impl FnOnce(Fixture)) {
    isolated_prepared(name, seed_receiver_downloads, |fixture| {
        assert!(fixture.bridge.shared.lock().unwrap().installed.is_empty());
        body(fixture);
    });
}

fn isolated_prepared(
    name: &str,
    prepare: impl FnOnce(&std::path::Path),
    body: impl FnOnce(Fixture),
) {
    const ROOT: &str = "NOVA_REMOTE_COORDINATOR_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-remote-coordinator-{}-{}-{}",
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
        let receipt = root.join("completed").is_file();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success() && receipt,
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = PathBuf::from(root);
    i_slint_backend_testing::init_integration_test_with_mock_time();
    storage::init_at(&root);
    prepare(&root);
    playback_foreground(true);
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
    body(Fixture { app, bridge });
    std::fs::write(root.join("completed"), name).unwrap();
}

fn peer(id: &str, epoch: u64, authorized: bool) -> PeerContext {
    PeerContext::new(id.into(), 1, epoch, Arc::new(move || authorized))
}

fn receive(bridge: &Bridge, peer: PeerContext, request: Request) -> Response {
    let response = Arc::new(Mutex::new(None));
    let completed = response.clone();
    bridge.receive_playback_request(
        peer,
        request,
        Box::new(move |value| {
            *completed.lock().unwrap() = Some(value);
        }),
    );
    response
        .lock()
        .unwrap()
        .take()
        .expect("this request must complete synchronously")
}

fn retain_offer(bridge: &Bridge) -> PlaybackOffer {
    bridge.retain_remote_offer(
        "controller",
        &MediaIdentity {
            media_type: "movie".into(),
            title_id: "movie-id".into(),
            episode_id: "movie-id".into(),
            title: "Movie".into(),
            episode: String::new(),
        },
        StreamUi {
            id: "private-row".into(),
            choice_id: None,
            owner_url: String::new(),
            display: "Release https://example.invalid/private.mkv".into(),
            card: StreamCardText::default(),
            source: StreamSource::Url("https://example.invalid/private.mkv?token=secret".into()),
            addon: "Receiver addon".into(),
            download: None,
        },
        0,
        nova_providers::plugin_settings::settings_generation(),
    )
}

#[test]
fn startup_profile_selection_blocks_remote_playback_and_state() {
    isolated(
        concat!(
            module_path!(),
            "::startup_profile_selection_blocks_remote_playback_and_state"
        ),
        |Fixture { app, bridge }| {
            let offer = retain_offer(&bridge);
            app.set_player_show_title("Remembered profile title".into());
            app.set_profiles_require_selection(true);
            let snapshot = bridge.playback_snapshot();
            assert!(!snapshot.ready);
            assert!(snapshot.title.is_empty());
            for request in [
                Request::Snapshot,
                Request::Lookup(MediaIdentity {
                    media_type: "movie".into(),
                    title_id: "movie-id".into(),
                    episode_id: "movie-id".into(),
                    title: "Movie".into(),
                    episode: String::new(),
                }),
                Request::Start {
                    offer_id: offer.id,
                    expected_session: None,
                },
            ] {
                assert_eq!(
                    receive(&bridge, peer("controller", 3, true), request),
                    Response::Error(PlaybackError::Unavailable)
                );
            }
            assert!(!app.get_player_open());
            assert!(bridge.shared.lock().unwrap().playback_intent.is_none());
            profile_policy::set_entry_blocked(true);
            let response = receive(&bridge, peer("controller", 3, true), Request::Snapshot);
            assert_eq!(
                response,
                Response::Error(PlaybackError::Failed("profile_locked".into()))
            );
            assert_eq!(
                error_text(&PlaybackError::Failed("profile_locked".into())),
                "Unlock on TV."
            );
            assert!(!app.get_player_open());
            profile_policy::set_entry_blocked(false);
            app.set_profiles_require_selection(false);
            assert!(bridge.playback_snapshot().ready);
        },
    );
}

#[test]
fn loading_reserves_local_role_and_controller_role_rejects_local_start() {
    isolated(
        concat!(
            module_path!(),
            "::loading_reserves_local_role_and_controller_role_rejects_local_start"
        ),
        |Fixture { app, bridge }| {
            let pending = bridge.begin_playback_request();
            assert!(pending.is_current());
            assert!(!app.get_player_open());
            let snapshot = bridge.playback_snapshot();
            assert!(snapshot.session.is_some());
            assert_eq!(snapshot.phase, PlaybackPhase::Loading);
            assert_eq!(
                bridge.shared.lock().unwrap().remote_playback.role,
                Role::Local
            );
            assert!(snapshot.capabilities.stop);
            assert!(!snapshot.capabilities.screenshot);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 3, true),
                    Request::Command {
                        session: snapshot.session.unwrap(),
                        action: PlaybackAction::Stop,
                    }
                ),
                Response::Accepted
            );
            assert!(!pending.is_current());
            assert!(bridge.playback_snapshot().session.is_none());
            assert_eq!(
                bridge.shared.lock().unwrap().remote_playback.role,
                Role::Idle
            );

            let (sequence, history_len) = {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.target = "playing-device".into();
                (state.remote_playback.sequence, state.progress.len())
            };
            let denied = bridge.begin_playback_request();
            assert!(!denied.is_current());
            assert!(!bridge.playback_snapshot().ready);
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.remote_playback.role, Role::Remote);
            assert_eq!(state.remote_playback.sequence, sequence);
            assert!(state.playback_intent.is_none());
            assert_eq!(
                state.progress.len(),
                history_len,
                "A denied local start must not create viewing history"
            );
            drop(state);
            assert!(!app.get_player_open());
        },
    );
}

#[test]
fn authority_session_and_loading_capabilities_block_commands_before_mutation() {
    isolated(
        concat!(
            module_path!(),
            "::authority_session_and_loading_capabilities_block_commands_before_mutation"
        ),
        |Fixture { app, bridge }| {
            let pending = bridge.begin_playback_request();
            let session = bridge.playback_snapshot().session.unwrap();
            app.set_player_open(true);
            app.set_position(42.0);
            let native_capture_epoch = bridge.player.screenshot_session();
            assert_eq!(
                receive(
                    &bridge,
                    peer("revoked", 1, false),
                    Request::Command {
                        session,
                        action: PlaybackAction::Stop,
                    }
                ),
                Response::Error(PlaybackError::Unauthorized)
            );
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Command {
                        session: SessionId {
                            sequence: session.sequence + 1,
                            ..session
                        },
                        action: PlaybackAction::SeekAbsolute(99.0),
                    }
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::Screenshot,
                    }
                ),
                Response::Error(PlaybackError::Unsupported)
            );
            app.set_playback_started(true);
            app.set_screenshot_busy(true);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::Screenshot,
                    }
                ),
                Response::Error(PlaybackError::Unsupported)
            );
            assert!(pending.is_current());
            assert_eq!(bridge.playback_snapshot().session, Some(session));
            assert_eq!(app.get_position(), 42.0);
            assert_eq!(
                bridge.player.screenshot_session(),
                native_capture_epoch,
                "Rejected capture must not replace the native capture session"
            );
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .screenshot_saved
            );
        },
    );
}

#[test]
fn rejected_starts_preserve_opaque_offers_and_preview_cleanup_checks_owner_epoch() {
    isolated(
        concat!(
            module_path!(),
            "::rejected_starts_preserve_opaque_offers_and_preview_cleanup_checks_owner_epoch"
        ),
        |Fixture { app, bridge }| {
            let offer = retain_offer(&bridge);
            assert_eq!(offer.name, "Receiver addon");
            assert_eq!(offer.description, "Release");
            assert!(!offer.id.contains("private.mkv"));
            let pending = bridge.begin_playback_request();
            let session = bridge.playback_snapshot().session.unwrap();
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, false),
                    Request::Start {
                        offer_id: offer.id.clone(),
                        expected_session: Some(session),
                    }
                ),
                Response::Error(PlaybackError::Unauthorized)
            );
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, true),
                    Request::Start {
                        offer_id: offer.id.clone(),
                        expected_session: None,
                    }
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .contains_key(&offer.id)
            );
            assert!(
                pending.is_current(),
                "Stale replacement must not stop receiver playback"
            );
            bridge.shared.lock().unwrap().remote_playback.role = Role::Remote;
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, true),
                    Request::Start {
                        offer_id: offer.id.clone(),
                        expected_session: Some(session),
                    }
                ),
                Response::Error(PlaybackError::Busy)
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .contains_key(&offer.id)
            );
            bridge.shared.lock().unwrap().remote_playback.role = Role::Local;
            app.set_player_open(true);
            app.set_playback_started(true);
            bridge.player.set_title_speed(1.3);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::BeginSpeedPreview,
                    }
                ),
                Response::Accepted
            );
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, true),
                    Request::Command {
                        session: SessionId {
                            sequence: session.sequence + 1,
                            ..session
                        },
                        action: PlaybackAction::EndSpeedPreview,
                    }
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_owner
                    .as_deref(),
                Some("controller")
            );
            for denied in [
                peer("other-controller", 7, false),
                peer("controller", 6, false),
            ] {
                assert_eq!(
                    receive(&bridge, denied, Request::ReleasePreview),
                    Response::Accepted
                );
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .remote_playback
                        .preview_owner
                        .as_deref(),
                    Some("controller")
                );
            }
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, false),
                    Request::ReleasePreview
                ),
                Response::Accepted
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_owner
                    .is_none(),
                "Revoked controller cleanup must still release its own hold"
            );
            assert_eq!(
                bridge.player.effective_playback_speed(),
                1.3,
                "A transient preview cannot persist or reset the semantic rate"
            );
        },
    );
}

#[test]
fn continuous_controls_preserve_intervening_command_order() {
    isolated(
        concat!(
            module_path!(),
            "::continuous_controls_preserve_intervening_command_order"
        ),
        |Fixture { app, bridge }| {
            let session = SessionId {
                epoch: 7,
                sequence: 1,
            };
            let token = session_text(Some(session));
            app.set_remote_controls_open(true);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.target = "selected-receiver".into();
                state.remote_playback.snapshot = Some(PlaybackSnapshot {
                    session: Some(session),
                    ready: true,
                    phase: PlaybackPhase::Playing,
                    capabilities: PlaybackCapabilities {
                        seek: true,
                        speed: true,
                        volume: true,
                        stop: true,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            for (inputs, expected) in [
                (
                    vec![("seek", 10.0), ("seek_relative", 10.0), ("seek", 50.0)],
                    vec![
                        PlaybackAction::SeekAbsolute(10.0),
                        PlaybackAction::SeekRelative(10.0),
                        PlaybackAction::SeekAbsolute(50.0),
                    ],
                ),
                (
                    vec![("speed", 1.5), ("reset_speed", 0.0), ("speed", 2.0)],
                    vec![
                        PlaybackAction::SetSpeed(1.5),
                        PlaybackAction::ResetSpeed,
                        PlaybackAction::SetSpeed(2.0),
                    ],
                ),
                (
                    vec![("volume", 20.0), ("volume_relative", 5.0), ("volume", 50.0)],
                    vec![
                        PlaybackAction::SetVolume(20.0),
                        PlaybackAction::VolumeStep(5.0),
                        PlaybackAction::SetVolume(50.0),
                    ],
                ),
                (
                    vec![("seek", 10.0), ("seek", 20.0), ("seek", 50.0)],
                    vec![PlaybackAction::SeekAbsolute(50.0)],
                ),
            ] {
                for (name, value) in inputs {
                    bridge.remote_ui_action(name, value, &token);
                }
                let queue = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .queued
                    .drain(..)
                    .collect::<Vec<_>>();
                assert_eq!(
                    queue,
                    expected
                        .into_iter()
                        .map(|action| Request::Command { session, action })
                        .collect::<Vec<_>>()
                );
            }
        },
    );
}

#[test]
fn continuous_controls_coalesce_and_unsent_preview_cannot_survive_release() {
    isolated(
        concat!(
            module_path!(),
            "::continuous_controls_coalesce_and_unsent_preview_cannot_survive_release"
        ),
        |Fixture { app, bridge }| {
            let session = SessionId {
                epoch: 7,
                sequence: 1,
            };
            let token = session_text(Some(session));
            app.set_remote_controls_open(true);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.target = "selected-receiver".into();
                state.remote_playback.snapshot = Some(PlaybackSnapshot {
                    session: Some(session),
                    ready: true,
                    phase: PlaybackPhase::Playing,
                    capabilities: PlaybackCapabilities {
                        seek: true,
                        speed: true,
                        volume: true,
                        stop: true,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            for volume in 0..500 {
                bridge.remote_ui_action("volume", (volume % 101) as f32, &token);
            }
            assert_eq!(
                bridge.shared.lock().unwrap().remote_playback.queued.len(),
                1
            );
            assert!(
                matches!(
                    bridge.shared.lock().unwrap().remote_playback.queued.back(),
                    Some(Request::Command {
                        action: PlaybackAction::SetVolume(95.0),
                        ..
                    })
                ),
                "the last valid slider value must survive coalescing"
            );
            bridge.remote_ui_action("speed_preview", 2.0, &token);
            bridge.remote_ui_action("speed_preview_end", 0.0, &token);
            {
                let state = bridge.shared.lock().unwrap();
                let queue = &state.remote_playback.queued;
                assert!(!queue.iter().any(|r| matches!(
                    r,
                    Request::Command {
                        action: PlaybackAction::BeginSpeedPreview,
                        ..
                    }
                )));
                assert!(matches!(
                    queue.front(),
                    Some(Request::Command {
                        action: PlaybackAction::EndSpeedPreview,
                        ..
                    })
                ));
            }
            for _ in 0..100 {
                bridge.remote_ui_action("seek_relative", 10.0, &token);
            }
            assert!(bridge.shared.lock().unwrap().remote_playback.queued.len() <= 32);
            bridge.remote_ui_action("stop", 0.0, &token);
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.remote_playback.queued.len(), 1);
            assert!(matches!(
                state.remote_playback.queued.front(),
                Some(Request::Command {
                    action: PlaybackAction::Stop,
                    ..
                })
            ));
        },
    );
}

#[test]
fn reopened_controller_reclaims_preview_before_old_cleanup() {
    isolated(
        concat!(
            module_path!(),
            "::reopened_controller_reclaims_preview_before_old_cleanup"
        ),
        |Fixture { app, bridge }| {
            bridge.shared.lock().unwrap().remote_playback.begin();
            let session = bridge
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .session
                .unwrap();
            app.set_player_open(true);
            app.set_playback_started(true);
            bridge.player.set_title_speed(1.3);
            let begin = |connection| peer("controller", 7, true).with_connection_id(connection);
            for connection in [10, 20] {
                assert_eq!(
                    receive(
                        &bridge,
                        begin(connection),
                        Request::Command {
                            session,
                            action: PlaybackAction::BeginSpeedPreview
                        }
                    ),
                    Response::Accepted
                );
            }
            // A held preview must not change the receiver's persisted base rate.
            assert_eq!(bridge.player.effective_playback_speed(), 1.3);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, false).with_connection_id(10),
                    Request::ReleasePreview
                ),
                Response::Accepted
            );
            assert_eq!(
                receive(
                    &bridge,
                    begin(10),
                    Request::Command {
                        session,
                        action: PlaybackAction::EndSpeedPreview
                    }
                ),
                Response::Accepted
            );
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_connection,
                20
            );
            assert_eq!(bridge.player.effective_playback_speed(), 1.3);
            assert_eq!(
                receive(
                    &bridge,
                    peer("different-controller", 7, true).with_connection_id(30),
                    Request::Command {
                        session,
                        action: PlaybackAction::BeginSpeedPreview
                    }
                ),
                // A supported action rejected by the dispatcher returns a
                // sanitized execution failure, never its local error details.
                Response::Error(PlaybackError::Failed(String::new()))
            );
            {
                let state = bridge.shared.lock().unwrap();
                assert_eq!(
                    state.remote_playback.preview_owner.as_deref(),
                    Some("controller")
                );
                assert_eq!(state.remote_playback.preview_connection, 20);
                assert_eq!(bridge.player.effective_playback_speed(), 1.3);
            }
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 7, false).with_connection_id(20),
                    Request::ReleasePreview
                ),
                Response::Accepted
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_owner
                    .is_none()
            );
            assert_eq!(bridge.player.effective_playback_speed(), 1.3);
        },
    );
}

#[test]
fn receiver_resolver_failure_keeps_failed_session_until_controller_stop() {
    isolated(
        concat!(
            module_path!(),
            "::receiver_resolver_failure_keeps_failed_session_until_controller_stop"
        ),
        |Fixture { app, bridge }| {
            bridge.shared.lock().unwrap().remote_playback.start_context = Some(MediaIdentity {
                media_type: "movie".into(),
                title_id: "movie-id".into(),
                episode_id: "movie-id".into(),
                title: "Movie".into(),
                episode: String::new(),
            });
            let request = bridge.begin_playback_request();
            let session = bridge.playback_snapshot().session.unwrap();
            assert!(bridge.player.wait_for_source("Resolving stream…"));
            app.set_streams_hint("Host detail stays independent".into());
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_mut()
                .unwrap()
                .in_app = true;
            bridge.provider_resolution_failed(&request, "Could not resolve this stream.".into());
            assert!(
                !request.is_current(),
                "Late worker results lose authority after failure"
            );
            assert!(app.get_player_open());
            assert!(bridge.player.playback_failed());
            assert_eq!(app.get_streams_hint(), "Host detail stays independent");
            bridge.note_player_progress_from_ui();
            let snapshot = bridge.playback_snapshot();
            assert_eq!(snapshot.session, Some(session));
            assert_eq!(snapshot.phase, PlaybackPhase::Failed);
            assert!(snapshot.capabilities.stop);
            assert!(!snapshot.capabilities.screenshot);
            {
                let state = bridge.shared.lock().unwrap();
                let intent = state.playback_intent.as_ref().unwrap();
                assert!(!intent.pending);
                assert!(intent.in_app);
            }
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 5, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::Stop,
                    }
                ),
                Response::Accepted
            );
            assert!(!app.get_player_open());
            assert!(!bridge.player.playback_failed());
            assert!(bridge.playback_snapshot().session.is_none());
            assert!(bridge.shared.lock().unwrap().playback_intent.is_none());

            // The ordinary local resolver path retains its existing cleanup.
            let local = bridge.begin_playback_request();
            assert!(bridge.player.wait_for_source("Resolving stream…"));
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_mut()
                .unwrap()
                .in_app = true;
            bridge.provider_resolution_failed(&local, "Could not resolve this stream.".into());
            assert!(!local.is_current());
            assert!(
                app.get_player_open(),
                "Local failure remains readable until Close"
            );
            assert!(bridge.shared.lock().unwrap().playback_intent.is_none());
            assert!(bridge.playback_snapshot().session.is_none());
            bridge.player.close();
            assert!(!bridge.player.playback_failed());
        },
    );
}

fn alias_video(id: &str, episode: u32, aliases: &[&str]) -> Video {
    Video {
        id: id.into(),
        season: Some(1),
        episode: Some(episode),
        extra: serde_json::from_value(serde_json::json!({
            "novaStreamIds": aliases,
            "novaMetadataRevision": nova_providers::metadata_revision(),
        }))
        .unwrap(),
        ..Default::default()
    }
}

#[test]
fn receiver_episode_resolution_prefers_native_ids_and_rejects_ambiguous_or_stale_aliases() {
    let mut videos = vec![
        alias_video("tt100:1:1", 1, &["tt100:1:2", "kitsu:100:1"]),
        alias_video("tt100:1:2", 2, &["kitsu:100:1"]),
    ];
    assert_eq!(
        remote_episode(&videos, "tt100:1:2").unwrap().id,
        "tt100:1:2"
    );
    assert!(remote_episode(&videos, "kitsu:100:1").is_none());
    videos.reverse();
    assert_eq!(
        remote_episode(&videos, "tt100:1:2").unwrap().id,
        "tt100:1:2"
    );
    assert!(remote_episode(&videos, "kitsu:100:1").is_none());
    videos.pop();
    assert_eq!(
        remote_episode(&videos, "kitsu:100:1").unwrap().id,
        "tt100:1:2"
    );
    videos[0].extra.insert(
        "novaMetadataRevision".into(),
        serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
    );
    assert!(remote_episode(&videos, "kitsu:100:1").is_none());
    assert!(remote_episode(&videos, "tt100:1:2").is_some());
    assert!(remote_episode(&videos, "unknown").is_none());
}

fn seed_receiver_downloads(root: &std::path::Path) {
    let root = root.join("downloads");
    std::fs::create_dir_all(&root).unwrap();
    let jobs = (1..=2)
        .map(|episode| {
            let path = root.join(format!("episode-{episode}.mkv"));
            std::fs::write(&path, b"receiver artifact").unwrap();
            crate::download::DownloadJob {
                id: format!("download-{episode}"),
                media_type: "series".into(),
                media_id: "tt100".into(),
                request_id: format!("tt100:1:{episode}"),
                title: format!("Episode {episode}"),
                phase: crate::download::DownloadPhase::Completed,
                total_bytes: (episode == 1).then_some(1_100_000_000),
                artifact_path: Some(path),
                ..Default::default()
            }
        })
        .collect();
    storage::set_str(
        "downloads:v1",
        &crate::download::DownloadManifest::new(jobs)
            .to_json()
            .unwrap(),
    );
    // Seed durable state before constructing the one device-owned coordinator.
    // Reopening the same root intentionally returns its existing live owner.
}

fn alias_media(episode: &str) -> MediaIdentity {
    MediaIdentity {
        media_type: "series".into(),
        title_id: "tt100".into(),
        episode_id: episode.into(),
        title: "Receiver show".into(),
        ..Default::default()
    }
}

fn assert_download_offer(bridge: &Bridge, response: Response, episode: u32) {
    let Response::Offers(offers) = response else {
        panic!("expected receiver downloads, got {response:?}");
    };
    assert_eq!(offers.len(), 1);
    let state = bridge.shared.lock().unwrap();
    let offer = &state.remote_playback.offers[&offers[0].id];
    assert_eq!(offer.media.episode_id, format!("tt100:1:{episode}"));
    assert!(
        matches!(&offer.stream.source, StreamSource::Downloaded { job_id, .. } if job_id == &format!("download-{episode}"))
    );
}

#[test]
fn cached_remote_aliases_find_receiver_downloads_without_metadata_addons() {
    isolated_with_downloads(
        concat!(
            module_path!(),
            "::cached_remote_aliases_find_receiver_downloads_without_metadata_addons"
        ),
        |Fixture { app: _app, bridge }| {
            let mut videos = vec![alias_video("tt100:1:1", 1, &["kitsu:100:1"])];
            write_episodes_cache_for("series", "tt100", &videos);
            assert_download_offer(
                &bridge,
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("kitsu:100:1")),
                ),
                1,
            );
            videos.push(alias_video("tt100:1:2", 2, &["kitsu:100:1"]));
            write_episodes_cache_for("series", "tt100", &videos);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("kitsu:100:1"))
                ),
                Response::Error(PlaybackError::NotFound),
            );
            videos[0]
                .extra
                .insert("novaStreamIds".into(), serde_json::json!(["tt100:1:2"]));
            write_episodes_cache_for("series", "tt100", &videos);
            assert_download_offer(
                &bridge,
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("tt100:1:2")),
                ),
                2,
            );
            videos.truncate(1);
            videos[0].extra.insert(
                "novaMetadataRevision".into(),
                serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
            );
            write_episodes_cache_for("series", "tt100", &videos);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("tt100:1:2"))
                ),
                Response::Error(PlaybackError::NotFound),
            );
        },
    );
}

#[test]
fn live_remote_metadata_resolves_after_reconciliation_without_first_alias_wins() {
    isolated_with_downloads(
        concat!(
            module_path!(),
            "::live_remote_metadata_resolves_after_reconciliation_without_first_alias_wins"
        ),
        |Fixture { app: _app, bridge }| {
            for (requested, aliases, expected) in [
                ("tt100:1:2", vec!["tt100:1:2"], Some(2)),
                ("kitsu:100:1", vec!["kitsu:100:1"], None),
            ] {
                write_episodes_cache_for("series", "tt100", &[]);
                let mut item: MetaItem = serde_json::from_value(serde_json::json!({
                    "id": "tt100", "type": "series", "name": "Receiver show"
                }))
                .unwrap();
                item.videos = vec![
                    alias_video("tt100:1:1", 1, &aliases),
                    alias_video("tt100:1:2", 2, &["kitsu:100:1"]),
                ];
                let response = Arc::new(Mutex::new(None));
                let completed = response.clone();
                bridge.apply_remote_metadata(
                    peer("controller", 1, true),
                    alias_media(requested),
                    VecDeque::new(),
                    Some(item),
                    Box::new(move |value| *completed.lock().unwrap() = Some(value)),
                );
                let response = response.lock().unwrap().take().unwrap();
                if let Some(episode) = expected {
                    assert_download_offer(&bridge, response, episode);
                } else {
                    assert_eq!(response, Response::Error(PlaybackError::NotFound));
                }
            }
        },
    );
}

#[test]
fn receiver_sizes_survive_offers_and_controller_stream_rows() {
    isolated_with_downloads(
        concat!(
            module_path!(),
            "::receiver_sizes_survive_offers_and_controller_stream_rows"
        ),
        |Fixture { app, bridge }| {
            let streams = Addon::parse_streams(
                &serde_json::to_vec(&serde_json::json!({"streams": [
                    {"name":"Addon", "title":"Hinted release\n💾 2.0 GB", "url":"https://example.invalid/hinted.mkv", "behaviorHints":{"videoSize":1100000000u64}},
                    {"name":"Addon", "title":"Advertised release\n💾 3.5 GB", "url":"https://example.invalid/advertised.mkv"},
                    {"name":"Addon", "title":"Unknown release", "url":"https://example.invalid/unknown.mkv"}
                ]})).unwrap(),
            ).unwrap();
            let rows = streams
                .into_iter()
                .map(|stream| {
                    (
                        StreamUi {
                            card: stream_card_text(&stream),
                            display: stream_display(&stream),
                            id: String::new(),
                            choice_id: None,
                            owner_url: String::new(),
                            source: StreamSource::Unsupported,
                            addon: "Receiver addon".into(),
                            download: None,
                        },
                        0,
                    )
                })
                .collect();
            let response = Arc::new(Mutex::new(None));
            let completed = response.clone();
            bridge.finish_remote_lookup(
                peer("controller", 1, true),
                alias_media("tt100:1:1"),
                rows,
                nova_providers::plugin_settings::settings_generation(),
                Box::new(move |value| *completed.lock().unwrap() = Some(value)),
            );
            let Response::Offers(offers) = response.lock().unwrap().take().unwrap() else {
                panic!("expected receiver offers");
            };
            assert_eq!(
                offers
                    .iter()
                    .map(|offer| offer.size.as_str())
                    .collect::<Vec<_>>(),
                ["1.1 GB", "1.1 GB", "3.5 GB", ""],
                "download totals, Stremio hints, advertised sizes and unknowns must retain their receiver display values"
            );
            bridge.shared.lock().unwrap().remote_playback.remote_offers = offers;
            bridge.render_remote_streams();
            assert_eq!(
                app.get_streams()
                    .iter()
                    .map(|row| row.size.to_string())
                    .collect::<Vec<_>>(),
                ["1.1 GB", "1.1 GB", "3.5 GB", ""]
            );

            let mut stream = StreamUi {
                id: String::new(),
                choice_id: None,
                owner_url: String::new(),
                display: "Release".into(),
                card: StreamCardText::default(),
                source: StreamSource::Unsupported,
                addon: "Receiver addon".into(),
                download: None,
            };
            stream.card.size = "é".repeat(30);
            let bounded = bridge.retain_remote_offer(
                "controller",
                &alias_media("tt100:1:1"),
                stream.clone(),
                0,
                nova_providers::plugin_settings::settings_generation(),
            );
            assert_eq!(bounded.size.len(), 32, "size uses a UTF-8-safe byte bound");
            stream.card.size = "https://example.invalid/private-size?token=secret".into();
            assert!(
                bridge
                    .retain_remote_offer(
                        "controller",
                        &alias_media("tt100:1:1"),
                        stream,
                        0,
                        nova_providers::plugin_settings::settings_generation()
                    )
                    .size
                    .is_empty()
            );
        },
    );
}

#[test]
fn settings_change_between_addon_replies_invalidates_the_entire_offer_batch() {
    isolated(
        concat!(
            module_path!(),
            "::settings_change_between_addon_replies_invalidates_the_entire_offer_batch"
        ),
        |Fixture { app: _app, bridge }| {
            use nova_providers::plugin_settings as settings;
            settings::configure_plugin_settings(Arc::new(plugin_settings::Backend::new())).unwrap();
            let generation = settings::settings_generation();
            let response = Arc::new(Mutex::new(None));
            let completed = response.clone();
            let mut batch = RemoteLookupBatch {
                remaining: 2,
                rows: vec![None; 2],
                done: Some(Box::new(move |value| {
                    *completed.lock().unwrap() = Some(value)
                })),
            };
            let old_source = StreamUi {
                id: String::new(),
                choice_id: None,
                owner_url: String::new(),
                display: "Account stream".into(),
                card: StreamCardText::default(),
                source: StreamSource::UrlWithOptions {
                    url: "https://example.invalid/old-account.mkv".into(),
                    headers: vec![("Authorization".into(), "Bearer old-account".into())],
                    subtitles: vec![],
                },
                addon: "Fast addon".into(),
                download: None,
            };
            assert!(batch.push(0, vec![(old_source, 1)]).is_none());
            settings::set_setting("test", "account", serde_json::json!("replacement")).unwrap();
            assert_ne!(generation, settings::settings_generation());
            // The slow reply correctly contributes no rows after invalidation;
            // that must not make the already-buffered fast reply valid again.
            let (rows, done) = batch.push(1, Vec::new()).unwrap();
            bridge.finish_remote_lookup(
                peer("controller", 1, true),
                alias_media("tt100:1:1"),
                rows,
                generation,
                done,
            );
            assert_eq!(
                response.lock().unwrap().take(),
                Some(Response::Error(PlaybackError::NotFound))
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .is_empty()
            );
            assert!(
                batch.push(1, Vec::new()).is_none(),
                "completion remains exactly once"
            );
        },
    );
}

#[test]
fn selector_lookup_cache_survives_target_toggle_but_not_expiry_context_or_close() {
    isolated(
        concat!(
            module_path!(),
            "::selector_lookup_cache_survives_target_toggle_but_not_expiry_context_or_close"
        ),
        |Fixture { app, bridge }| {
            app.set_stream_selector_open(true);
            let media = alias_media("tt100:1:1");
            let offer = PlaybackOffer {
                id: "receiver-opaque-offer".into(),
                name: "Receiver addon".into(),
                description: "Receiver release".into(),
                size: "1.1 GB".into(),
            };
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.target = "receiver".into();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.selector_lookups.insert(
                    "receiver".into(),
                    CachedRemoteLookup {
                        media: media.clone(),
                        offers: vec![offer],
                        started: Instant::now(),
                    },
                );
            }
            assert!(bridge.restore_selector_lookup("receiver", &media));
            assert_eq!(app.get_streams().row_count(), 1);
            let generation = bridge.shared.lock().unwrap().remote_playback.generation;
            // The same detach path used by This device retires the connection
            // and clears displayed rows, but preserves selector-owned offers.
            bridge.detach_remote_playback(true);
            assert!(bridge.shared.lock().unwrap().remote_playback.generation > generation);
            assert_eq!(app.get_streams().row_count(), 0);
            assert!(
                bridge.restore_selector_lookup("receiver", &media),
                "returning to the receiver needs no lookup"
            );
            assert_eq!(
                app.get_streams().row_data(0).unwrap().id,
                "receiver-opaque-offer"
            );
            assert_eq!(app.get_streams().row_data(0).unwrap().size, "1.1 GB");
            assert!(!bridge.restore_selector_lookup("other-receiver", &media));
            assert!(!bridge.restore_selector_lookup("receiver", &alias_media("tt100:1:2")));
            {
                let mut state = bridge.shared.lock().unwrap();
                let cached = state
                    .remote_playback
                    .selector_lookups
                    .get_mut("receiver")
                    .unwrap();
                cached.started = Instant::now() - Duration::from_secs(300);
            }
            assert!(!bridge.restore_selector_lookup("receiver", &media));
            {
                let mut state = bridge.shared.lock().unwrap();
                let cached = state
                    .remote_playback
                    .selector_lookups
                    .get_mut("receiver")
                    .unwrap();
                cached.started = Instant::now();
                cached.offers.clear();
            }
            assert!(
                bridge.restore_selector_lookup("receiver", &media),
                "successful empty results are reusable too"
            );
            assert_eq!(app.get_streams().row_count(), 0);
            app.set_stream_selector_open(false);
            assert!(!bridge.restore_selector_lookup("receiver", &media));
            bridge.remote_disconnect();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .selector_lookups
                    .is_empty()
            );
            app.set_show_home(false);
            bridge.remote_tick();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .selector_lookups
                    .is_empty()
            );
        },
    );
}

#[test]
fn language_changes_refresh_remote_statuses_without_reconnecting() {
    isolated(
        concat!(
            module_path!(),
            "::language_changes_refresh_remote_statuses_without_reconnecting"
        ),
        |Fixture { app, bridge }| {
            use nova_config::Language;
            slint::select_bundled_translation("hr").unwrap();
            app.set_remote_devices(
                Rc::new(VecModel::from(vec![
                    crate::RemoteDeviceRow {
                        id: "receiver".into(),
                        name: "Living room".into(),
                        ready: true,
                        status: "Available".into(),
                        ..Default::default()
                    },
                    crate::RemoteDeviceRow {
                        id: "offline".into(),
                        name: "Bedroom".into(),
                        status: "Open Nova to connect".into(),
                        ..Default::default()
                    },
                ]))
                .into(),
            );
            let snapshot = PlaybackSnapshot {
                ready: true,
                session: Some(SessionId {
                    epoch: 1,
                    sequence: 2,
                }),
                phase: PlaybackPhase::Paused,
                paused: true,
                resume_prompt: true,
                ..Default::default()
            };
            let settings = |language| CacheSettings {
                language,
                ..CacheSettings::default()
            };
            bridge.apply_language(&settings(Language::English));
            bridge.apply_remote_snapshot(snapshot);
            app.set_remote_error("preserved connection error".into());
            assert_eq!(app.get_remote_phase(), "Paused");
            let session = app.get_remote_session();
            bridge.apply_language(&settings(Language::Croatian));
            assert_eq!(app.get_remote_phase(), "Pauzirano");
            assert_eq!(app.get_remote_prompt(), "Nastaviti reprodukciju?");
            assert_eq!(
                app.get_remote_devices().row_data(0).unwrap().status,
                "Dostupno"
            );
            assert_eq!(
                app.get_remote_devices().row_data(1).unwrap().status,
                "Otvorite Novu za povezivanje"
            );
            assert_eq!(app.get_remote_session(), session);
            assert!(app.get_remote_connected());
            assert_eq!(app.get_remote_error(), "preserved connection error");
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .queued
                    .is_empty()
            );
            bridge.apply_language(&settings(Language::English));
            assert_eq!(app.get_remote_phase(), "Paused");
            assert_eq!(app.get_remote_prompt(), "Resume playback?");
            assert_eq!(
                app.get_remote_devices().row_data(0).unwrap().status,
                "Available"
            );
        },
    );
}

#[test]
fn remote_opt_out_revokes_offers_and_preserves_local_playback() {
    isolated(
        concat!(
            module_path!(),
            "::remote_opt_out_revokes_offers_and_preserves_local_playback"
        ),
        |Fixture { app, bridge }| {
            let pending = bridge.begin_playback_request();
            let session = bridge.playback_snapshot().session.unwrap();
            app.set_player_open(true);
            app.set_player_show_title("Private title".into());
            app.set_position(42.0);
            let offer = retain_offer(&bridge);
            app.set_playback_started(true);
            assert!(
                bridge
                    .prepare_speed_preview(
                        &PlaybackAction::BeginSpeedPreview,
                        Some(&peer("controller", 1, true))
                    )
                    .unwrap()
            );
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .remote_control_enabled = false;
            bridge.remote_control_policy_changed(false);
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .is_empty()
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_owner
                    .is_none()
            );
            assert!(
                bridge
                    .prepare_speed_preview(&PlaybackAction::BeginSpeedPreview, None)
                    .unwrap()
            );
            bridge.remote_control_policy_changed(false);
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .preview_owner
                    .as_deref(),
                Some("local")
            );

            let snapshot = bridge.playback_snapshot();
            assert!(!snapshot.ready);
            assert!(snapshot.session.is_none());
            assert!(snapshot.title.is_empty());
            for request in [
                Request::Snapshot,
                Request::Start {
                    offer_id: offer.id.clone(),
                    expected_session: Some(session),
                },
                Request::Command {
                    session,
                    action: PlaybackAction::Stop,
                },
            ] {
                assert_eq!(
                    receive(&bridge, peer("controller", 1, true), request),
                    Response::Error(PlaybackError::Unauthorized)
                );
            }
            // A lookup accepted before opting out must not publish fresh offers.
            let result = Arc::new(Mutex::new(None));
            let captured = result.clone();
            bridge.finish_remote_lookup(
                peer("controller", 1, true),
                MediaIdentity::default(),
                vec![],
                nova_providers::plugin_settings::settings_generation(),
                Box::new(move |r| *captured.lock().unwrap() = Some(r)),
            );
            assert_eq!(
                result.lock().unwrap().take(),
                Some(Response::Error(PlaybackError::Unauthorized))
            );
            assert!(app.get_player_open());
            assert_eq!(app.get_position(), 42.0);
            assert!(pending.is_current());
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .remote_control_enabled = true;
            bridge.remote_control_policy_changed(true);
            assert_eq!(bridge.playback_snapshot().session, Some(session));
            assert!(bridge.playback_snapshot().ready);
            assert_eq!(
                bridge.start_remote_offer(&peer("controller", 1, true), &offer.id, Some(session)),
                Response::Error(PlaybackError::NotFound)
            );
        },
    );
}

#[test]
fn background_ticks_do_not_start_device_discovery() {
    isolated(
        concat!(
            module_path!(),
            "::background_ticks_do_not_start_device_discovery"
        ),
        |Fixture { app, bridge }| {
            app.set_show_home(true);
            app.set_remote_has_ready_devices(true);
            playback_foreground(false);
            bridge.remote_tick();
            bridge.refresh_playback_devices();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .discovery_at
                    .is_none()
            );
            assert!(
                app.get_remote_has_ready_devices(),
                "background discovery must not even rewrite the model"
            );
            playback_foreground(true);
        },
    );
}

#[test]
fn semantic_commands_require_current_membership_and_retained_exit_has_no_authority() {
    isolated(
        concat!(
            module_path!(),
            "::semantic_commands_require_current_membership_and_retained_exit_has_no_authority"
        ),
        |Fixture { app, bridge }| {
            bridge.wire_remote_playback();
            bridge.shared.lock().unwrap().remote_playback.begin();
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(100.0);
            app.set_player_chapters(
                Rc::new(VecModel::from(vec![crate::ChapterRow {
                    title: "Scene".into(),
                    position: 30.0,
                }]))
                .into(),
            );
            let snapshot = bridge.playback_snapshot();
            assert!(snapshot.valid_payload());
            let session = snapshot.session.unwrap();
            let chapter = snapshot.chapters[0].id.clone();
            let request = |session, id| Request::Command {
                session,
                action: PlaybackAction::SeekChapter { id },
            };
            assert_eq!(
                receive(
                    &bridge,
                    peer("revoked", 1, false),
                    request(session, chapter.clone())
                ),
                Response::Error(PlaybackError::Unauthorized)
            );
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    request(
                        SessionId {
                            sequence: session.sequence + 1,
                            ..session
                        },
                        chapter.clone()
                    )
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            app.set_player_chapters(Default::default());
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    request(session, chapter)
                ),
                Response::Error(PlaybackError::Unsupported)
            );
            // Storage/identity failure must not be acknowledged as a saved draft.
            assert!(
                bridge
                    .dispatch_playback_action(PlaybackAction::AddBookmark {
                        note: "Keep my draft".into()
                    })
                    .is_err()
            );
            app.set_player_open(false);
            app.set_remote_title("Retained scene".into());
            app.set_remote_session("old-session".into());
            app.set_remote_controls_open(true);
            app.set_remote_controls_visual_retained(true);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.target = "paired-device".into();
                state.remote_playback.snapshot = Some(snapshot);
            }
            bridge.remote_disconnect();
            assert!(!app.get_remote_connected());
            assert!(!app.get_remote_controls_open());
            assert_eq!(app.get_remote_title(), "Retained scene");
            assert_eq!(app.get_remote_session(), "old-session");
            bridge.queue_remote_action(session, PlaybackAction::TogglePause);
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .queued
                    .is_empty()
            );
            // An exit completion cannot erase a reopened controller's fresh data.
            app.set_remote_controls_open(true);
            app.invoke_remote_controls_visual_closed();
            assert_eq!(app.get_remote_title(), "Retained scene");
            app.set_remote_controls_open(false);
            app.invoke_remote_controls_visual_closed();
            assert!(app.get_remote_title().is_empty());
            assert!(app.get_remote_session().is_empty());
        },
    );
}

#[test]
fn current_offers_are_source_bound_and_capture_recovery_at_start_acceptance() {
    isolated(
        concat!(
            module_path!(),
            "::current_offers_are_source_bound_and_capture_recovery_at_start_acceptance"
        ),
        |Fixture { app, bridge }| {
            use nova_config::playback_preferences::{
                SessionPreferences, TitlePreferences, TrackChoice, TrackDescriptor, TrackKind,
            };
            let media = MediaIdentity {
                media_type: "series".into(),
                title_id: "movie-id".into(),
                episode_id: "movie-id".into(),
                title: "Receiver series".into(),
                episode: "Episode one".into(),
            };
            let prefs = SessionPreferences::from_settings(
                &CacheSettings::default(),
                TitlePreferences {
                    audio: Some(TrackChoice::Track(TrackDescriptor {
                        language: Some("ja".into()),
                        kind: Some(TrackKind::Normal),
                        title: None,
                    })),
                    subtitles: Some(TrackChoice::Off),
                    speed: Some(1.1),
                    ..Default::default()
                },
            );
            // The testing backend cannot install a video renderer. This still
            // initializes the native preference/source ownership exactly as a
            // queued load does, without networking or launching an external app.
            let _ = bridge.player.play_with_preferences(
                "https://example.invalid/current",
                0.0,
                &[],
                &[],
                prefs.clone(),
            );
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.start_context = Some(media.clone());
                state.cache_settings.episode_start_behavior = EpisodeStartBehavior::Ask;
                state.progress.insert(
                    progress_map_key(&media.title_id, &media.episode_id),
                    EpisodeProgress {
                        series_id: media.title_id.clone(),
                        episode_id: media.episode_id.clone(),
                        position_secs: 300.0,
                        duration_secs: 1200.0,
                        ..Default::default()
                    },
                );
            }
            let initial = bridge.begin_playback_request();
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(1200.0);
            app.set_position(50.0);
            let session = bridge.playback_snapshot().session.unwrap();
            let source = bridge.player.screenshot_session();
            let owner = peer("controller", 7, true);
            let make_offer = || {
                let offer = retain_offer(&bridge);
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .get_mut(&offer.id)
                    .unwrap()
                    .media = media.clone();
                offer
            };
            let offered = make_offer();
            assert_eq!(
                bridge.finish_current_remote_lookup(
                    &owner,
                    session,
                    source,
                    Response::Offers(vec![offered.clone()])
                ),
                Response::Offers(vec![offered.clone()])
            );
            assert!(initial.is_current());
            assert!(app.get_player_open());
            assert_eq!(app.get_position(), 50.0);
            // Canceling the chooser is a presentation action, with no Stop or
            // source mutation. The next offer can be accepted at a later playhead.
            assert_eq!(bridge.player.screenshot_session(), source);
            assert_eq!(bridge.playback_snapshot().session, Some(session));
            let unrelated = make_offer();
            let old_session = SessionId {
                sequence: session.sequence.wrapping_add(1),
                ..session
            };
            assert_eq!(
                bridge.finish_current_remote_lookup(
                    &owner,
                    old_session,
                    source,
                    Response::Offers(vec![unrelated.clone()])
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .contains_key(&unrelated.id)
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .contains_key(&offered.id)
            );
            let late_source = make_offer();
            let _ = bridge.player.play_with_preferences(
                "https://example.invalid/recovered",
                50.0,
                &[],
                &[],
                prefs,
            );
            let recovered_source = bridge.player.screenshot_session();
            assert_ne!(recovered_source, source);
            app.set_playback_started(true);
            assert_eq!(
                bridge.finish_current_remote_lookup(
                    &owner,
                    session,
                    source,
                    Response::Offers(vec![late_source.clone()])
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert!(
                !bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .offers
                    .contains_key(&late_source.id)
            );
            // Acceptance also rechecks source ownership, even if the logical
            // session has not yet changed (for example during native recovery).
            assert_eq!(
                bridge.start_remote_offer(&owner, &offered.id, Some(session)),
                Response::Error(PlaybackError::StaleSession)
            );
            assert!(initial.is_current());
            assert!(app.get_player_open());
            bridge
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .offers
                .get_mut(&offered.id)
                .unwrap()
                .retry_session = Some((session, recovered_source));
            assert_eq!(
                bridge.start_remote_offer(&owner, &offered.id, Some(old_session)),
                Response::Error(PlaybackError::StaleSession)
            );
            // Both user intent and playback advance while the chooser is open.
            // The receiver must snapshot these latest values when Start arrives.
            bridge.player.set_title_speed(1.35);
            bridge.player.set_title_subtitle_style_value(0, 2.25);
            let latest = bridge.player.retry_preferences();
            app.set_position(97.25);
            // Exercise the same acceptance phase used by Start, before this
            // headless backend's native launch failure clears receiver state.
            assert!(matches!(
                bridge.accept_remote_offer(
                    &peer("another-controller", 7, true),
                    &offered.id,
                    Some(session)
                ),
                Err(PlaybackError::NotFound)
            ));
            assert!(initial.is_current());
            let stream = bridge
                .accept_remote_offer(&owner, &offered.id, Some(session))
                .unwrap();
            assert!(!initial.is_current());
            {
                let state = bridge.shared.lock().unwrap();
                assert_eq!(state.remote_playback.start_context.as_ref(), Some(&media));
                let (position, explicit_recovery, preferences) = state
                    .remote_playback
                    .start_retry
                    .as_ref()
                    .expect("accepted replacement captures receiver context")
                    .retry_state();
                assert_eq!(position, Some(97.25));
                assert!(
                    explicit_recovery,
                    "recovery must bypass ordinary Ask semantics"
                );
                let preferences =
                    preferences.expect("capture latest semantic preferences at acceptance");
                assert_eq!(preferences.title.audio, latest.title.audio);
                assert_eq!(preferences.title.subtitles, latest.title.subtitles);
                assert_eq!(preferences.title.speed, latest.title.speed);
                assert_eq!(
                    preferences.title.subtitle_style,
                    latest.title.subtitle_style
                );
                assert_eq!(preferences.title.intro_skip, latest.title.intro_skip);
                assert_eq!(preferences.audio_language, latest.audio_language);
                assert_eq!(preferences.subtitle_language, latest.subtitle_language);
                assert_eq!(preferences.default_speed, latest.default_speed);
                assert_eq!(
                    preferences.default_subtitle_style,
                    latest.default_subtitle_style
                );
                assert!(!state.remote_playback.offers.contains_key(&offered.id));
            }
            let accepted_source = bridge.player.screenshot_session();
            // Launch the actual accepted source: this backend cannot install a
            // video rendering notifier, so success would be a false fixture.
            assert_eq!(
                bridge.launch_remote_offer(stream),
                Response::Error(PlaybackError::Failed(String::new()))
            );
            assert_ne!(
                bridge.player.screenshot_session(),
                accepted_source,
                "native launch is reached rather than diverted to Ask"
            );
            assert!(!app.get_resume_prompt_visible());
            let state = bridge.shared.lock().unwrap();
            assert!(state.pending_resume_stream.is_none());
            assert!(state.remote_playback.start_context.is_none());
            assert!(state.remote_playback.start_retry.is_none());
            assert!(
                state.remote_playback.session.is_none(),
                "failed native launch cannot leave a fictitious live receiver session"
            );
            drop(state);
            let actual = bridge.player.retry_preferences();
            assert_eq!(actual.title.audio, latest.title.audio);
            assert_eq!(actual.title.subtitles, latest.title.subtitles);
            assert_eq!(actual.title.speed, latest.title.speed);
            assert_eq!(actual.title.subtitle_style, latest.title.subtitle_style);
            assert_eq!(actual.audio_language, latest.audio_language);
            assert_eq!(actual.subtitle_language, latest.subtitle_language);
        },
    );
}

#[test]
fn sleep_timer_is_receiver_owned_survives_disconnect_and_rejects_old_session_cancel() {
    isolated(
        concat!(
            module_path!(),
            "::sleep_timer_is_receiver_owned_survives_disconnect_and_rejects_old_session_cancel"
        ),
        |Fixture { app, bridge }| {
            let media = MediaIdentity {
                media_type: "series".into(),
                title_id: "timer-show".into(),
                episode_id: "e1".into(),
                ..Default::default()
            };
            bridge.shared.lock().unwrap().remote_playback.start_context = Some(media.clone());
            let request = bridge.begin_playback_request();
            app.set_player_open(true);
            let session = bridge.playback_snapshot().session.unwrap();
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::SetSleepTimer(
                            nova_playback_api::SleepTimerMode::Minutes(30)
                        )
                    }
                ),
                Response::Accepted
            );
            assert!(bridge.playback_snapshot().sleep_timer.is_some());
            // Disconnect cleanup releases ephemeral holds, never the receiver timer.
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::ReleasePreview
                ),
                Response::Accepted
            );
            assert!(bridge.playback_snapshot().sleep_timer.is_some());
            bridge.shared.lock().unwrap().remote_playback.start_context = Some(media);
            let replacement = bridge.begin_playback_request();
            assert!(!request.is_current());
            app.set_player_open(true);
            let current = bridge.playback_snapshot().session.unwrap();
            assert_ne!(session, current);
            assert_eq!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Command {
                        session,
                        action: PlaybackAction::CancelSleepTimer
                    }
                ),
                Response::Error(PlaybackError::StaleSession)
            );
            assert!(bridge.playback_snapshot().sleep_timer.is_some());
            // A native deadline elapsed while Slint was paused invalidates the
            // pending source and closes before its late callback can start it.
            bridge.player.set_sleep_deadline(Some(0));
            assert!(bridge.refresh_sleep_timer());
            assert!(!app.get_player_open());
            assert!(!replacement.is_current());
            assert!(!bridge.playback_request_is_current(&replacement));
            assert!(bridge.playback_snapshot().sleep_timer.is_none());
        },
    );
}

#[test]
fn cancelling_a_candidate_keeps_live_playback_timer_but_closing_loading_player_cancels_it() {
    isolated(
        concat!(
            module_path!(),
            "::cancelling_a_candidate_keeps_live_playback_timer_but_closing_loading_player_cancels_it"
        ),
        |Fixture { app, bridge }| {
            let media = MediaIdentity {
                media_type: "series".into(),
                title_id: "timer-show".into(),
                episode_id: "e1".into(),
                ..Default::default()
            };
            bridge.shared.lock().unwrap().remote_playback.start_context = Some(media.clone());
            let candidate = bridge.begin_playback_request();
            app.set_player_open(true); // An older loaded item remains on screen.
            bridge
                .set_sleep_timer(nova_playback_api::SleepTimerMode::Minutes(30))
                .unwrap();
            bridge.cancel_pending_playback();
            assert!(!candidate.is_current());
            assert!(app.get_player_open());
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .sleep_timer
                    .snapshot(crate::player::elapsed_realtime_ms())
                    .is_some()
            );
            assert!(!bridge.player.sleep_deadline_expired());

            bridge.shared.lock().unwrap().remote_playback.start_context = Some(media);
            let loading = bridge.begin_playback_request();
            app.set_player_open(true);
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_mut()
                .unwrap()
                .in_app = true;
            bridge.cancel_pending_playback();
            assert!(!loading.is_current());
            assert!(!app.get_player_open());
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .sleep_timer
                    .snapshot(crate::player::elapsed_realtime_ms())
                    .is_none()
            );
        },
    );
}

#[test]
fn restricted_remote_offers_require_approved_title_and_local_provenance() {
    isolated_prepared(
        concat!(
            module_path!(),
            "::restricted_remote_offers_require_approved_title_and_local_provenance"
        ),
        |root| {
            // Real startup establishes the selected profile before Bridge
            // attaches its activation session. Initializing afterward would
            // replace that context with a durable owner that cannot use UI
            // state, and would test a lifecycle production never performs.
            profiles::initialize().unwrap();
            seed_receiver_downloads(root);
        },
        |Fixture { app: _app, bridge }| {
            let owner = profiles::ProfileStore::capture();
            assert!(owner.is_scoped());
            assert!(bridge.shared.lock().unwrap().installed.is_empty());
            let mut policy = profile_policy::ProfilePolicy {
                pin: "0012".into(),
                restricted: true,
                allowed_titles: vec![profile_policy::AllowedTitle {
                    media_type: "series".into(),
                    title_id: "tt100".into(),
                }],
                ..Default::default()
            };
            owner
                .write(
                    "library",
                    r#"[{"id":"tt100","type_":"series","name":"Receiver show"}]"#,
                )
                .unwrap();
            owner
                .write(
                    profile_policy::KEY,
                    &serde_json::to_string(&policy).unwrap(),
                )
                .unwrap();
            write_episodes_cache_for(
                "series",
                "tt100",
                &[alias_video("tt100:1:1", 1, &["kitsu:100:1"])],
            );
            let response = receive(
                &bridge,
                peer("controller", 1, true),
                Request::Lookup(alias_media("tt100:1:1")),
            );
            assert!(
                matches!(response,Response::Offers(ref rows) if rows.is_empty()),
                "unapproved local source must not be offered: {response:?}"
            );
            policy.allowed_addons.push("local".into());
            owner
                .write(
                    profile_policy::KEY,
                    &serde_json::to_string(&policy).unwrap(),
                )
                .unwrap();
            assert_download_offer(
                &bridge,
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("tt100:1:1")),
                ),
                1,
            );
            policy.allowed_titles.clear();
            owner
                .write(
                    profile_policy::KEY,
                    &serde_json::to_string(&policy).unwrap(),
                )
                .unwrap();
            assert!(matches!(
                receive(
                    &bridge,
                    peer("controller", 1, true),
                    Request::Lookup(alias_media("tt100:1:1"))
                ),
                Response::Error(PlaybackError::Unavailable)
            ));
        },
    );
}

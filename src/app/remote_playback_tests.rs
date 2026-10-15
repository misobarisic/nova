//! Coordinator behavior against a real Bridge, with one storage/backend per child.
use super::*;

struct Fixture {
    app: AppWindow,
    bridge: Bridge,
}

fn isolated(name: &str, body: impl FnOnce(Fixture)) {
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
        |Fixture { app: _, bridge }| {
            let session = SessionId {
                epoch: 7,
                sequence: 1,
            };
            let token = session_text(Some(session));
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.snapshot = Some(PlaybackSnapshot {
                    session: Some(session),
                    ready: true,
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
        |Fixture { app: _, bridge }| {
            let session = SessionId {
                epoch: 7,
                sequence: 1,
            };
            let token = session_text(Some(session));
            {
                let mut state = bridge.shared.lock().unwrap();
                state.remote_playback.role = Role::Remote;
                state.remote_playback.snapshot = Some(PlaybackSnapshot {
                    session: Some(session),
                    ready: true,
                    ..Default::default()
                });
            }
            for volume in 0..500 {
                bridge.remote_ui_action("volume", volume as f32, &token);
            }
            assert_eq!(
                bridge.shared.lock().unwrap().remote_playback.queued.len(),
                1
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
                Response::Error(PlaybackError::Unsupported)
            );
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

fn install_receiver_downloads(bridge: &mut Bridge) {
    let root = PathBuf::from(std::env::var_os("NOVA_REMOTE_COORDINATOR_TEST_ROOT").unwrap())
        .join("downloads");
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
    bridge.downloads = DownloadCoordinator::new(root);
    assert!(bridge.shared.lock().unwrap().installed.is_empty());
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
    isolated(
        concat!(
            module_path!(),
            "::cached_remote_aliases_find_receiver_downloads_without_metadata_addons"
        ),
        |Fixture {
             app: _app,
             mut bridge,
         }| {
            install_receiver_downloads(&mut bridge);
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
    isolated(
        concat!(
            module_path!(),
            "::live_remote_metadata_resolves_after_reconciliation_without_first_alias_wins"
        ),
        |Fixture {
             app: _app,
             mut bridge,
         }| {
            install_receiver_downloads(&mut bridge);
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
    isolated(
        concat!(
            module_path!(),
            "::receiver_sizes_survive_offers_and_controller_stream_rows"
        ),
        |Fixture { app, mut bridge }| {
            install_receiver_downloads(&mut bridge);
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
            settings::configure_plugin_settings(Arc::new(plugin_settings::Backend)).unwrap();
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

//! Real-Bridge coordinator regressions; every test owns its backend and storage.
use super::*;

struct Fixture {
    app: AppWindow,
    bridge: Bridge,
}

fn isolated(name: &str, body: impl FnOnce(Fixture)) {
    const ROOT: &str = "NOVA_PARTY_COORDINATOR_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-party-coordinator-{}-{}-{}",
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
    remote_playback::playback_foreground(true);
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

fn room(role: PartyRole) -> RoomSnapshot {
    RoomSnapshot {
        control_policy: nova_party::ControlPolicy::HostOnly,
        control_revision: 0,
        pending_media: None,
        role,
        room_id: "test-room".into(),
        host_id: "host".into(),
        member_id: if role == PartyRole::Host {
            "host"
        } else {
            "guest"
        }
        .into(),
        media: Some(PartyMedia {
            identity: MediaIdentity {
                media_type: "movie".into(),
                title_id: "tt123".into(),
                episode_id: "tt123".into(),
                title: "Example Movie".into(),
                episode: String::new(),
            },
            season: None,
            episode: None,
            duration: 120.0,
        }),
        generation: 1,
        revision: 1,
        timeline: nova_party::Timeline {
            target_rate: 1.0,
            position: 20.0,
            anchor_ms: 0,
            paused: true,
        },
        participants: Vec::new(),
        waiting: Vec::new(),
        wait_for_guests: true,
        connected: true,
        ended: false,
        error: None,
        clock_offset_ms: 0.0,
    }
}

fn install_room(bridge: &Bridge, app: &AppWindow, role: PartyRole) -> Arc<PartyService> {
    let service = PartyService::new().unwrap();
    {
        let mut state = bridge.shared.lock().unwrap();
        state.party.service = Some(service.clone());
        state.party.snapshot = Some(room(role));
        // Hold worker dispatch: these tests exercise the coordinator without
        // binding an endpoint. This is separate from the setup-only UI busy flag.
        state.party.busy = true;
    }
    app.set_party_active(true);
    app.set_party_busy(false);
    service
}

fn select_local_media(bridge: &Bridge, media: &PartyMedia) {
    let identity = &media.identity;
    bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
        details: Default::default(),
        open_token: Arc::new(()),
        pending_watch_now: None,
        episodes_loading: false,
        id: identity.title_id.clone(),
        type_: identity.media_type.clone(),
        request_id: identity.episode_id.clone(),
        videos: Vec::new(),
        season_backdrops: HashMap::new(),
        seasons: if identity.media_type == "movie" {
            Vec::new()
        } else {
            vec![1]
        },
        season_index: 0,
        episode_page: 0,
        name: identity.title.clone(),
        year: String::new(),
        poster_url: String::new(),
        background_url: String::new(),
        logo_url: String::new(),
        description: String::new(),
        genres: Vec::new(),
    });
    let selected = bridge
        .shared
        .lock()
        .unwrap()
        .party
        .snapshot
        .as_ref()
        .and_then(RoomSnapshot::selected_media)
        .cloned();
    if selected
        .as_ref()
        .is_some_and(|expected| compatible_media(media, expected, false))
    {
        bridge.party_prepare_stream_selection();
    }
}

#[test]
fn party_sources_ignore_history_and_ask_without_changing_the_saved_preference() {
    isolated(
        concat!(
            module_path!(),
            "::party_sources_ignore_history_and_ask_without_changing_the_saved_preference"
        ),
        |Fixture { app, bridge }| {
            let settings = {
                let mut state = bridge.shared.lock().unwrap();
                state.cache_settings.episode_start_behavior = EpisodeStartBehavior::Ask;
                state.cache_settings.clone()
            };
            settings::write_settings(&settings);
            let saved_settings = storage::get_str("settings");
            let saved_preferences = storage::get_str(playback_preferences::PREFERENCES_KEY);
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            for media_type in ["series", "movie"] {
                let mut expected = room(PartyRole::Guest);
                expected.media.as_mut().unwrap().identity.media_type = media_type.into();
                if media_type == "series" {
                    expected.media.as_mut().unwrap().identity.episode_id = "tt123:1:2".into();
                    expected.media.as_mut().unwrap().season = Some(1);
                    expected.media.as_mut().unwrap().episode = Some(2);
                }
                for snapshot_kind in ["compatible", "different", "unavailable"] {
                    select_local_media(&bridge, expected.media.as_ref().unwrap());
                    let key = progress_map_key(
                        "tt123",
                        &expected.media.as_ref().unwrap().identity.episode_id,
                    );
                    {
                        let mut state = bridge.shared.lock().unwrap();
                        state.playback = None;
                        state.progress.insert(
                            key,
                            EpisodeProgress {
                                series_id: "tt123".into(),
                                episode_id: expected
                                    .media
                                    .as_ref()
                                    .unwrap()
                                    .identity
                                    .episode_id
                                    .clone(),
                                position_secs: 300.0,
                                duration_secs: 1000.0,
                                ..Default::default()
                            },
                        );
                        state.party.snapshot = match snapshot_kind {
                            "compatible" => Some(expected.clone()),
                            "different" => {
                                let mut other = expected.clone();
                                other.media.as_mut().unwrap().identity.title_id = "tt456".into();
                                Some(other)
                            }
                            _ => None,
                        };
                    }

                    if snapshot_kind == "compatible" {
                        bridge.party_prepare_stream_selection();
                    }
                    // Arming the load is observable even on a headless backend
                    // without mpv; no decoder or network is needed here.
                    let _ = bridge.open_player("https://example.invalid/party".into());

                    assert!(!app.get_resume_prompt_visible());
                    let state = bridge.shared.lock().unwrap();
                    assert!(state.pending_resume_stream.is_none());
                    if snapshot_kind != "compatible" {
                        assert!(
                            state.playback.is_none(),
                            "Unaccepted media must not arm playback"
                        );
                        drop(state);
                        bridge.player.close();
                        continue;
                    }
                    let target = state.playback.as_ref().unwrap();
                    assert_eq!(
                        target.resume_pos,
                        (snapshot_kind == "compatible").then_some(20.0),
                        "{media_type}: {snapshot_kind}"
                    );
                    assert!(
                        target.resume_done,
                        "Only the party coordinator may align it"
                    );
                    assert_eq!(
                        state.cache_settings.episode_start_behavior,
                        EpisodeStartBehavior::Ask
                    );
                    drop(state);
                    bridge.player.close();
                }
            }
            assert_eq!(storage::get_str("settings"), saved_settings);
            assert_eq!(
                storage::get_str(playback_preferences::PREFERENCES_KEY),
                saved_preferences
            );

            // Outside a party the same saved Ask preference still gates an
            // episode load, proving this is a session policy rather than a
            // temporary mutation of the user's preference.
            bridge.party_leave();
            let mut episode = room(PartyRole::Guest).media.unwrap();
            episode.identity.media_type = "series".into();
            episode.identity.episode_id = "tt123:1:2".into();
            select_local_media(&bridge, &episode);
            let _ = bridge.open_player("https://example.invalid/ordinary".into());
            assert!(app.get_resume_prompt_visible());
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .pending_resume_stream
                    .is_some()
            );
        },
    );
}

#[test]
fn party_start_uses_the_actual_intent_host_clock_and_duration_bound() {
    isolated(
        concat!(
            module_path!(),
            "::party_start_uses_the_actual_intent_host_clock_and_duration_bound"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            select_local_media(&bridge, room(PartyRole::Guest).media.as_ref().unwrap());
            let _request = bridge.begin_playback_request();
            {
                let mut state = bridge.shared.lock().unwrap();
                // Browsing another detail page must not change the source's
                // identity once the playback request has captured it.
                state.modal_item.as_mut().unwrap().id = "tt456".into();
                let room = state.party.snapshot.as_mut().unwrap();
                room.timeline.paused = false;
                room.timeline.anchor_ms = nova_party::local_now_ms();
                room.clock_offset_ms = 5_000.0;
            }
            let position = bridge.party_start_position().unwrap();
            assert!((25.0..26.0).contains(&position), "{position}");
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .timeline
                .position = 500.0;
            assert_eq!(bridge.party_start_position(), Some(120.0));
        },
    );
}

#[test]
fn choosing_another_source_closes_the_player_without_leaving_the_room() {
    isolated(
        concat!(
            module_path!(),
            "::choosing_another_source_closes_the_player_without_leaving_the_room"
        ),
        |Fixture { app, bridge }| {
            bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: "tt123".into(),
                type_: "movie".into(),
                request_id: "tt123".into(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: "Example Movie".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            });
            let pending = bridge.begin_playback_request();
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_position(20.0);
            app.set_duration(120.0);
            app.set_party_page_open(true);
            let service = install_room(&bridge, &app, PartyRole::Guest);
            assert!(compatible_media(
                &bridge.party_media().unwrap(),
                room(PartyRole::Guest).media.as_ref().unwrap(),
                false
            ));
            let generation = bridge.shared.lock().unwrap().party.generation;

            bridge.party_choose_stream();

            assert!(!app.get_player_open());
            assert!(!pending.is_current());
            assert!(!app.get_party_page_open());
            assert!(app.get_party_active());
            assert!(!app.get_party_busy());
            assert!(app.get_modal_visible());
            assert!(app.get_stream_selector_open());
            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert!(state.party.choosing_source);
            assert_eq!(state.party.generation, generation);
            assert_eq!(
                state.party.snapshot.as_ref().unwrap().role,
                PartyRole::Guest
            );
        },
    );
}

#[test]
fn leaving_retires_pending_work_and_temporary_rate_without_changing_sync_settings() {
    isolated(
        concat!(
            module_path!(),
            "::leaving_retires_pending_work_and_temporary_rate_without_changing_sync_settings"
        ),
        |Fixture { app, bridge }| {
            let sync = nova_sync::SyncSettings {
                enabled: false,
                device_name: "Local device".into(),
                require_confirmation: true,
                ..Default::default()
            };
            nova_sync::write_settings(&sync);
            let cache_before =
                serde_json::to_value(&bridge.shared.lock().unwrap().cache_settings).unwrap();
            let preferences_before = storage::get_str(playback_preferences::PREFERENCES_KEY);
            let _service = install_room(&bridge, &app, PartyRole::Host);
            bridge.player.set_title_speed(1.5);
            bridge.player.set_party_playback_rate(Some(1.03));
            assert!((bridge.player.effective_playback_speed() - 1.03).abs() < 0.0001);
            let generation = {
                let mut state = bridge.shared.lock().unwrap();
                state.party.generation = 7;
                state.party.choosing_source = true;
                state.party.choosing_media = true;
                state.party.resume_guard = true;
                state.party.pending.push_back(Operation::Command(
                    1,
                    0,
                    HostCommand::SeekAbsolute(80.0),
                ));
                state.party.generation
            };
            app.set_party_busy(true);
            app.set_party_waiting(true);
            app.set_party_invite("temporary-invite".into());
            app.set_party_invite_qr(invite_qr_image("temporary-invite").unwrap());

            bridge.party_leave();

            let state = bridge.shared.lock().unwrap();
            assert_ne!(
                state.party.generation, generation,
                "Late completions must fail their generation check"
            );
            assert!(!state.party.active());
            assert!(state.party.snapshot.is_none());
            assert!(state.party.pending.is_empty());
            assert!(!state.party.choosing_source);
            assert!(!state.party.choosing_media);
            assert!(!state.party.resume_guard);
            assert_eq!(
                serde_json::to_value(&state.cache_settings).unwrap(),
                cache_before
            );
            drop(state);
            assert!(!app.get_party_active());
            assert!(!app.get_party_busy());
            assert!(!app.get_party_waiting());
            assert!(app.get_party_invite().is_empty());
            assert_eq!(app.get_party_invite_qr().size().width, 0);
            assert_eq!(bridge.player.effective_playback_speed(), 1.5);
            assert_eq!(nova_sync::read_settings(), sync);
            assert_eq!(
                storage::get_str(playback_preferences::PREFERENCES_KEY),
                preferences_before
            );
            assert!(
                nova_sync::engine().is_none(),
                "Party lifecycle must not enable normal sync"
            );

            // Setup may still be running before a service exists at all.
            app.set_player_open(true);
            bridge.player.set_paused(true);
            let epoch = bridge.player.screenshot_session();
            let generation = {
                let mut state = bridge.shared.lock().unwrap();
                state.party.busy = true;
                state.party.setup_restore = Some((epoch, false));
                state.party.generation
            };
            assert!(bridge.party_active());
            bridge.party_leave();
            let state = bridge.shared.lock().unwrap();
            assert!(!state.party.active());
            assert_ne!(state.party.generation, generation);
            assert!(
                !app.get_is_paused(),
                "Cancelling setup restores the original playback pause state"
            );
        },
    );
}

#[test]
fn semantic_chapter_and_bookmark_seeks_use_the_host_timeline_and_reject_guests() {
    isolated(
        concat!(
            module_path!(),
            "::semantic_chapter_and_bookmark_seeks_use_the_host_timeline_and_reject_guests"
        ),
        |Fixture { app, bridge }| {
            select_local_media(&bridge, room(PartyRole::Host).media.as_ref().unwrap());
            let request = bridge.begin_playback_request();
            assert!(bridge.playback_request_is_current(&request));
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(120.0);
            app.set_position(80.0);
            assert!(bridge.add_scene_bookmark("Shared scene"));
            let bookmark = app
                .get_scene_bookmarks()
                .row_data(0)
                .unwrap()
                .id
                .to_string();
            app.set_player_chapters(
                Rc::new(VecModel::from(vec![crate::ChapterRow {
                    title: "Chapter".into(),
                    position: 35.0,
                }]))
                .into(),
            );
            app.set_position(20.0);
            let _service = install_room(&bridge, &app, PartyRole::Host);
            // This fixture already presents the accepted episode. Claim its
            // picker before dispatch so the first report does not perform the
            // normal once-per-generation source-follow navigation.
            bridge.party_prepare_stream_selection();
            let actions = [
                PlaybackAction::SeekChapter {
                    id: format!("c{:016x}", 35.0f64.to_bits()),
                },
                PlaybackAction::SeekBookmark { id: bookmark },
            ];
            for action in actions.iter().cloned() {
                bridge.dispatch_playback_action(action).unwrap();
                assert_eq!(
                    app.get_position(),
                    20.0,
                    "the room coordinator owns the actual seek"
                );
            }
            {
                let mut state = bridge.shared.lock().unwrap();
                assert_eq!(state.party.pending.len(), 2);
                assert!(matches!(
                    state.party.pending[0],
                    Operation::Command(1, 0, HostCommand::SeekAbsolute(35.0))
                ));
                assert!(matches!(
                    state.party.pending[1],
                    Operation::Command(1, 0, HostCommand::SeekAbsolute(80.0))
                ));
                state.party.pending.clear();
                state.party.snapshot = Some(room(PartyRole::Guest));
            }
            for action in actions {
                bridge.dispatch_playback_action(action).unwrap();
                assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
                assert_eq!(app.get_position(), 20.0);
                assert!(!app.get_party_error().is_empty());
            }
        },
    );
}

#[test]
fn guests_cannot_change_the_timeline_but_keep_local_track_and_volume_controls() {
    isolated(
        concat!(
            module_path!(),
            "::guests_cannot_change_the_timeline_but_keep_local_track_and_volume_controls"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_is_paused(true);
            app.set_position(20.0);
            app.set_duration(120.0);
            bridge.player.set_party_playback_rate(Some(1.0));
            for action in [
                PlaybackAction::TogglePause,
                PlaybackAction::SetPaused(false),
                PlaybackAction::SeekAbsolute(80.0),
                PlaybackAction::SeekRelative(10.0),
            ] {
                bridge.dispatch_playback_action(action).unwrap();
                assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
                assert!(app.get_is_paused());
                assert_eq!(app.get_position(), 20.0);
                assert!(!app.get_party_error().is_empty());
            }
            for action in [
                PlaybackAction::SelectAudioTrack(1),
                PlaybackAction::SelectSubtitleTrack(-1),
                PlaybackAction::ResetAudioTrack,
                PlaybackAction::ResetSubtitleTrack,
                PlaybackAction::SetVolume(37.0),
                PlaybackAction::SetMuted(true),
            ] {
                assert!(
                    !bridge.party_action(&action),
                    "Local controls must reach the ordinary player path"
                );
            }
            bridge
                .dispatch_playback_action(PlaybackAction::SetVolume(37.0))
                .unwrap();
            assert_eq!(app.get_volume(), 37.0);
            bridge
                .dispatch_playback_action(PlaybackAction::SetMuted(true))
                .unwrap();
            assert!(app.get_muted());
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert!(bridge.party_active());
            assert!(
                !app.get_party_busy(),
                "Report serialization must not disable room UI controls"
            );
            assert_eq!(bridge.player.effective_playback_speed(), 1.0);
        },
    );
}

#[test]
fn hosting_can_start_without_a_loaded_player() {
    isolated(
        concat!(
            module_path!(),
            "::hosting_can_start_without_a_loaded_player"
        ),
        |Fixture { app, bridge }| {
            assert!(!app.get_player_open());
            assert!(bridge.party_media().is_none());
            bridge.party_tick();
            assert!(app.get_party_can_host());

            bridge.party_begin(None);

            assert!(bridge.party_active());
            assert!(app.get_party_busy());
            assert!(app.get_party_error().is_empty());
            let state = bridge.shared.lock().unwrap();
            assert!(state.party.busy);
            assert!(state.party.setup_restore.is_none());
            drop(state);
            // No endpoint completion or native decoder is needed to prove
            // hosting accepts a lobby with no selected local media.
            bridge.party_leave();
            assert!(!bridge.party_active());
        },
    );
}

#[test]
fn lobby_publication_and_reports_do_not_reuse_an_old_local_video() {
    isolated(
        concat!(
            module_path!(),
            "::lobby_publication_and_reports_do_not_reuse_an_old_local_video"
        ),
        |Fixture { app, bridge }| {
            select_local_media(&bridge, room(PartyRole::Host).media.as_ref().unwrap());
            let _request = bridge.begin_playback_request();
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(120.0);
            for role in [PartyRole::Host, PartyRole::Guest] {
                let _service = install_room(&bridge, &app, role);
                let snapshot = {
                    let mut state = bridge.shared.lock().unwrap();
                    let snapshot = state.party.snapshot.as_mut().unwrap();
                    snapshot.media = None;
                    snapshot.generation = 3;
                    snapshot.clone()
                };

                bridge.party_publish();
                bridge.party_tick();

                assert!(app.get_party_active());
                assert!(!app.get_party_has_media());
                assert!(app.get_party_title().is_empty());
                assert!(app.get_party_episode().is_empty());
                assert_eq!(app.get_party_duration(), 0.0);
                assert_eq!(app.get_party_position(), 0.0);
                assert!(app.get_party_time_text().is_empty());
                assert!(app.get_party_warning().is_empty());
                assert!(app.get_party_can_return_to_player());
                assert!(app.get_is_paused());
                assert_eq!(
                    app.get_party_status(),
                    if role == PartyRole::Host {
                        ""
                    } else {
                        "Waiting for the host to choose"
                    }
                );
                let report = bridge.party_report(Some(&snapshot));
                assert_eq!(report.generation, 3);
                assert!(report.media.is_none());
                assert!(!report.loaded);
                assert!(!report.ready);
                assert!(bridge.party_start_position().is_none());
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .connected = false;
                bridge.party_publish();
                assert_eq!(app.get_party_status(), "Reconnecting to the host…");
            }
        },
    );
}

#[test]
fn host_title_changes_preserve_membership_and_retire_the_old_timeline() {
    isolated(
        concat!(
            module_path!(),
            "::host_title_changes_preserve_membership_and_retire_the_old_timeline"
        ),
        |Fixture { app, bridge }| {
            let service = install_room(&bridge, &app, PartyRole::Host);
            app.set_party_invite("same-invite".into());
            let before = bridge.shared.lock().unwrap().party.snapshot.clone();
            bridge.party_prepare_media_change(true);
            assert_eq!(bridge.shared.lock().unwrap().party.snapshot, before);
            assert!(
                bridge.shared.lock().unwrap().party.pending.is_empty(),
                "Browsing has no media intent"
            );
            let mut other = before.as_ref().unwrap().media.clone().unwrap();
            other.identity.title_id = "tt456".into();
            other.identity.episode_id = "tt456".into();
            assert!(bridge.party_preflight_media(other));
            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert_eq!(state.party.snapshot, before);
            assert!(state.party.choosing_media);
            assert!(
                matches!(state.party.pending.front(), Some(Operation::PrepareMedia(1, 0, media)) if media.identity.title_id == "tt456")
            );
            assert_eq!(app.get_party_invite(), "same-invite");
            assert!(app.get_party_active());
        },
    );
}

#[test]
fn host_local_source_replacement_does_not_replace_the_shared_title() {
    isolated(
        concat!(
            module_path!(),
            "::host_local_source_replacement_does_not_replace_the_shared_title"
        ),
        |Fixture { app, bridge }| {
            let service = install_room(&bridge, &app, PartyRole::Host);
            let snapshot = bridge.shared.lock().unwrap().party.snapshot.clone();
            select_local_media(&bridge, snapshot.as_ref().unwrap().media.as_ref().unwrap());
            let request = bridge.begin_playback_request();
            app.set_player_open(true);
            app.set_playback_started(true);

            bridge.party_choose_stream();

            assert!(!app.get_player_open());
            assert!(!request.is_current());
            assert!(app.get_modal_visible());
            assert!(app.get_stream_selector_open());
            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert_eq!(state.party.snapshot, snapshot);
            assert!(state.party.choosing_source);
            assert!(!state.party.choosing_media);
            assert!(state.party.pending.is_empty());
        },
    );
}

#[test]
fn guests_cannot_replace_the_shared_title() {
    isolated(
        concat!(module_path!(), "::guests_cannot_replace_the_shared_title"),
        |Fixture { app, bridge }| {
            let service = install_room(&bridge, &app, PartyRole::Guest);
            let snapshot = bridge.shared.lock().unwrap().party.snapshot.clone();
            for browse in [true, false] {
                bridge.party_prepare_media_change(browse);
            }
            assert!(bridge.party_action(&PlaybackAction::NextEpisode {
                token: String::new()
            }));
            let mut other = room(PartyRole::Guest).media.unwrap();
            other.identity.title_id = "tt456".into();
            other.identity.episode_id = "tt456".into();
            other.identity.title = "Another Movie".into();
            select_local_media(&bridge, &other);
            let _request = bridge.begin_playback_request();
            let report = bridge.party_report(snapshot.as_ref());
            assert!(report.media.is_none());
            assert!(!report.ready);
            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert_eq!(state.party.snapshot, snapshot);
            assert!(!state.party.choosing_media);
            assert!(state.party.pending.is_empty());
        },
    );
}

#[test]
fn stopping_playback_keeps_hosts_and_guests_in_the_room() {
    isolated(
        concat!(
            module_path!(),
            "::stopping_playback_keeps_hosts_and_guests_in_the_room"
        ),
        |Fixture { app, bridge }| {
            for role in [PartyRole::Host, PartyRole::Guest] {
                let service = install_room(&bridge, &app, role);
                select_local_media(&bridge, room(role).media.as_ref().unwrap());
                let request = bridge.begin_playback_request();
                app.set_player_open(true);
                app.set_playback_started(true);
                app.set_party_page_open(false);
                app.set_party_invite("same-invite".into());
                let generation = bridge.shared.lock().unwrap().party.generation;

                bridge
                    .dispatch_playback_action(PlaybackAction::Stop)
                    .unwrap();

                assert!(!app.get_player_open());
                assert!(!request.is_current());
                assert!(app.get_party_active());
                assert!(app.get_party_page_open());
                assert_eq!(app.get_party_invite(), "same-invite");
                let state = bridge.shared.lock().unwrap();
                assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
                assert_eq!(state.party.generation, generation);
                assert_eq!(state.party.snapshot.as_ref().unwrap().role, role);
                if role == PartyRole::Host {
                    assert!(state.party.choosing_media);
                    assert!(matches!(
                        state.party.pending.front(),
                        Some(Operation::ClearMedia { browse: false })
                    ));
                } else {
                    assert!(!state.party.choosing_media);
                    assert!(state.party.pending.is_empty());
                }
                drop(state);
                // Keep the next role's assertions independent without sending
                // Leave or waiting for an endpoint that these fixtures omit.
                bridge.shared.lock().unwrap().party = State::default();
            }
        },
    );
}

fn prepare_next_party(bridge: &Bridge, app: &AppWindow, role: PartyRole) -> String {
    let _service = install_room(bridge, app, role);
    let mut media = room(role).media.unwrap();
    media.identity.media_type = "series".into();
    media.identity.episode_id = "tt123:1:1".into();
    media.identity.episode = "S1 E1".into();
    media.season = Some(1);
    media.episode = Some(1);
    media.duration = 1440.0;
    bridge
        .shared
        .lock()
        .unwrap()
        .party
        .snapshot
        .as_mut()
        .unwrap()
        .media = Some(media.clone());
    if role == PartyRole::Guest {
        bridge
            .shared
            .lock()
            .unwrap()
            .party
            .snapshot
            .as_mut()
            .unwrap()
            .control_policy = ControlPolicy::Everyone;
    }
    select_local_media(bridge, &media);
    {
        let mut state = bridge.shared.lock().unwrap();
        let modal = state.modal_item.as_mut().unwrap();
        modal.videos = (1..=3)
            .map(|episode| Video {
                id: format!("tt123:1:{episode}"),
                season: Some(1),
                episode: Some(episode),
                ..Default::default()
            })
            .collect();
        modal.seasons = vec![1];
        state.cache_settings.prefetch_next_episode_streams = false;
    }
    bridge.party_prepare_stream_selection();
    let request = bridge.begin_playback_request();
    assert!(request.is_current());
    bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
        session: next_episode::new_session(),
        series_id: "tt123".into(),
        episode_id: "tt123:1:1".into(),
        ..Default::default()
    });
    app.set_modal_visible(true);
    app.set_detail_is_movie(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(1440.0);
    app.set_position(1321.0);
    bridge.refresh_next_episode_prompt();
    assert!(app.get_next_episode_visible());
    app.get_next_episode_token().to_string()
}

fn accept_next_party(bridge: &Bridge) -> (NextSelection, PlaybackGuard) {
    let mut state = bridge.shared.lock().unwrap();
    let proposed = match state.party.pending.pop_front().unwrap() {
        Operation::PrepareMedia(1, 0, media) => media,
        _ => panic!("Next must propose once against the original generation"),
    };
    let mut accepted = state.party.snapshot.clone().unwrap();
    accepted.generation = 2;
    accepted.media = None;
    accepted.pending_media = Some(proposed);
    let (download, next) = state.party.accept_prepared_selection(&accepted, 1);
    assert!(download.is_none());
    state.party.snapshot = Some(accepted);
    next.unwrap()
}

#[test]
fn host_and_everyone_guest_next_wait_for_acceptance_then_open_their_source_picker() {
    isolated(
        concat!(
            module_path!(),
            "::host_and_everyone_guest_next_wait_for_acceptance_then_open_their_source_picker"
        ),
        |Fixture { app, bridge }| {
            for role in [PartyRole::Host, PartyRole::Guest] {
                bridge.shared.lock().unwrap().party = State::default();
                let token = prepare_next_party(&bridge, &app, role);
                let close_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let count = close_calls.clone();
                let close_bridge = bridge.clone();
                app.on_close_player(move || {
                    count.fetch_add(1, Ordering::Relaxed);
                    close_bridge
                        .dispatch_playback_action(PlaybackAction::Stop)
                        .unwrap();
                });
                app.window().set_fullscreen(true);
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode {
                        token: token.clone(),
                    })
                    .unwrap();
                assert!(
                    app.get_player_open(),
                    "Keep the original player until the room accepts"
                );
                assert!(
                    app.get_party_busy(),
                    "Awaiting acceptance disables repeat UI requests"
                );
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .playback
                        .as_ref()
                        .unwrap()
                        .episode_id,
                    "tt123:1:1"
                );
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                    .unwrap();
                assert_eq!(
                    bridge.shared.lock().unwrap().party.pending.len(),
                    1,
                    "Duplicate clicks retain the same origin generation"
                );
                let (choice, guard) = accept_next_party(&bridge);
                bridge.party_complete_next(choice, guard);
                assert!(!app.get_player_open());
                assert!(app.window().is_fullscreen());
                assert_eq!(
                    close_calls.load(Ordering::Relaxed),
                    0,
                    "Internal Next close never dispatches global ClearMedia"
                );
                assert!(app.get_stream_selector_open());
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .unwrap()
                        .request_id,
                    "tt123:1:2"
                );
                assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
                let accepted = bridge.shared.lock().unwrap().party.snapshot.clone();
                let owner = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .open_token
                    .clone();
                bridge.party_follow_selection(accepted.as_ref());
                assert!(Arc::ptr_eq(
                    &owner,
                    &bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .unwrap()
                        .open_token
                ));
            }
        },
    );
}

#[test]
fn late_next_acceptance_cannot_close_a_replacement_local_session() {
    isolated(
        concat!(
            module_path!(),
            "::late_next_acceptance_cannot_close_a_replacement_local_session"
        ),
        |Fixture { app, bridge }| {
            let token = prepare_next_party(&bridge, &app, PartyRole::Host);
            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                .unwrap();
            let (choice, guard) = accept_next_party(&bridge);
            bridge
                .shared
                .lock()
                .unwrap()
                .playback
                .as_mut()
                .unwrap()
                .session = next_episode::new_session();
            bridge.party_complete_next(choice, guard);
            assert!(app.get_player_open());
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .request_id,
                "tt123:1:1"
            );
        },
    );
}

#[test]
fn stale_next_offer_cannot_advance_a_room_that_already_selected_the_successor() {
    isolated(
        concat!(
            module_path!(),
            "::stale_next_offer_cannot_advance_a_room_that_already_selected_the_successor"
        ),
        |Fixture { app, bridge }| {
            let token = prepare_next_party(&bridge, &app, PartyRole::Guest);
            {
                let mut state = bridge.shared.lock().unwrap();
                let room = state.party.snapshot.as_mut().unwrap();
                room.generation += 1;
                room.media.as_mut().unwrap().identity.episode_id = "tt123:1:2".into();
                room.media.as_mut().unwrap().episode = Some(2);
            }
            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                .unwrap();
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert!(app.get_player_open());
            let winning = bridge.shared.lock().unwrap().party.snapshot.clone();
            bridge.party_follow_selection(winning.as_ref());
            assert!(
                !app.get_player_open(),
                "Follow the other member's winning generation normally"
            );
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .request_id,
                "tt123:1:2"
            );
        },
    );
}

#[test]
fn chosen_source_report_survives_discarded_results_and_retires_on_generation_and_leave() {
    isolated(
        concat!(
            module_path!(),
            "::chosen_source_report_survives_discarded_results_and_retires_on_generation_and_leave"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            let snapshot = room(PartyRole::Guest);
            select_local_media(&bridge, snapshot.media.as_ref().unwrap());
            let chosen = StreamUi {
                id: "chosen".into(),
                choice_id: None,
                owner_url: "local-addon".into(),
                display: "Local choice".into(),
                card: Default::default(),
                source: StreamSource::Url(
                    "https://media.example/exact.mkv?signature=private".into(),
                ),
                addon: "Own addon".into(),
                download: None,
            };
            let expected = party_source::fingerprint(&chosen.source).unwrap();
            bridge.shared.lock().unwrap().streams = vec![chosen.clone()];
            bridge.note_selected_playback_stream(&chosen);
            let _request = bridge.begin_playback_request();
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(120.0);
            bridge.shared.lock().unwrap().streams.clear();
            let report = bridge.party_report(Some(&snapshot));
            assert_eq!(report.source_fingerprint, Some(expected));
            let mut next = snapshot.clone();
            next.generation += 1;
            assert!(
                bridge
                    .party_report(Some(&next))
                    .source_fingerprint
                    .is_none()
            );
            assert!(bridge.shared.lock().unwrap().party.source_choice.is_none());
            bridge.shared.lock().unwrap().party.snapshot = Some(next.clone());
            bridge.note_selected_playback_stream(&chosen);
            assert_eq!(
                bridge.party_report(Some(&next)).source_fingerprint,
                Some(expected)
            );
            bridge.party_leave();
            assert!(bridge.shared.lock().unwrap().party.source_choice.is_none());
            assert!(
                bridge
                    .party_report(Some(&next))
                    .source_fingerprint
                    .is_none()
            );
        },
    );
}

#[test]
fn participant_updates_repaint_only_existing_own_results_without_order_or_focus_changes() {
    isolated(
        concat!(
            module_path!(),
            "::participant_updates_repaint_only_existing_own_results_without_order_or_focus_changes"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            let snapshot = room(PartyRole::Guest);
            select_local_media(&bridge, snapshot.media.as_ref().unwrap());
            let row = |id: &str, url: &str| StreamUi {
                id: id.into(),
                choice_id: None,
                owner_url: "local-addon".into(),
                display: id.into(),
                card: Default::default(),
                source: StreamSource::Url(url.into()),
                addon: "Own addon".into(),
                download: None,
            };
            let own = vec![
                row("first", "https://media.example/one"),
                row("second", "https://media.example/two"),
            ];
            let fingerprint = party_source::fingerprint(&own[1].source).unwrap();
            bridge.shared.lock().unwrap().streams = own;
            let rows: slint::ModelRc<StreamRow> = Rc::new(VecModel::from(vec![
                StreamRow {
                    id: "first".into(),
                    recommended: true,
                    ..Default::default()
                },
                StreamRow {
                    id: "second".into(),
                    ..Default::default()
                },
            ]))
            .into();
            app.set_streams(rows.clone());
            app.set_detail_kb_s(1);
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .participants = vec![nova_party::Participant {
                id: "host".into(),
                name: "Host".into(),
                is_host: true,
                connected: true,
                ready: true,
                buffering: false,
                position: 20.0,
                buffered_ahead: 30.0,
                reason: None,
                source_fingerprint: Some(fingerprint),
            }];
            bridge.party_publish();
            assert_eq!(app.get_streams().row_count(), 2);
            assert!(!app.get_streams().row_data(0).unwrap().party_recommended);
            assert!(app.get_streams().row_data(0).unwrap().recommended);
            assert!(app.get_streams().row_data(1).unwrap().party_recommended);
            assert_eq!(app.get_detail_kb_s(), 1);
            assert!(std::ptr::eq(rows.as_any(), app.get_streams().as_any()));
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .participants[0]
                .connected = false;
            bridge.party_publish();
            assert!(!app.get_streams().row_data(1).unwrap().party_recommended);
            assert_eq!(app.get_streams().row_data(1).unwrap().id, "second");
            assert_eq!(app.get_detail_kb_s(), 1);
            assert_eq!(bridge.shared.lock().unwrap().streams.len(), 2);
            bridge.party_leave();
        },
    );
}

#[test]
fn shared_policy_grants_guest_controls_with_captured_revision_but_keeps_admin_private() {
    isolated(
        concat!(
            module_path!(),
            "::shared_policy_grants_guest_controls_with_captured_revision_but_keeps_admin_private"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            {
                let mut state = bridge.shared.lock().unwrap();
                let room = state.party.snapshot.as_mut().unwrap();
                room.control_policy = ControlPolicy::Everyone;
                room.control_revision = 7;
                // Exercise room controls after the initial source-follow step.
                state.party.followed_generation = Some(1);
            }
            bridge.party_publish();
            assert!(app.get_party_can_control());
            assert!(app.get_party_can_choose_media());
            assert!(!app.get_party_is_host());
            for action in [
                PlaybackAction::TogglePause,
                PlaybackAction::SeekAbsolute(45.0),
                PlaybackAction::SeekRelative(-5.0),
            ] {
                bridge.dispatch_playback_action(action).unwrap();
            }
            {
                let mut state = bridge.shared.lock().unwrap();
                assert_eq!(state.party.pending.len(), 3);
                assert!(state.party.pending.iter().all(|operation| match operation {
                    Operation::Command(1, 7, _) => true,
                    Operation::Pause(intent) =>
                        intent.guard.generation == 1 && intent.control_revision == 7,
                    _ => false,
                }));
                state.party.pending.clear();
            }
            for operation in [
                Operation::Kick("host".into()),
                Operation::Wait(false),
                Operation::Continue,
                Operation::Policy(ControlPolicy::HostOnly),
            ] {
                bridge.party_enqueue(operation);
            }
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .control_policy = ControlPolicy::HostOnly;
            bridge
                .dispatch_playback_action(PlaybackAction::SeekAbsolute(99.0))
                .unwrap();
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
        },
    );
}

#[test]
fn guest_stream_preflight_blocks_empty_lobby_and_other_episode_before_mutating_picker() {
    isolated(
        concat!(
            module_path!(),
            "::guest_stream_preflight_blocks_empty_lobby_and_other_episode_before_mutating_picker"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            let mut local = room(PartyRole::Guest).media.unwrap();
            local.identity.media_type = "series".into();
            local.identity.episode_id = "tt123:1:31".into();
            local.season = Some(1);
            local.episode = Some(31);
            select_local_media(&bridge, &local);
            for empty in [true, false] {
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .media = (!empty).then(|| room(PartyRole::Guest).media.unwrap());
                app.set_stream_selector_open(false);
                let token = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .open_token
                    .clone();
                bridge.select_episode_streams(
                    "tt123".into(),
                    "tt123:1:31".into(),
                    Video {
                        id: "tt123:1:31".into(),
                        season: Some(1),
                        episode: Some(31),
                        ..Default::default()
                    },
                    0,
                );
                assert!(!app.get_stream_selector_open());
                let state = bridge.shared.lock().unwrap();
                assert!(state.playback.is_none());
                assert!(state.party.pending.is_empty());
                assert_eq!(state.modal_item.as_ref().unwrap().request_id, "tt123:1:31");
                assert!(Arc::ptr_eq(
                    &token,
                    &state.modal_item.as_ref().unwrap().open_token
                ));
                drop(state);
                assert!(!bridge.party_stream_lookup_allowed("tt123:1:31"));
                assert!(!bridge.begin_playback_request().is_current());
            }
        },
    );
}

#[test]
fn everyone_can_propose_any_episode_and_passive_lookup_never_proposes() {
    isolated(
        concat!(
            module_path!(),
            "::everyone_can_propose_any_episode_and_passive_lookup_never_proposes"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .control_policy = ControlPolicy::Everyone;
            let mut media = room(PartyRole::Guest).media.unwrap();
            media.identity.media_type = "series".into();
            media.identity.episode_id = "tt123:7:31".into();
            media.season = Some(7);
            media.episode = Some(31);
            select_local_media(&bridge, &media);
            assert!(bridge.party_stream_lookup_allowed("tt123:7:31"));
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert!(bridge.party_preflight_media(media.clone()));
            let state = bridge.shared.lock().unwrap();
            assert!(
                matches!(state.party.pending.front(), Some(Operation::PrepareMedia(1, 0, proposed)) if proposed.identity.episode_id == "tt123:7:31")
            );
            assert!(!state.party.selection.as_ref().unwrap().accepted);
            drop(state);
            assert!(
                !bridge.begin_playback_request().is_current(),
                "Only accepted pending media may load"
            );
        },
    );
}

#[test]
fn pending_source_guard_rejects_room_changes_aba_and_reopened_picker() {
    isolated(
        concat!(
            module_path!(),
            "::pending_source_guard_rejects_room_changes_aba_and_reopened_picker"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            let media = room(PartyRole::Guest).media.unwrap();
            select_local_media(&bridge, &media);
            let guard = bridge.party_playback_guard().unwrap().unwrap();
            let request = bridge.begin_playback_request();
            assert!(bridge.playback_request_is_current(&request));
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .generation += 2;
            assert!(
                !bridge.playback_request_is_current(&request),
                "Same identity in A-B-A is a different room generation"
            );
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.snapshot.as_mut().unwrap().generation = 1;
                state.party.snapshot.as_mut().unwrap().room_id = "another-room".into();
                assert!(!guard.accepts(&state.party));
                state.party.snapshot.as_mut().unwrap().room_id = "test-room".into();
                state.modal_item.as_mut().unwrap().open_token = Arc::new(());
            }
            assert!(
                bridge.party_playback_guard().is_err(),
                "A matching ID never owns a reopened picker"
            );
            bridge.party_leave();
            assert!(!guard.accepts(&bridge.shared.lock().unwrap().party));
        },
    );
}

#[test]
fn host_follows_guest_selection_once_and_matching_picker_keeps_its_owner() {
    isolated(
        concat!(
            module_path!(),
            "::host_follows_guest_selection_once_and_matching_picker_keeps_its_owner"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Host);
            let selected = room(PartyRole::Host);
            bridge.party_follow_selection(Some(&selected));
            assert!(app.get_stream_selector_open());
            let owner = bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .open_token
                .clone();
            bridge.party_follow_selection(Some(&selected));
            assert!(Arc::ptr_eq(
                &owner,
                &bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .open_token
            ));
            bridge.stream_selector_back();
            assert!(!app.get_stream_selector_open());
            bridge.party_follow_selection(Some(&selected));
            assert!(
                !app.get_stream_selector_open(),
                "Reports cannot reopen a dismissed picker"
            );
        },
    );
}

#[test]
fn creation_policy_uses_the_local_draft_and_joining_ignores_it() {
    isolated(
        concat!(
            module_path!(),
            "::creation_policy_uses_the_local_draft_and_joining_ignores_it"
        ),
        |Fixture { app, bridge }| {
            bridge.wire_party();
            assert_eq!(bridge.party_creation_policy(false), ControlPolicy::HostOnly);
            let settings = storage::get_str("settings");
            app.invoke_party_set_allow_everyone(true);
            assert!(app.get_party_allow_everyone());
            assert_eq!(bridge.party_creation_policy(false), ControlPolicy::Everyone);
            assert_eq!(
                bridge.party_creation_policy(true),
                ControlPolicy::HostOnly,
                "Joining never applies an unrelated local draft"
            );
            app.invoke_party_set_allow_everyone(false);
            assert!(!app.get_party_allow_everyone());
            assert_eq!(bridge.party_creation_policy(false), ControlPolicy::HostOnly);
            assert_eq!(
                storage::get_str("settings"),
                settings,
                "The draft is session-only"
            );
        },
    );
}

#[test]
fn policy_revocation_blocks_passive_lookup_before_clearing_retained_results() {
    isolated(
        concat!(
            module_path!(),
            "::policy_revocation_blocks_passive_lookup_before_clearing_retained_results"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            let mut other = room(PartyRole::Guest).media.unwrap();
            other.identity.title_id = "tt456".into();
            other.identity.episode_id = "tt456".into();
            select_local_media(&bridge, &other);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.snapshot.as_mut().unwrap().control_policy = ControlPolicy::Everyone;
            }
            assert!(bridge.party_stream_lookup_allowed("tt456"));
            let generation = bridge.shared.lock().unwrap().stream_generation;
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .control_policy = ControlPolicy::HostOnly;
            bridge.start_stream_search("tt456".into());
            assert_eq!(
                bridge.shared.lock().unwrap().stream_generation,
                generation,
                "Rejected lookup must not replace stream ownership or query addons"
            );
            assert!(
                bridge.shared.lock().unwrap().party.pending.is_empty(),
                "Passive lookup never proposes media"
            );
        },
    );
}

#[test]
fn rejected_media_proposals_can_retry_the_same_episode() {
    isolated(
        concat!(
            module_path!(),
            "::rejected_media_proposals_can_retry_the_same_episode"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Host);
            let mut next = room(PartyRole::Host).media.unwrap();
            next.identity.title_id = "tt456".into();
            next.identity.episode_id = "tt456".into();
            assert!(bridge.party_preflight_media(next.clone()));
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.pending.clear();
                state.party.reject_prepared_selection();
                assert!(state.party.selection.is_none());
            }
            assert!(bridge.party_preflight_media(next));
            assert!(matches!(
                bridge.shared.lock().unwrap().party.pending.front(),
                Some(Operation::PrepareMedia(1, 0, _))
            ));
            bridge.shared.lock().unwrap().party.pending.clear();
            select_local_media(&bridge, room(PartyRole::Host).media.as_ref().unwrap());
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .reject_prepared_selection();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .selection
                    .as_ref()
                    .unwrap()
                    .accepted,
                "A failure cannot erase an accepted selection"
            );
        },
    );
}

#[test]
fn proposal_acceptance_never_rebinds_a_source_across_an_aba_generation() {
    isolated(
        concat!(
            module_path!(),
            "::proposal_acceptance_never_rebinds_a_source_across_an_aba_generation"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Host);
            let mut next = room(PartyRole::Host).media.unwrap();
            next.identity.title_id = "tt456".into();
            next.identity.episode_id = "tt456".into();
            assert!(bridge.party_preflight_media(next.clone()));
            let mut accepted = room(PartyRole::Host);
            accepted.generation = 2;
            accepted.media = None;
            accepted.pending_media = Some(next);
            let state = bridge.shared.lock().unwrap();
            assert!(state.party.prepared_selection_matches(&accepted, 1));
            accepted.generation = 4;
            assert!(
                !state.party.prepared_selection_matches(&accepted, 1),
                "Matching media in A-B-A must not promote the original request to the later generation"
            );
            accepted.generation = 2;
            accepted.room_id = "replacement-room".into();
            assert!(!state.party.prepared_selection_matches(&accepted, 1));
        },
    );
}

#[test]
fn featured_title_stream_fallback_obeys_party_permissions_and_binds_its_picker() {
    isolated(
        concat!(
            module_path!(),
            "::featured_title_stream_fallback_obeys_party_permissions_and_binds_its_picker"
        ),
        |Fixture { app, bridge }| {
            for (role, policy, allowed) in [
                (PartyRole::Guest, ControlPolicy::HostOnly, false),
                (PartyRole::Guest, ControlPolicy::Everyone, true),
                (PartyRole::Host, ControlPolicy::HostOnly, true),
            ] {
                bridge.shared.lock().unwrap().party = State::default();
                let _service = install_room(&bridge, &app, role);
                {
                    let mut state = bridge.shared.lock().unwrap();
                    let room = state.party.snapshot.as_mut().unwrap();
                    room.media = None;
                    room.control_policy = policy;
                }
                let mut media = room(role).media.unwrap();
                media.identity.media_type = "series".into();
                select_local_media(&bridge, &media);
                {
                    let mut state = bridge.shared.lock().unwrap();
                    let modal = state.modal_item.as_mut().unwrap();
                    modal.seasons.clear();
                    modal.pending_watch_now = Some(WatchNowOrigin::Featured);
                }
                app.set_stream_selector_open(false);
                bridge.show_episode_picker("tt123".into(), Vec::new());
                assert_eq!(app.get_stream_selector_open(), allowed);
                let state = bridge.shared.lock().unwrap();
                if allowed {
                    let selection = state.party.selection.as_ref().unwrap();
                    assert!(
                        selection.owner.is_some(),
                        "The fallback source belongs to its actual picker opening"
                    );
                    assert!(
                        !selection.accepted,
                        "Wait for room acceptance before launching"
                    );
                    assert!(
                        matches!(state.party.pending.front(), Some(Operation::PrepareMedia(1, 0, media)) if media.identity.episode_id == "tt123")
                    );
                } else {
                    assert!(state.party.selection.is_none());
                    assert!(state.party.pending.is_empty());
                }
            }
        },
    );
}

#[test]
fn locked_profile_preflight_cannot_announce_or_replace_party_media() {
    isolated(
        concat!(
            module_path!(),
            "::locked_profile_preflight_cannot_announce_or_replace_party_media"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Host);
            let before = bridge.shared.lock().unwrap().party.snapshot.clone();
            let mut other = room(PartyRole::Host).media.unwrap();
            other.identity.title_id = "tt456".into();
            other.identity.episode_id = "tt456".into();
            profile_policy::set_entry_blocked(true);
            assert!(!bridge.party_preflight_media(other.clone()));
            assert!(!bridge.party_preflight_media_for_title(other, "movie", "provider:allowed"));
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.party.snapshot, before);
            assert!(state.party.selection.is_none());
            assert!(state.party.pending.is_empty());
            drop(state);
            profile_policy::set_entry_blocked(false);
        },
    );
}

#[test]
fn rejected_next_keeps_current_playback_or_follows_the_winning_episode() {
    isolated(
        concat!(
            module_path!(),
            "::rejected_next_keeps_current_playback_or_follows_the_winning_episode"
        ),
        |Fixture { app, bridge }| {
            for another_member_won in [false, true] {
                bridge.shared.lock().unwrap().party = State::default();
                let token = prepare_next_party(&bridge, &app, PartyRole::Guest);
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                    .unwrap();
                let latest = {
                    let mut state = bridge.shared.lock().unwrap();
                    state.party.pending.clear();
                    // This is the same rejection cleanup used by the coordinator
                    // after a nonterminal service reply. The room stays connected.
                    state.party.reject_prepared_selection();
                    let latest = state.party.snapshot.as_mut().unwrap();
                    if another_member_won {
                        latest.generation = 2;
                        let mut winner = latest.media.take().unwrap();
                        winner.identity.episode_id = "tt123:1:2".into();
                        winner.episode = Some(2);
                        winner.duration = 0.0;
                        latest.pending_media = Some(winner);
                    } else {
                        latest.control_policy = ControlPolicy::HostOnly;
                        latest.control_revision += 1;
                    }
                    latest.clone()
                };
                bridge.party_follow_selection(Some(&latest));
                assert!(bridge.party_active());
                assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
                if another_member_won {
                    assert!(!app.get_player_open());
                    assert_eq!(
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .modal_item
                            .as_ref()
                            .unwrap()
                            .request_id,
                        "tt123:1:2"
                    );
                    assert!(app.get_stream_selector_open());
                } else {
                    assert!(
                        app.get_player_open(),
                        "Permission rejection does not close the original player"
                    );
                    assert_eq!(
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .modal_item
                            .as_ref()
                            .unwrap()
                            .request_id,
                        "tt123:1:1"
                    );
                }
            }
        },
    );
}

#[test]
fn accepted_party_next_uses_the_exact_successor_across_seasons() {
    isolated(
        concat!(
            module_path!(),
            "::accepted_party_next_uses_the_exact_successor_across_seasons"
        ),
        |Fixture { app, bridge }| {
            prepare_next_party(&bridge, &app, PartyRole::Host);
            {
                let mut state = bridge.shared.lock().unwrap();
                let modal = state.modal_item.as_mut().unwrap();
                modal.videos.retain(|video| video.id == "tt123:1:1");
                modal.videos.push(Video {
                    id: "tt123:2:1".into(),
                    season: Some(2),
                    episode: Some(1),
                    ..Default::default()
                });
                modal.seasons = vec![1, 2];
            }
            bridge.refresh_next_episode_prompt();
            let token = app.get_next_episode_token().to_string();
            assert!(token.ends_with(":tt123:2:1"));
            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                .unwrap();
            assert!(app.get_player_open());
            let (choice, guard) = accept_next_party(&bridge);
            bridge.party_complete_next(choice, guard);
            assert!(!app.get_player_open());
            assert!(app.get_stream_selector_open());
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.modal_item.as_ref().unwrap().request_id, "tt123:2:1");
            assert_eq!(state.modal_item.as_ref().unwrap().season_index, 1);
            assert!(state.party.pending.is_empty());
        },
    );
}

#[test]
fn old_next_offer_cannot_rebind_its_playing_source_after_room_aba() {
    isolated(
        concat!(
            module_path!(),
            "::old_next_offer_cannot_rebind_its_playing_source_after_room_aba"
        ),
        |Fixture { app, bridge }| {
            let token = prepare_next_party(&bridge, &app, PartyRole::Host);
            // Same title, same episode and same local banner token, but a later
            // room selection owns the media. The old source captured generation 1.
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .generation = 3;
            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                .unwrap();
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert!(app.get_player_open());
        },
    );
}

#[test]
fn shared_speed_callbacks_use_room_revision_and_preserve_local_preferences() {
    isolated(
        concat!(
            module_path!(),
            "::shared_speed_callbacks_use_room_revision_and_preserve_local_preferences"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.cache_settings.playback_speed = 1.2;
                state.party.choosing_media = true;
                let room = state.party.snapshot.as_mut().unwrap();
                room.media = None;
                room.control_policy = ControlPolicy::Everyone;
                room.control_revision = 9;
                room.generation = 7;
                room.timeline.target_rate = 1.5;
            }
            bridge.player.set_title_speed(1.4);
            bridge.player.set_party_playback_rate(Some(1.545));
            let local =
                serde_json::to_value(&bridge.shared.lock().unwrap().cache_settings).unwrap();
            let saved = storage::get_str(playback_preferences::PREFERENCES_KEY);
            bridge.wire_party();
            bridge.party_publish();
            assert_eq!(app.get_party_target_speed(), 1.5);
            assert!(
                app.get_party_status().is_empty(),
                "Everyone can choose in an empty lobby"
            );
            app.invoke_party_set_target_speed(1.25);
            app.invoke_party_step_target_speed(1);
            app.invoke_party_step_target_speed(-1);
            app.invoke_party_reset_target_speed();
            for action in [
                PlaybackAction::SetSpeed(1.75),
                PlaybackAction::SpeedStep(1),
                PlaybackAction::ResetSpeed,
                PlaybackAction::BeginSpeedPreview,
                PlaybackAction::EndSpeedPreview,
            ] {
                bridge.dispatch_playback_action(action).unwrap();
            }
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                state.party.pending.len(),
                7,
                "Hold preview neither edits nor resets room speed"
            );
            assert!(
                state
                    .party
                    .pending
                    .iter()
                    .all(|operation| matches!(operation, Operation::Command(7, 9, _)))
            );
            assert!(matches!(
                state.party.pending[0],
                Operation::Command(7, 9, HostCommand::SetRate(1.25))
            ));
            assert!(matches!(
                state.party.pending[1],
                Operation::Command(7, 9, HostCommand::StepRate(1))
            ));
            assert!(matches!(
                state.party.pending[2],
                Operation::Command(7, 9, HostCommand::StepRate(-1))
            ));
            assert!(matches!(
                state.party.pending[3],
                Operation::Command(7, 9, HostCommand::ResetRate)
            ));
            assert!(matches!(
                state.party.pending[4],
                Operation::Command(7, 9, HostCommand::SetRate(1.75))
            ));
            assert!(
                matches!(
                    state.party.pending[5],
                    Operation::Command(7, 9, HostCommand::StepRate(1))
                ),
                "Steps must reach the host as direction, not a value derived from 1.545x correction"
            );
            assert!(matches!(
                state.party.pending[6],
                Operation::Command(7, 9, HostCommand::ResetRate)
            ));
            assert_eq!(serde_json::to_value(&state.cache_settings).unwrap(), local);
            drop(state);
            assert_eq!(
                storage::get_str(playback_preferences::PREFERENCES_KEY),
                saved
            );
            assert!(
                (bridge.player.effective_playback_speed() - 1.545).abs() < 0.0001,
                "Local native rate changes only after the accepted room target arrives"
            );
        },
    );
}

#[test]
fn host_only_guests_cannot_edit_shared_speed_or_local_speed_preferences() {
    isolated(
        concat!(
            module_path!(),
            "::host_only_guests_cannot_edit_shared_speed_or_local_speed_preferences"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            bridge.player.set_title_speed(1.4);
            bridge.player.set_party_playback_rate(Some(1.03));
            let local =
                serde_json::to_value(&bridge.shared.lock().unwrap().cache_settings).unwrap();
            let saved = storage::get_str(playback_preferences::PREFERENCES_KEY);
            bridge.wire_party();
            app.invoke_party_set_target_speed(2.0);
            app.invoke_party_step_target_speed(1);
            app.invoke_party_reset_target_speed();
            for action in [
                PlaybackAction::SetSpeed(0.5),
                PlaybackAction::SpeedStep(-1),
                PlaybackAction::ResetSpeed,
            ] {
                bridge.dispatch_playback_action(action).unwrap();
            }
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert!(!app.get_party_error().is_empty());
            assert!((bridge.player.effective_playback_speed() - 1.03).abs() < 0.0001);
            assert_eq!(
                serde_json::to_value(&bridge.shared.lock().unwrap().cache_settings).unwrap(),
                local
            );
            assert_eq!(
                storage::get_str(playback_preferences::PREFERENCES_KEY),
                saved
            );
            bridge.party_leave();
            assert!(
                (bridge.player.effective_playback_speed() - 1.4).abs() < 0.0001,
                "Leave restores the saved per-title baseline"
            );
        },
    );
}

#[test]
fn shared_speed_presentation_stays_on_target_during_native_drift_correction() {
    isolated(
        concat!(
            module_path!(),
            "::shared_speed_presentation_stays_on_target_during_native_drift_correction"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Host);
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .timeline
                .target_rate = 1.5;
            bridge.player.set_title_speed(1.4);
            for actual in [1.455, 1.5, 1.545] {
                bridge.player.set_party_playback_rate(Some(actual));
                bridge.party_publish();
                assert_eq!(app.get_party_target_speed(), 1.5);
                assert!(
                    (f64::from(bridge.player.effective_playback_speed()) - actual).abs() < 0.0001
                );
            }
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .timeline
                .target_rate = 2.0;
            bridge.party_publish();
            assert_eq!(app.get_party_target_speed(), 2.0);
            bridge.party_leave();
            assert_eq!(app.get_party_target_speed(), 1.0);
            assert!((bridge.player.effective_playback_speed() - 1.4).abs() < 0.0001);
        },
    );
}

#[test]
fn absolute_rate_queue_keeps_the_latest_value_without_crossing_command_barriers() {
    let mut state = State::default();
    for index in 0..120 {
        assert!(state.push_operation(Operation::Command(
            1,
            3,
            HostCommand::SetRate(0.5 + (index % 31) as f64 * 0.05)
        )));
    }
    assert!(state.push_operation(Operation::Command(1, 3, HostCommand::SetRate(2.0))));
    assert_eq!(state.pending.len(), 1);
    assert!(matches!(
        state.pending[0],
        Operation::Command(1, 3, HostCommand::SetRate(2.0))
    ));
    for barrier in [
        Operation::Command(1, 3, HostCommand::StepRate(1)),
        Operation::Command(1, 3, HostCommand::ResetRate),
        Operation::Command(1, 3, HostCommand::SeekAbsolute(20.0)),
        Operation::PrepareMedia(1, 3, room(PartyRole::Guest).media.unwrap()),
    ] {
        state.pending.clear();
        state.push_operation(Operation::Command(1, 3, HostCommand::SetRate(1.5)));
        state.push_operation(barrier);
        state.push_operation(Operation::Command(1, 3, HostCommand::SetRate(2.0)));
        assert_eq!(
            state.pending.len(),
            3,
            "Absolute moves never absorb a step, reset, seek or selection"
        );
    }
    state.pending.clear();
    for _ in 0..31 {
        state.push_operation(Operation::Command(1, 3, HostCommand::StepRate(1)));
    }
    state.push_operation(Operation::Command(1, 3, HostCommand::SetRate(1.5)));
    assert!(
        state.push_operation(Operation::Command(1, 3, HostCommand::SetRate(2.0))),
        "A full queue must still retain the final adjacent slider choice"
    );
    assert_eq!(state.pending.len(), 32);
    assert!(matches!(
        state.pending.back(),
        Some(Operation::Command(1, 3, HostCommand::SetRate(2.0)))
    ));
    assert!(
        !state.push_operation(Operation::Command(2, 3, HostCommand::SetRate(0.5))),
        "Different generations cannot coalesce"
    );
    assert!(
        !state.push_operation(Operation::Command(1, 4, HostCommand::SetRate(0.5))),
        "Different permissions cannot coalesce"
    );
}

#[test]
fn guest_controls_wait_after_a_response_but_reports_and_host_controls_do_not() {
    let now = Instant::now();
    let mut state = State {
        snapshot: Some(room(PartyRole::Guest)),
        last_guest_control_finished: Some(now),
        ..Default::default()
    };
    state
        .pending
        .push_back(Operation::Command(1, 0, HostCommand::SetRate(2.0)));
    assert_eq!(
        state.guest_control_wait(now),
        Some(Duration::from_millis(50))
    );
    assert_eq!(
        state.guest_control_wait(now + Duration::from_millis(49)),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        state.guest_control_wait(now + Duration::from_millis(50)),
        None
    );
    // A completed rejection consumes the same host limiter budget; another
    // response restarts the interval rather than letting the final value race.
    state.last_guest_control_finished = Some(now + Duration::from_millis(60));
    assert_eq!(
        state.guest_control_wait(now + Duration::from_millis(70)),
        Some(Duration::from_millis(40))
    );
    state.pending.clear();
    state.pending.push_back(Operation::PrepareMedia(
        1,
        0,
        room(PartyRole::Guest).media.unwrap(),
    ));
    assert!(
        state
            .guest_control_wait(now + Duration::from_millis(70))
            .is_some()
    );
    state
        .pending
        .push_front(Operation::Report(LocalReport::default()));
    assert_eq!(
        state.guest_control_wait(now + Duration::from_millis(70)),
        None,
        "Heartbeat reports are never throttled with controls"
    );
    state.pending.pop_front();
    state.snapshot.as_mut().unwrap().role = PartyRole::Host;
    assert_eq!(
        state.guest_control_wait(now + Duration::from_millis(70)),
        None
    );
}

#[test]
fn control_wakeup_is_bound_to_room_generation_and_party_activation() {
    let mut state = State {
        snapshot: Some(room(PartyRole::Guest)),
        generation: 4,
        busy: true,
        ..Default::default()
    };
    let wakeup = ControlWakeup {
        activation: 4,
        room_id: "test-room".into(),
        generation: 1,
    };
    assert!(wakeup.current(&state));
    state.snapshot.as_mut().unwrap().generation = 3;
    assert!(
        !wakeup.current(&state),
        "A-B-A is a different wakeup generation"
    );
    state.snapshot.as_mut().unwrap().generation = 1;
    state.generation = 5;
    assert!(
        !wakeup.current(&state),
        "Leaving and rejoining invalidates old timers"
    );
    state.generation = 4;
    state.snapshot.as_mut().unwrap().room_id = "another-room".into();
    assert!(!wakeup.current(&state));
}

#[test]
fn slider_callback_burst_preserves_its_final_rate_and_step_reset_order() {
    isolated(
        concat!(
            module_path!(),
            "::slider_callback_burst_preserves_its_final_rate_and_step_reset_order"
        ),
        |Fixture { app, bridge }| {
            let _service = install_room(&bridge, &app, PartyRole::Guest);
            {
                let mut state = bridge.shared.lock().unwrap();
                let room = state.party.snapshot.as_mut().unwrap();
                room.media = None;
                room.control_policy = ControlPolicy::Everyone;
                room.control_revision = 7;
            }
            bridge.wire_party();
            for index in 0..120 {
                app.invoke_party_set_target_speed(0.5 + (index % 31) as f32 * 0.05);
            }
            app.invoke_party_set_target_speed(2.0);
            assert_eq!(bridge.shared.lock().unwrap().party.pending.len(), 1);
            assert!(matches!(
                bridge.shared.lock().unwrap().party.pending.front(),
                Some(Operation::Command(1, 7, HostCommand::SetRate(2.0)))
            ));
            app.invoke_party_step_target_speed(1);
            app.invoke_party_set_target_speed(0.5);
            app.invoke_party_reset_target_speed();
            app.invoke_party_set_target_speed(1.25);
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.party.pending.len(), 5);
            assert!(matches!(
                state.party.pending[1],
                Operation::Command(1, 7, HostCommand::StepRate(1))
            ));
            assert!(matches!(
                state.party.pending[3],
                Operation::Command(1, 7, HostCommand::ResetRate)
            ));
            assert!(matches!(
                state.party.pending[4],
                Operation::Command(1, 7, HostCommand::SetRate(1.25))
            ));
        },
    );
}

#[test]
fn slow_reports_allow_one_queued_control_before_the_next_overdue_heartbeat() {
    let start = Instant::now();
    let mut state = State::default();
    state.pending.extend([
        Operation::Command(1, 0, HostCommand::SetRate(1.5)),
        Operation::PrepareMedia(1, 0, room(PartyRole::Guest).media.unwrap()),
    ]);
    let dispatch = |state: &mut State, at: Instant| {
        let operation = if state.report_should_run(at) {
            Operation::Report(LocalReport::default())
        } else {
            state.pending.pop_front().expect("queued work")
        };
        state.note_operation_dispatched(&operation, at);
        operation
    };
    assert!(matches!(dispatch(&mut state, start), Operation::Report(_)));
    // Each report takes 600ms, exceeding the 500ms dispatch deadline.
    assert!(matches!(
        dispatch(&mut state, start + Duration::from_millis(600)),
        Operation::Command(1, 0, HostCommand::SetRate(1.5))
    ));
    assert!(matches!(
        dispatch(&mut state, start + Duration::from_millis(601)),
        Operation::Report(_)
    ));
    assert!(matches!(
        dispatch(&mut state, start + Duration::from_millis(1201)),
        Operation::PrepareMedia(1, 0, _)
    ));
    assert!(matches!(
        dispatch(&mut state, start + Duration::from_millis(1202)),
        Operation::Report(_)
    ));
    assert!(state.pending.is_empty());
    assert!(
        matches!(
            dispatch(&mut state, start + Duration::from_millis(1802)),
            Operation::Report(_)
        ),
        "Idle overdue heartbeats continue without waiting for a control"
    );
    assert!(!state.report_should_run(start + Duration::from_millis(2301)));
    state
        .pending
        .push_back(Operation::Command(1, 0, HostCommand::StepRate(1)));
    assert!(matches!(
        dispatch(&mut state, start + Duration::from_millis(2301)),
        Operation::Command(..)
    ));
    assert!(
        state.report_should_run(start + Duration::from_millis(2302)),
        "A control burst cannot displace the next deadline"
    );
}

fn prepare_pause_party(bridge: &Bridge, app: &AppWindow, role: PartyRole) {
    let _service = install_room(bridge, app, role);
    if role == PartyRole::Guest {
        bridge
            .shared
            .lock()
            .unwrap()
            .party
            .snapshot
            .as_mut()
            .unwrap()
            .control_policy = ControlPolicy::Everyone;
    }
    select_local_media(bridge, room(role).media.as_ref().unwrap());
    assert!(bridge.begin_playback_request().is_current());
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(true);
    bridge.party_publish();
}

fn take_pause(bridge: &Bridge) -> PauseIntent {
    match bridge
        .shared
        .lock()
        .unwrap()
        .party
        .pending
        .pop_front()
        .unwrap()
    {
        Operation::Pause(intent) => intent,
        _ => panic!("expected a source-owned pause request"),
    }
}

#[test]
fn rapid_party_toggles_project_the_latest_intent_before_acknowledgement() {
    isolated(
        concat!(
            module_path!(),
            "::rapid_party_toggles_project_the_latest_intent_before_acknowledgement"
        ),
        |Fixture { app, bridge }| {
            for role in [PartyRole::Host, PartyRole::Guest] {
                bridge.shared.lock().unwrap().party = State::default();
                prepare_pause_party(&bridge, &app, role);
                bridge
                    .dispatch_playback_action(PlaybackAction::TogglePause)
                    .unwrap();
                assert!(
                    !app.get_party_paused(),
                    "Accepted local resume updates transport immediately"
                );
                assert!(
                    app.get_is_paused(),
                    "Native playback remains held for the room clock"
                );
                let first = take_pause(&bridge);
                assert!(!first.paused);
                bridge
                    .dispatch_playback_action(PlaybackAction::TogglePause)
                    .unwrap();
                assert!(app.get_party_paused());
                let second_request = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .pending_pause
                    .as_ref()
                    .unwrap()
                    .request;
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.party.snapshot.as_mut().unwrap().timeline.paused = false;
                    state.party.finish_pause(&first);
                    assert_eq!(
                        state.party.pending_pause.as_ref().unwrap().request,
                        second_request,
                        "Old ACK cannot consume the newer pause"
                    );
                }
                bridge.party_publish_pause();
                assert!(
                    app.get_party_paused(),
                    "An older resume ACK cannot replace a newer pause icon"
                );
                bridge
                    .dispatch_playback_action(PlaybackAction::TogglePause)
                    .unwrap();
                assert!(!app.get_party_paused());
                assert_eq!(
                    bridge.shared.lock().unwrap().party.pending.len(),
                    1,
                    "Adjacent queued toggles keep only the final state"
                );
                let latest = take_pause(&bridge);
                assert!(latest.request > second_request);
                assert!(!latest.paused);
                {
                    let mut state = bridge.shared.lock().unwrap();
                    let room = state.party.snapshot.as_mut().unwrap();
                    room.timeline.paused = false;
                    room.timeline.anchor_ms =
                        nova_party::local_now_ms() + nova_party::START_LEAD_MS;
                    state.party.finish_pause(&latest);
                }
                bridge.party_publish_pause();
                assert!(
                    !app.get_party_paused(),
                    "Accepted scheduled resume uses the logical timeline"
                );
                assert!(
                    app.get_is_paused(),
                    "The icon does not wait for native scheduled unpause"
                );
                assert!(bridge.shared.lock().unwrap().party.pending_pause.is_none());
            }
        },
    );
}

#[test]
fn pause_rejections_roll_back_only_the_latest_matching_request() {
    isolated(
        concat!(
            module_path!(),
            "::pause_rejections_roll_back_only_the_latest_matching_request"
        ),
        |Fixture { app, bridge }| {
            prepare_pause_party(&bridge, &app, PartyRole::Guest);
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let rejected = take_pause(&bridge);
            bridge.shared.lock().unwrap().party.finish_pause(&rejected);
            bridge.party_publish_pause();
            assert!(
                app.get_party_paused(),
                "Rejected resume restores confirmed pause"
            );
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let older = take_pause(&bridge);
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let request = bridge
                .shared
                .lock()
                .unwrap()
                .party
                .pending_pause
                .as_ref()
                .unwrap()
                .request;
            bridge.shared.lock().unwrap().party.finish_pause(&older);
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .pending_pause
                    .as_ref()
                    .unwrap()
                    .request,
                request,
                "An older rejection cannot erase newer queued intent"
            );
            let latest = take_pause(&bridge);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.snapshot.as_mut().unwrap().timeline.paused = false;
                state.party.finish_pause(&latest);
            }
            bridge.party_publish_pause();
            assert!(!app.get_party_paused());
            assert!(bridge.shared.lock().unwrap().party.pending_pause.is_none());
        },
    );
}

#[test]
fn pause_projection_expires_on_authority_room_and_source_changes_even_while_busy() {
    isolated(
        concat!(
            module_path!(),
            "::pause_projection_expires_on_authority_room_and_source_changes_even_while_busy"
        ),
        |Fixture { app, bridge }| {
            for change in [
                "generation",
                "room",
                "revision",
                "policy",
                "disconnected",
                "ended",
                "identity",
                "selection",
                "source",
                "leave",
            ] {
                bridge.shared.lock().unwrap().party = State::default();
                prepare_pause_party(&bridge, &app, PartyRole::Guest);
                bridge
                    .dispatch_playback_action(PlaybackAction::TogglePause)
                    .unwrap();
                assert!(!app.get_party_paused());
                let owner = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .pending_pause
                    .as_ref()
                    .unwrap()
                    .clone();
                if change == "source" {
                    bridge.player.close();
                } else if change == "leave" {
                    bridge.party_leave();
                } else {
                    let mut state = bridge.shared.lock().unwrap();
                    if change == "selection" {
                        state.party.selection_revision += 1;
                    } else {
                        let room = state.party.snapshot.as_mut().unwrap();
                        match change {
                            "generation" => room.generation += 2,
                            "room" => room.room_id = "replacement-room".into(),
                            "revision" => room.control_revision += 1,
                            "policy" => room.control_policy = ControlPolicy::HostOnly,
                            "disconnected" => room.connected = false,
                            "ended" => room.ended = true,
                            "identity" => {
                                room.media.as_mut().unwrap().identity.title_id = "tt456".into()
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                bridge.party_publish_pause();
                assert!(
                    app.get_party_paused(),
                    "{change}: stale optimistic resume must disappear"
                );
                {
                    let mut state = bridge.shared.lock().unwrap();
                    assert!(state.party.pending_pause.is_none(), "{change}");
                    assert!(
                        !state
                            .party
                            .pending
                            .iter()
                            .any(|operation| matches!(operation, Operation::Pause(_))),
                        "{change}: stale queued transport must not be sent"
                    );
                    state.party.finish_pause(&owner);
                    assert!(
                        state.party.pending_pause.is_none(),
                        "{change}: late ACK cannot revive the projection"
                    );
                }
                if matches!(change, "policy" | "disconnected" | "ended" | "leave") {
                    assert!(!app.get_party_can_control(), "{change}");
                }
            }
        },
    );
}

#[test]
fn pause_queue_coalesces_only_adjacent_owners_and_never_projects_dropped_requests() {
    isolated(
        concat!(
            module_path!(),
            "::pause_queue_coalesces_only_adjacent_owners_and_never_projects_dropped_requests"
        ),
        |Fixture { app, bridge }| {
            prepare_pause_party(&bridge, &app, PartyRole::Host);
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let first = take_pause(&bridge);
            for barrier in [
                Operation::Command(1, 0, HostCommand::SeekAbsolute(30.0)),
                Operation::Command(1, 0, HostCommand::SetRate(1.5)),
                Operation::PrepareMedia(1, 0, room(PartyRole::Host).media.unwrap()),
            ] {
                let mut later = first.clone();
                later.request += 1;
                later.paused = true;
                let mut state = bridge.shared.lock().unwrap();
                state.party.pending.clear();
                state.party.push_operation(Operation::Pause(first.clone()));
                state.party.push_operation(barrier);
                state.party.push_operation(Operation::Pause(later));
                assert_eq!(state.party.pending.len(), 3);
            }
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.pending.clear();
                for _ in 0..31 {
                    state
                        .party
                        .push_operation(Operation::Command(1, 0, HostCommand::StepRate(1)));
                }
                state.party.push_operation(Operation::Pause(first.clone()));
                state.party.pending_pause = Some(first);
            }
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            assert!(app.get_party_paused());
            assert_eq!(
                bridge.shared.lock().unwrap().party.pending.len(),
                32,
                "A full queue still accepts its adjacent final pause state"
            );
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.pending.clear();
                state.party.pending_pause = None;
                for _ in 0..32 {
                    state
                        .party
                        .push_operation(Operation::Command(1, 0, HostCommand::StepRate(1)));
                }
            }
            bridge.party_publish_pause();
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            assert!(
                app.get_party_paused(),
                "Queue rejection cannot claim immediate acceptance"
            );
            assert!(bridge.shared.lock().unwrap().party.pending_pause.is_none());
            assert!(!app.get_party_error().is_empty());
        },
    );
}

#[test]
fn suspended_pause_owner_stays_retired_after_foreground_returns() {
    isolated(
        concat!(
            module_path!(),
            "::suspended_pause_owner_stays_retired_after_foreground_returns"
        ),
        |Fixture { app, bridge }| {
            prepare_pause_party(&bridge, &app, PartyRole::Host);
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let popped = take_pause(&bridge);
            remote_playback::playback_foreground(false);
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .invalidate_pause_intents();
            remote_playback::playback_foreground(true);
            {
                let state = bridge.shared.lock().unwrap();
                let party = &state.party;
                let room = party.snapshot_for_sources();
                assert!(
                    !popped.current(
                        party.generation,
                        party.selection_revision,
                        bridge.player.screenshot_session(),
                        party.pause_epoch,
                        room.as_ref()
                    ),
                    "An old worker cannot replay a resume after returning to the same source and room"
                );
            }
            bridge.party_publish_pause();
            assert!(app.get_party_paused());
            bridge
                .dispatch_playback_action(PlaybackAction::TogglePause)
                .unwrap();
            let fresh = take_pause(&bridge);
            assert!(!fresh.same_scope(&popped));
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.finish_pause(&popped);
                assert_eq!(
                    state.party.pending_pause.as_ref().unwrap().request,
                    fresh.request,
                    "An old interval's ACK cannot consume a fresh resume"
                );
            }
        },
    );
}

#[test]
#[cfg(all(feature = "desktop", any(target_os = "linux", target_os = "windows")))]
fn system_media_party_controls_follow_shared_authority_without_saving_personal_speed() {
    use nova_player::media_controls::Command;
    isolated(
        concat!(
            module_path!(),
            "::system_media_party_controls_follow_shared_authority_without_saving_personal_speed"
        ),
        |Fixture { app, bridge }| {
            for role in [PartyRole::Host, PartyRole::Guest] {
                bridge.shared.lock().unwrap().party = State::default();
                prepare_pause_party(&bridge, &app, role);
                bridge.mark_playback_opened(true);
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .timeline
                    .target_rate = 1.55;
                let saved = storage::get_str(playback_preferences::PREFERENCES_KEY);
                bridge
                    .dispatch_playback_action(PlaybackAction::TogglePause)
                    .unwrap();
                let snapshot = bridge.system_media_snapshot();
                assert!(snapshot.can_play);
                assert!(
                    !snapshot.paused,
                    "OS transport follows accepted local pause intent while native playback is held"
                );
                assert!(app.get_is_paused());
                assert_eq!(
                    snapshot.rate, 1.55,
                    "OS rate is the accepted target, independent of drift correction"
                );
                bridge.handle_system_media(snapshot.owner.clone(), Command::SetRate(1.75));
                assert!(bridge.shared.lock().unwrap().party.pending.iter().any(|operation| matches!(operation, Operation::Command(1, 0, HostCommand::SetRate(rate)) if *rate == 1.75)));
                assert_eq!(
                    storage::get_str(playback_preferences::PREFERENCES_KEY),
                    saved
                );
                let queued = bridge.shared.lock().unwrap().party.pending.len();
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .control_revision += 1;
                bridge.handle_system_media(snapshot.owner.clone(), Command::TogglePause);
                assert_eq!(
                    bridge.shared.lock().unwrap().party.pending.len(),
                    queued,
                    "An old policy revision cannot become a fresh toggle"
                );
                if role == PartyRole::Guest {
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .party
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .control_policy = ControlPolicy::HostOnly;
                    app.set_party_can_control(false);
                    let readonly = bridge.system_media_snapshot();
                    assert!(!readonly.can_play);
                    assert!(!readonly.can_seek);
                    bridge.handle_system_media(readonly.owner.clone(), Command::SetRate(1.0));
                    assert_eq!(bridge.shared.lock().unwrap().party.pending.len(), queued);
                    bridge.handle_system_media(readonly.owner, Command::Stop);
                    assert!(!app.get_player_open());
                    assert!(bridge.party_active());
                    assert!(
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .party
                            .snapshot
                            .as_ref()
                            .unwrap()
                            .media
                            .is_some(),
                        "Guest Stop closes only its own source"
                    );
                }
            }
        },
    );
}

#[test]
fn early_footer_next_uses_the_same_host_and_shared_guest_transaction() {
    isolated(
        concat!(
            module_path!(),
            "::early_footer_next_uses_the_same_host_and_shared_guest_transaction"
        ),
        |Fixture { app, bridge }| {
            for role in [PartyRole::Host, PartyRole::Guest] {
                bridge.shared.lock().unwrap().party = State::default();
                prepare_next_party(&bridge, &app, role);
                bridge.mark_playback_opened(true);
                app.set_position(10.0);
                bridge.refresh_next_episode_prompt();
                assert!(!app.get_next_episode_visible());
                assert!(app.get_next_episode_button_available());
                let token = app.get_next_episode_button_token().to_string();
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode {
                        token: token.clone(),
                    })
                    .unwrap();
                assert!(
                    app.get_player_open(),
                    "Even early footer requests wait for room acceptance"
                );
                assert!(app.get_party_busy());
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode {
                        token: token.clone(),
                    })
                    .unwrap();
                assert_eq!(bridge.shared.lock().unwrap().party.pending.len(), 1);
                let (choice, guard) = accept_next_party(&bridge);
                bridge.party_complete_next(choice, guard);
                assert!(!app.get_player_open());
                assert!(app.get_stream_selector_open());
                assert!(!app.get_auto_next_preview());
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .unwrap()
                        .request_id,
                    "tt123:1:2"
                );
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .party
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .generation,
                    2
                );
                bridge
                    .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                    .ok();
                assert!(
                    !bridge
                        .shared
                        .lock()
                        .unwrap()
                        .party
                        .pending
                        .iter()
                        .any(|operation| matches!(
                            operation,
                            Operation::PrepareMedia(..) | Operation::ClearMedia { .. }
                        )),
                    "An old footer cannot replace or clear the accepted successor"
                );
            }
            bridge.shared.lock().unwrap().party = State::default();
            prepare_next_party(&bridge, &app, PartyRole::Guest);
            bridge.mark_playback_opened(true);
            bridge
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_mut()
                .unwrap()
                .control_policy = ControlPolicy::HostOnly;
            bridge.party_publish();
            app.set_position(10.0);
            bridge.refresh_next_episode_prompt();
            assert!(app.get_next_episode_button_available());
            assert!(!app.get_party_can_control());
            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode {
                    token: app.get_next_episode_button_token().to_string(),
                })
                .unwrap();
            assert!(app.get_player_open());
            assert!(bridge.shared.lock().unwrap().party.pending.is_empty());
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .generation,
                1,
                "Footer availability never grants HostOnly guests shared authority"
            );
        },
    );
}

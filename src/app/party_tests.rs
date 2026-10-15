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

                    // Arming the load is observable even on a headless backend
                    // without mpv; no decoder or network is needed here.
                    let _ = bridge.open_player("https://example.invalid/party".into());

                    assert!(!app.get_resume_prompt_visible());
                    let state = bridge.shared.lock().unwrap();
                    assert!(state.pending_resume_stream.is_none());
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
            assert!(app.get_discover_search_open());
            assert_eq!(app.get_search_text(), "Example Movie");
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
                state
                    .party
                    .pending
                    .push_back(Operation::Command(1, HostCommand::SeekAbsolute(80.0)));
                state.party.generation
            };
            app.set_party_busy(true);
            app.set_party_waiting(true);
            app.set_party_invite("temporary-invite".into());

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
                        "Choose what to watch"
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
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(120.0);
            select_local_media(&bridge, room(PartyRole::Host).media.as_ref().unwrap());
            let _request = bridge.begin_playback_request();
            let epoch = bridge.player.screenshot_session();
            let (generation, snapshot) =
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.party.choosing_source = true;
                    state.party.snapshot.as_mut().unwrap().participants.push(
                        nova_party::Participant {
                            id: "guest".into(),
                            name: "Guest device".into(),
                            is_host: false,
                            connected: true,
                            ready: true,
                            buffering: false,
                            position: 20.0,
                            buffered_ahead: 60.0,
                            reason: None,
                        },
                    );
                    state.party.pending.extend([
                        Operation::Command(1, HostCommand::SeekAbsolute(80.0)),
                        Operation::Report(LocalReport::default()),
                        Operation::Continue,
                        Operation::Kick("guest".into()),
                        Operation::Wait(false),
                    ]);
                    (state.party.generation, state.party.snapshot.clone())
                };

            bridge.party_prepare_media_change(true);

            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert_eq!(state.party.generation, generation);
            assert_eq!(state.party.snapshot, snapshot);
            assert!(state.party.choosing_media);
            assert!(!state.party.choosing_source);
            assert_eq!(state.party.selection_epoch, epoch);
            assert_eq!(state.party.pending.len(), 3);
            assert!(matches!(
                state.party.pending[0],
                Operation::ClearMedia { browse: true }
            ));
            assert!(matches!(state.party.pending[1], Operation::Kick(_)));
            assert!(matches!(state.party.pending[2], Operation::Wait(false)));
            drop(state);
            assert_eq!(app.get_party_invite(), "same-invite");
            assert!(app.get_party_active());
            assert!(app.get_party_busy());
            assert!(app.get_is_paused());
            assert_eq!(bridge.party_start_position(), Some(0.0));

            // Successful ClearMedia completion uses this browse path. It
            // starts unfiltered discovery instead of searching the old title.
            app.set_show_downloads(true);
            app.set_show_calendar(true);
            bridge.party_browse(None);
            assert!(!app.get_show_downloads());
            assert!(!app.get_show_calendar());
            assert!(!app.get_more_open());
            assert!(!app.get_player_open());
            assert!(app.get_search_text().is_empty());
            assert!(!app.get_discover_search_open());
            assert!(bridge.party_active());
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
            assert_eq!(app.get_search_text(), "Example Movie");
            assert!(app.get_discover_search_open());
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

#[test]
fn host_next_episode_reaches_the_source_picker_without_leaving_the_room() {
    isolated(
        concat!(
            module_path!(),
            "::host_next_episode_reaches_the_source_picker_without_leaving_the_room"
        ),
        |Fixture { app, bridge }| {
            let service = install_room(&bridge, &app, PartyRole::Host);
            let mut media = room(PartyRole::Host).media.unwrap();
            media.identity.media_type = "series".into();
            media.identity.episode_id = "tt123:1:1".into();
            media.identity.episode = "S1 E1".into();
            media.season = Some(1);
            media.episode = Some(1);
            media.duration = 1440.0;
            select_local_media(&bridge, &media);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.party.snapshot.as_mut().unwrap().media = Some(media);
                state.modal_item.as_mut().unwrap().videos = (1..=2)
                    .map(|episode| Video {
                        id: format!("tt123:1:{episode}"),
                        season: Some(1),
                        episode: Some(episode),
                        ..Default::default()
                    })
                    .collect();
                state.cache_settings.prefetch_next_episode_streams = false;
            }
            let request = bridge.begin_playback_request();
            let session = crate::app::next_episode::new_session();
            bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
                session,
                series_id: "tt123".into(),
                episode_id: "tt123:1:1".into(),
                ..Default::default()
            });
            // Use the production close route: the nested Stop opens the room
            // before Next continues into the following episode's source picker.
            let close_bridge = bridge.clone();
            app.on_close_player(move || {
                close_bridge
                    .dispatch_playback_action(PlaybackAction::Stop)
                    .unwrap();
            });
            app.set_modal_visible(true);
            app.set_detail_is_movie(false);
            app.set_party_page_open(false);
            app.set_player_open(true);
            app.set_playback_started(true);
            app.set_duration(1440.0);
            app.set_position(1321.0);
            bridge.refresh_next_episode_prompt();
            assert!(app.get_next_episode_visible());
            assert_eq!(
                app.get_next_episode_token().to_string(),
                format!("{session}:tt123:1:2")
            );
            let token = app.get_next_episode_token().to_string();

            bridge
                .dispatch_playback_action(PlaybackAction::NextEpisode { token })
                .unwrap();

            assert!(!app.get_player_open());
            assert!(!request.is_current());
            assert!(!app.get_next_episode_visible());
            assert!(app.get_party_active());
            assert!(!app.get_party_page_open());
            assert!(app.get_modal_visible());
            assert!(app.get_stream_selector_open());
            assert_eq!(app.get_detail_tab(), 3);
            let state = bridge.shared.lock().unwrap();
            assert!(Arc::ptr_eq(state.party.service.as_ref().unwrap(), &service));
            assert!(state.party.choosing_media);
            assert!(matches!(
                state.party.pending.front(),
                Some(Operation::ClearMedia { browse: false })
            ));
            assert_eq!(state.modal_item.as_ref().unwrap().request_id, "tt123:1:2");
            assert_eq!(state.playback.as_ref().unwrap().episode_id, "tt123:1:2");
        },
    );
}

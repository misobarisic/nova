//! Local actions use the same authoritative alias registers as incoming sync.
use super::*;

const ORIGINAL: &str = "kitsu:50021";
const CANONICAL: &str = "tt26743760";
const FIRST: &str = "kitsu:50021:2";
const SECOND: &str = "kitsu:50021:3";

fn videos() -> Vec<Video> {
    serde_json::from_value(serde_json::json!([
        {"id":FIRST,"season":1,"episode":1,"released":"2000-01-01","novaMetadataRevision":nova_providers::metadata_revision(),"novaStreamIds":[FIRST]},
        {"id":SECOND,"season":1,"episode":2,"released":"2000-01-01","novaMetadataRevision":nova_providers::metadata_revision(),"novaStreamIds":[SECOND]}
    ])).unwrap()
}

fn select(bridge: &Bridge, id: &str) {
    bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
        details: Default::default(),
        open_token: Arc::new(()),
        pending_watch_now: None,
        episodes_loading: false,
        id: id.into(),
        type_: "series".into(),
        request_id: FIRST.into(),
        videos: videos(),
        season_backdrops: HashMap::new(),
        seasons: vec![1],
        season_index: 0,
        episode_page: 0,
        name: id.into(),
        year: String::new(),
        poster_url: String::new(),
        background_url: String::new(),
        logo_url: String::new(),
        description: String::new(),
        genres: vec![],
    });
    bridge.apply_episode_rows();
    bridge.apply_season_cards();
}

fn settled(before: u64) {
    let started = std::time::Instant::now();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(3),
        move || {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "local identity projection timed out"
            );
            if progress_projection::publication_count() > before {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
    persistence::flush().unwrap();
}

fn settled_library_view(app: &AppWindow) {
    let started = std::time::Instant::now();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(3),
        move || {
            assert!(started.elapsed() < Duration::from_secs(20));
            if !weak.upgrade().unwrap().get_library_view_loading() {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
}

#[test]
fn local_cross_parent_watch_unwatch_resume_and_history_preserve_action_ownership() {
    const ROOT: &str = "NOVA_PROGRESS_IDENTITY_TEST_ROOT";
    let Some(root) = std::env::var_os(ROOT) else {
        let root = std::env::temp_dir().join(format!(
            "nova-progress-identity-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(ROOT, &root).env("XDG_CACHE_HOME", root.join("cache"))
            .args(["--exact", "app::progress_identity_tests::local_cross_parent_watch_unwatch_resume_and_history_preserve_action_ownership", "--nocapture"]).output().unwrap();
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
    let other = profiles::create("Other profile").unwrap();
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
    let writer = persistence::start().unwrap();
    for id in [ORIGINAL, CANONICAL] {
        write_episodes_cache_for("series", id, &videos());
    }
    {
        let mut state = bridge.shared.lock().unwrap();
        state.entries = [ORIGINAL, CANONICAL]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                serde_json::from_value(serde_json::json!({
                    "id":id,"type_":"series","name":id,"added_at_secs":index + 1
                }))
                .unwrap()
            })
            .collect();
        Bridge::set_episode_watched_locked(&mut state.progress, CANONICAL, SECOND, true);
    }
    bridge.persist_library();
    write_progress_map(&bridge.shared.lock().unwrap().progress.clone());
    persistence::flush().unwrap();
    bridge.apply_library_to_ui();
    super::run::wire_page_navigation(&app, &bridge);
    let retained_library = app.get_library();
    let retained_home = app.get_home_continue();
    app.set_library_scroll_y(-321.0);
    let reads = detail::trace_episode_reads(|| app.invoke_library_picked());
    assert!(
        reads.is_empty(),
        "Library navigation must not scan metadata on the UI thread: {reads:?}"
    );
    assert!(app.get_show_library());
    assert!(
        app.get_library() == retained_library,
        "show retained cards before the worker finishes"
    );
    assert_eq!(app.get_library_scroll_y(), -321.0);
    app.set_home_view(2);
    let reads = detail::trace_episode_reads(|| app.invoke_home_picked());
    assert!(
        reads.is_empty(),
        "Home navigation must not scan metadata on the UI thread: {reads:?}"
    );
    assert!(app.get_show_home());
    assert_eq!(
        app.get_home_view(),
        0,
        "explicit Home navigation returns to its landing page"
    );
    assert!(app.get_home_continue() == retained_home);
    app.set_home_view(1);
    bridge.show_discover_page();
    app.set_home_scroll_y(-654.0);
    let before = progress_projection::publication_count();
    settled(before);
    assert_eq!(
        app.get_home_view(),
        1,
        "late refresh must not leave a newly opened Home subpage"
    );
    assert_eq!(app.get_home_scroll_y(), -654.0);
    assert!(
        !app.get_show_home() && !app.get_show_library(),
        "late data cannot replay navigation after leaving Home"
    );
    let retained = app.get_library();
    for query in ["superseded search", "no matching saved title"] {
        app.set_library_query(query.into());
        bridge.request_library_view();
        assert!(
            app.get_library_view_loading(),
            "pending results hide synchronously"
        );
        assert!(
            app.get_library() == retained,
            "keep artwork while hiding stale rows"
        );
    }
    settled_library_view(&app);
    assert_eq!(
        app.get_library().row_count(),
        0,
        "publish only the latest requested view"
    );
    app.set_library_query("".into());
    bridge.request_library_view();
    assert!(app.get_library_view_loading());
    settled_library_view(&app);
    assert!(
        app.get_library().row_count() > 0,
        "warm views settle the loading state too"
    );
    let painted = app.get_library().row_data(0).unwrap().id.to_string();
    // The current filter can change before its next publication. Activation
    // still targets the painted native identity, not a rederived neighbour.
    app.set_library_query("not in the painted view".into());
    let reads = detail::trace_episode_reads(|| bridge.open_library_item(0));
    assert!(!reads.is_empty());
    assert!(
        reads.iter().all(|(_, id)| *id == painted),
        "opening one card must not read other episode caches: {reads:?}"
    );
    assert_eq!(
        bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .id,
        painted
    );
    assert_eq!(app.get_library_kb_idx(), 0);
    let entry = bridge
        .shared
        .lock()
        .unwrap()
        .entries
        .iter()
        .find(|entry| entry.id == painted)
        .unwrap()
        .clone();
    app.set_library_kb_idx(7);
    let reads = detail::trace_episode_reads(|| bridge.open_library_entry(entry.clone()));
    assert!(reads.iter().all(|(_, id)| *id == painted));
    assert_eq!(
        app.get_library_kb_idx(),
        7,
        "Home openings leave the Library return target alone"
    );
    let saved = bridge.shared.lock().unwrap().entries.clone();
    {
        let mut state = bridge.shared.lock().unwrap();
        state.entries.retain(|entry| entry.id != painted);
        state.modal_item = None;
    }
    bridge.open_library_item(0);
    assert!(
        bridge.shared.lock().unwrap().modal_item.is_none(),
        "removed painted titles cannot redirect to another row"
    );
    bridge.shared.lock().unwrap().entries = saved;
    app.set_library_query("".into());
    // Metadata may prove an alias after the title was painted. The worker
    // restores the opening season once, but never overwrites a manual choice.
    let mut late = videos();
    late[1].season = Some(2);
    bridge
        .shared
        .lock()
        .unwrap()
        .progress
        .remove(&progress_map_key(ORIGINAL, SECOND));
    super::bridge::install_global_bridge(&bridge);
    let before = progress_projection::publication_count();
    for id in [ORIGINAL, CANONICAL] {
        write_episodes_cache_for("series", id, &late);
    }
    bridge.open_library_entry(entry.clone());
    assert_eq!(app.get_season_combo_idx(), 0);
    settled(before);
    assert_eq!(
        app.get_season_combo_idx(),
        1,
        "late proven history restores the latest watched season"
    );
    assert!(bridge.shared.lock().unwrap().progress[&progress_map_key(ORIGINAL, SECOND)].watched);
    bridge.open_library_entry(entry);
    bridge.season_picked(0);
    bridge
        .shared
        .lock()
        .unwrap()
        .progress
        .remove(&progress_map_key(ORIGINAL, SECOND));
    let before = progress_projection::publication_count();
    bridge.request_progress_projection();
    settled(before);
    assert_eq!(
        app.get_season_combo_idx(),
        0,
        "background aliases cannot undo explicit season navigation"
    );
    for id in [ORIGINAL, CANONICAL] {
        write_episodes_cache_for("series", id, &videos());
    }
    select(&bridge, ORIGINAL);
    let owner = nova_sync::local_store().unwrap();
    let domain = sync::profile_domain(sync::DOMAIN_PROGRESS);
    let before = progress_projection::publication_count();
    bridge.episode_watch_action(0, 1);
    persistence::flush().unwrap();
    let digest = owner.lock().unwrap().digest();
    settled(before);
    {
        let state = bridge.shared.lock().unwrap();
        for title in [ORIGINAL, CANONICAL] {
            for episode in [FIRST, SECOND] {
                assert!(state.progress[&progress_map_key(title, episode)].watched);
            }
        }
        assert!(
            state.new_episodes_list.is_empty(),
            "watched union must not advertise a duplicate New episode"
        );
    }
    assert_eq!(app.get_home_new_episodes().row_count(), 0);
    assert_eq!(
        owner.lock().unwrap().digest(),
        digest,
        "alias publication cannot allocate watch actions"
    );
    nova_sync::playback_checkpoint(|| {
        write_progress_map(&bridge.shared.lock().unwrap().progress.clone())
    });
    persistence::flush().unwrap();
    assert_eq!(
        owner.lock().unwrap().digest(),
        digest,
        "an ordinary checkpoint after alias projection cannot reassert its propagated marker"
    );
    assert!(
        owner
            .lock()
            .unwrap()
            .record(&domain, &progress_map_key(CANONICAL, FIRST))
            .is_none(),
        "a materialized view is not an authoritative record"
    );

    // Both parents use the same raw Kitsu episode IDs. Same-second opposing
    // actions are ordered by real action clocks, rather than title preference.
    for title in [CANONICAL, ORIGINAL] {
        select(&bridge, title);
        let before = progress_projection::publication_count();
        bridge.episode_watch_action(0, 2);
        settled(before);
        for parent in [ORIGINAL, CANONICAL] {
            assert!(
                !bridge.shared.lock().unwrap().progress[&progress_map_key(parent, FIRST)].watched
            );
        }
        let before = progress_projection::publication_count();
        bridge.episode_watch_action(0, 1);
        settled(before);
        for parent in [ORIGINAL, CANONICAL] {
            assert!(
                bridge.shared.lock().unwrap().progress[&progress_map_key(parent, FIRST)].watched
            );
        }
    }
    select(&bridge, CANONICAL);
    let before = progress_projection::publication_count();
    bridge.episode_watch_action(0, 2);
    select(&bridge, ORIGINAL);
    bridge.episode_watch_action(0, 1);
    settled(before);
    for parent in [ORIGINAL, CANONICAL] {
        assert!(bridge.shared.lock().unwrap().progress[&progress_map_key(parent, FIRST)].watched);
    }

    let before = progress_projection::publication_count();
    bridge.episode_watch_action(0, 2);
    settled(before);
    // Reverse the race: canonical is already false while an unsettled Watch
    // just made original true. An explicit same-second Unwatch must win too.
    select(&bridge, ORIGINAL);
    let before = progress_projection::publication_count();
    bridge.episode_watch_action(0, 1);
    select(&bridge, CANONICAL);
    bridge.episode_watch_action(0, 2);
    settled(before);
    for parent in [ORIGINAL, CANONICAL] {
        assert!(!bridge.shared.lock().unwrap().progress[&progress_map_key(parent, FIRST)].watched);
    }
    bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
        series_id: ORIGINAL.into(),
        episode_id: FIRST.into(),
        session: next_episode::new_session(),
        ..Default::default()
    });
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(1200.0);
    app.set_position(150.0);
    let before = progress_projection::publication_count();
    bridge.note_player_progress_from_ui();
    settled(before);
    for parent in [ORIGINAL, CANONICAL] {
        let state = bridge.shared.lock().unwrap();
        let row = &state.progress[&progress_map_key(parent, FIRST)];
        assert_eq!(row.position_secs, 150.0);
        assert!(!row.watched);
    }
    app.set_player_open(false);
    bridge.shared.lock().unwrap().playback = None;

    // Removing a saved parent does not remove valid history, nor can that
    // history re-create its Library row. A per-episode tombstone stays absent.
    bridge
        .shared
        .lock()
        .unwrap()
        .entries
        .retain(|entry| entry.id != ORIGINAL);
    bridge.persist_library();
    persistence::flush().unwrap();
    let original_key = progress_map_key(ORIGINAL, FIRST);
    {
        let device = nova_sync::local_device().unwrap();
        let mut store = owner.lock().unwrap();
        store.set(&domain, &original_key, None, 0, device);
        store.save().unwrap();
    }
    bridge.shared.lock().unwrap().progress.remove(&original_key);
    {
        let _guard = sync::ApplyingGuard::new();
        write_progress_map(&bridge.shared.lock().unwrap().progress.clone());
    }
    let before = progress_projection::publication_count();
    bridge.request_progress_projection();
    settled(before);
    assert!(
        !bridge
            .shared
            .lock()
            .unwrap()
            .progress
            .contains_key(&original_key)
    );
    assert!(
        owner
            .lock()
            .unwrap()
            .record(&domain, &original_key)
            .unwrap()
            .is_deleted()
    );
    assert_eq!(bridge.shared.lock().unwrap().entries.len(), 1);
    assert!(
        owner
            .lock()
            .unwrap()
            .record(
                &sync::profile_domain(sync::DOMAIN_LIBRARY),
                &library::library_key("series", ORIGINAL)
            )
            .unwrap()
            .is_deleted()
    );
    let digest = owner.lock().unwrap().digest();
    assert!(!bridge.sync_reconcile_progress_aliases());
    assert!(!bridge.sync_reconcile_progress_aliases());
    assert_eq!(
        owner.lock().unwrap().digest(),
        digest,
        "replay is projection only"
    );
    assert!(
        profiles::ProfileStore::for_profile(&other)
            .read(EPISODE_PROGRESS_KEY)
            .unwrap()
            .is_none(),
        "active history cannot leak to another profile"
    );
    drop(writer);
}

#[test]
fn legacy_progress_without_manual_action_marker_retains_its_schema() {
    let progress: EpisodeProgress = serde_json::from_value(serde_json::json!({
        "series_id":"kitsu:50021","episode_id":"kitsu:50021:2","position_secs":0,
        "duration_secs":0,"watched":true,"play_count":1,"updated_at_secs":1
    }))
    .unwrap();
    assert!(progress.manual_watch_action.is_none());
    assert!(
        serde_json::to_value(progress)
            .unwrap()
            .get("manual_watch_action")
            .is_none()
    );
}

#[test]
fn explicit_unwatch_of_an_unprojected_row_is_still_a_distinct_action() {
    let mut progress = HashMap::new();
    Bridge::set_episode_watched_locked(&mut progress, CANONICAL, FIRST, false);
    let row = &progress[&progress_map_key(CANONICAL, FIRST)];
    assert!(!row.watched);
    assert_eq!(row.position_secs, 0.0);
    assert!(row.unwatched_at_secs > 0);
    let first = row
        .manual_watch_action
        .clone()
        .expect("explicit action marker");
    Bridge::set_episode_watched_locked(&mut progress, CANONICAL, FIRST, false);
    assert_ne!(
        progress[&progress_map_key(CANONICAL, FIRST)]
            .manual_watch_action
            .as_deref(),
        Some(first.as_str())
    );
}

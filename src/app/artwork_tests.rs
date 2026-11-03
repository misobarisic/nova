//! Exercise completion delivery independently of shared-cache residency.
use super::*;

fn isolated(name: &str, test: impl FnOnce(AppWindow, Bridge)) {
    isolated_impl(name, false, test);
}

fn isolated_impl(name: &str, profile: bool, test: impl FnOnce(AppWindow, Bridge)) {
    const TEST_DIR: &str = "NOVA_ARTWORK_DELIVERY_TEST_DIR";
    let Some(root) = std::env::var_os(TEST_DIR) else {
        let root = std::env::temp_dir().join(format!(
            "nova-artwork-delivery-{}-{}-{name}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(TEST_DIR, &root)
            .args([
                "--exact",
                &format!("app::artwork_tests::{name}"),
                "--nocapture",
            ])
            .output()
            .unwrap();
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
    i_slint_backend_testing::init_integration_test_with_mock_time();
    storage::init_at(&root);
    if profile {
        profiles::initialize().unwrap();
    }
    decoded_cache_clear();
    set_decoded_cache_budget(0);
    let app = AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    let player = crate::player::Player::setup(&app);
    let downloads = DownloadCoordinator::new(root.join("downloads"));
    #[cfg(feature = "desktop")]
    let bridge = {
        Bridge::new(
            app.as_weak(),
            PosterTx::closed(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        )
    };
    #[cfg(not(feature = "desktop"))]
    let bridge = Bridge::new(
        app.as_weak(),
        Arc::new(AtomicU64::new(0)),
        player,
        downloads,
    );
    test(app, bridge);
}

fn dimensions(image: Image) -> (u32, u32) {
    let size = image.size();
    (size.width, size.height)
}

fn preview(id: &str, url: &str) -> MetaPreview {
    MetaPreview {
        id: id.into(),
        type_: "series".into(),
        name: id.into(),
        background: (!url.is_empty()).then(|| url.into()),
        ..Default::default()
    }
}

#[test]
fn home_completion_paints_without_lru_and_inactive_prefetch_does_not_reload() {
    isolated(
        "home_completion_paints_without_lru_and_inactive_prefetch_does_not_reload",
        |app, bridge| {
            let url = "https://example.invalid/direct-home-backdrop.jpg";
            {
                let mut state = bridge.shared.lock().unwrap();
                state.home_showcase = vec![preview("visible", url)];
                state.home_showcase_pending_index = Some(0);
                state
                    .home_showcase_artwork
                    .entry(0)
                    .or_default()
                    .loading_url = Some(url.into());
            }
            let generation = bridge.shared.lock().unwrap().home_showcase_list_generation;
            bridge.finish_home_showcase_artwork(
                0,
                generation,
                image_settings_revision(),
                url.into(),
                Some(SharedPixelBuffer::new(12, 7)),
            );
            assert_eq!(app.get_home_featured_title(), "visible");
            assert_eq!(dimensions(app.get_home_featured_backdrop()), (12, 7));
            assert_eq!(decoded_cache_stats(), (0, 0));

            // An inactive successful prefetch retains only its URL. Trying to
            // recover its pixels immediately would loop with a zero cache budget.
            let other_url = "https://example.invalid/inactive-home-backdrop.jpg";
            {
                let mut state = bridge.shared.lock().unwrap();
                state.home_showcase.push(preview("next", other_url));
                state
                    .home_showcase_artwork
                    .entry(1)
                    .or_default()
                    .loading_url = Some(other_url.into());
            }
            bridge.finish_home_showcase_artwork(
                1,
                generation,
                image_settings_revision(),
                other_url.into(),
                Some(SharedPixelBuffer::new(9, 5)),
            );
            {
                let state = bridge.shared.lock().unwrap();
                assert!(state.home_showcase_artwork[&1].loading_url.is_none());
                assert_eq!(state.home_showcase_artwork[&1].backdrop_url, other_url);
            }
            assert_eq!(app.get_home_featured_title(), "visible");
            assert_eq!(dimensions(app.get_home_featured_backdrop()), (12, 7));

            // Navigating there can accept a fresh completion even if that
            // completion also never entered the shared cache.
            {
                let mut state = bridge.shared.lock().unwrap();
                state.home_showcase_pending_index = Some(1);
                state.home_showcase_artwork.get_mut(&1).unwrap().loading_url =
                    Some(other_url.into());
                // Do not start a real prefetch of the slide we just left.
                state.home_showcase[0].background = None;
            }
            bridge.finish_home_showcase_artwork(
                1,
                generation,
                image_settings_revision(),
                other_url.into(),
                Some(SharedPixelBuffer::new(9, 5)),
            );
            assert_eq!(app.get_home_featured_title(), "next");
            assert_eq!(dimensions(app.get_home_featured_backdrop()), (9, 5));
            assert_eq!(decoded_cache_stats(), (0, 0));
            bridge.finish_home_showcase_artwork(
                0,
                generation.wrapping_sub(1),
                image_settings_revision(),
                url.into(),
                Some(SharedPixelBuffer::new(1, 1)),
            );
            assert_eq!(
                app.get_home_featured_title(),
                "next",
                "old list completion cannot select a replaced slide"
            );
        },
    );
}

fn modal(videos: Vec<Video>, seasons: Vec<u32>) -> ModalItem {
    ModalItem {
        details: Default::default(),
        open_token: Arc::new(()),
        pending_watch_now: None,
        episodes_loading: false,
        id: "test-series".into(),
        type_: "series".into(),
        request_id: "test-series".into(),
        videos,
        season_backdrops: HashMap::new(),
        seasons,
        season_index: 0,
        episode_page: 0,
        name: "Test series".into(),
        year: String::new(),
        poster_url: String::new(),
        background_url: String::new(),
        logo_url: String::new(),
        description: String::new(),
        genres: Vec::new(),
    }
}

fn thumbnail_job(bridge: &Bridge, url: &str) -> EpisodeThumbJob {
    let state = bridge.shared.lock().unwrap();
    let modal = state.modal_item.as_ref().unwrap();
    EpisodeThumbJob {
        context: profiles::capture(),
        job_id: 1,
        request_key: "test".into(),
        url: url.into(),
        cache_key: image_cache_key(url, ImageUse::EpisodeThumbnail),
        settings_revision: image_settings_revision(),
        open_token: modal.open_token.clone(),
        item_id: modal.id.clone(),
        media_type: modal.type_.clone(),
        verify: false,
    }
}

#[test]
fn detail_current_page_retains_direct_pixels_and_rejects_replaced_identity() {
    isolated(
        "detail_current_page_retains_direct_pixels_and_rejects_replaced_identity",
        |app, bridge| {
            let url = "https://example.invalid/direct-episode.jpg";
            let videos = (1..=120)
                .map(|episode| Video {
                    id: format!("episode-{episode}"),
                    name: format!("Episode {episode}"),
                    season: Some(1),
                    episode: Some(episode),
                    thumbnail: Some(url.into()),
                    ..Default::default()
                })
                .collect();
            {
                let mut state = bridge.shared.lock().unwrap();
                state.cache_settings.show_unwatched_thumbs = true;
                state.modal_item = Some(modal(videos, vec![1]));
                state.modal_item.as_mut().unwrap().episode_page = 1;
            }
            app.set_modal_visible(true);
            bridge.apply_episode_rows();
            bridge.apply_season_cards();
            assert_eq!(app.get_episode_rows().row_count(), EPISODE_PAGE_SIZE);
            assert_eq!(app.get_episode_rows().row_data(0).unwrap().id, "episode-51");
            assert_eq!(app.get_episode_total(), 120);
            let job = thumbnail_job(&bridge, url);
            bridge.publish_detail_thumbnail(&job, SharedPixelBuffer::new(6, 3));
            for row in app.get_episode_rows().iter() {
                assert!(row.has_thumb);
                assert_eq!(dimensions(row.thumb), (6, 3));
            }
            assert!(app.get_season_cards().row_data(0).unwrap().has_thumb);
            assert_eq!(decoded_cache_stats(), (0, 0));
            bridge.apply_episode_rows();
            bridge.apply_season_cards();
            assert!(app.get_episode_rows().iter().all(|row| row.has_thumb));
            assert!(app.get_season_cards().row_data(0).unwrap().has_thumb);

            // Clamping builds only the final page; filtering then clamps it to
            // page zero instead of manufacturing every episode image first.
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_mut()
                .unwrap()
                .episode_page = 999;
            bridge.apply_episode_rows();
            assert_eq!(app.get_episode_page(), 2);
            assert_eq!(app.get_episode_rows().row_count(), 20);
            app.set_episode_filter("Episode 51".into());
            bridge.apply_episode_rows();
            assert_eq!(app.get_episode_page(), 0);
            assert_eq!(app.get_episode_rows().row_count(), 1);
            assert_eq!(app.get_episode_rows().row_data(0).unwrap().id, "episode-51");
            bridge.publish_detail_thumbnail(&job, SharedPixelBuffer::new(6, 3));
            assert!(app.get_episode_rows().row_data(0).unwrap().has_thumb);

            // A new source URL, reopening the same series, and a settings edit
            // each invalidate older jobs independently.
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_mut()
                .unwrap()
                .videos[50]
                .thumbnail = Some("new-url".into());
            bridge.apply_episode_rows();
            bridge.publish_detail_thumbnail(&job, SharedPixelBuffer::new(1, 1));
            assert!(!app.get_episode_rows().row_data(0).unwrap().has_thumb);
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_mut()
                .unwrap()
                .videos[50]
                .thumbnail = Some(url.into());
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_mut()
                .unwrap()
                .open_token = Arc::new(());
            bridge.apply_episode_rows();
            bridge.publish_detail_thumbnail(&job, SharedPixelBuffer::new(1, 1));
            assert!(!app.get_episode_rows().row_data(0).unwrap().has_thumb);
            let mut stale_settings = thumbnail_job(&bridge, url);
            stale_settings.settings_revision = stale_settings.settings_revision.wrapping_add(1);
            bridge.publish_detail_thumbnail(&stale_settings, SharedPixelBuffer::new(1, 1));
            assert!(!app.get_episode_rows().row_data(0).unwrap().has_thumb);
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .show_unwatched_thumbs = false;
            bridge.apply_episode_rows();
            bridge.publish_detail_thumbnail(
                &thumbnail_job(&bridge, url),
                SharedPixelBuffer::new(1, 1),
            );
            assert!(!app.get_episode_rows().row_data(0).unwrap().has_thumb);
        },
    );
}

#[test]
fn episode_rebuild_requeues_seasons_and_old_jobs_cannot_unregister_new_work() {
    isolated(
        "episode_rebuild_requeues_seasons_and_old_jobs_cannot_unregister_new_work",
        |app, bridge| {
            let videos = (1..=7)
                .map(|season| Video {
                    id: format!("season-{season}-episode"),
                    season: Some(season),
                    episode: Some(1),
                    thumbnail: Some(format!("https://example.invalid/season-{season}.jpg")),
                    ..Default::default()
                })
                .collect();
            bridge.shared.lock().unwrap().modal_item = Some(modal(videos, (1..=7).collect()));
            app.set_modal_visible(true);
            // Occupied worker slots keep this deterministic and prevent transport.
            EPISODE_ACTIVE.store(4, Ordering::SeqCst);
            bridge.apply_season_cards();
            bridge.dispatch_season_thumbs();
            assert_eq!(EPISODE_QUEUE.lock().unwrap().len(), 7);
            bridge.refresh_episode_rows();
            assert_eq!(
                EPISODE_QUEUE.lock().unwrap().len(),
                7,
                "episode rebuild cannot drop waiting season images"
            );
            assert_eq!(EPISODE_INFLIGHT.lock().unwrap().len(), 7);
            let mut old = thumbnail_job(&bridge, "old");
            old.request_key = "reused-key".into();
            old.job_id = 10;
            EPISODE_INFLIGHT
                .lock()
                .unwrap()
                .insert(old.request_key.clone(), 11);
            Bridge::release_episode_thumb_job(&old);
            assert_eq!(
                EPISODE_INFLIGHT.lock().unwrap().get(&old.request_key),
                Some(&11)
            );
            let jobs: Vec<_> = EPISODE_QUEUE.lock().unwrap().drain(..).collect();
            for job in jobs {
                Bridge::release_episode_thumb_job(&job);
            }
            EPISODE_INFLIGHT.lock().unwrap().clear();
            EPISODE_ACTIVE.store(0, Ordering::SeqCst);
        },
    );
}

#[test]
fn remote_recipe_refresh_rebuilds_calendar_and_current_detail_work_once() {
    isolated(
        "remote_recipe_refresh_rebuilds_calendar_and_current_detail_work_once",
        |app, bridge| {
            let (year, month, day) = home::civil_from_days(today_days());
            let videos: Vec<_> = (1..=120)
                .map(|episode| Video {
                    id: format!("episode-{episode}"),
                    season: Some(1),
                    episode: Some(episode),
                    released: Some(format!("{year:04}-{month:02}-{day:02}")),
                    thumbnail: Some(format!("https://example.invalid/episode-{episode}.jpg")),
                    ..Default::default()
                })
                .collect();
            detail::write_episodes_cache_for("series", "test-series", &videos);
            {
                let mut state = bridge.shared.lock().unwrap();
                state.entries.push(
                    serde_json::from_value(serde_json::json!({
                        "id": "test-series", "type_": "series", "name": "Test series"
                    }))
                    .unwrap(),
                );
                state.modal_item = Some(modal(videos, vec![1]));
                state.modal_item.as_mut().unwrap().episode_page = 1;
                state.cache_settings.show_unwatched_thumbs = true;
            }
            app.set_show_calendar(true);
            app.set_calendar_first(today_days() as i32);
            app.set_calendar_selected(today_days() as i32);
            app.set_calendar_all(true);
            app.set_modal_visible(true);
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            bridge.apply_season_cards();
            bridge.apply_episode_rows();
            let calendar_identity = app
                .get_calendar_rows()
                .row_data(0)
                .unwrap()
                .artwork_identity;
            let season_identity = app.get_season_cards().row_data(0).unwrap().artwork_identity;
            let episode_identity = app.get_episode_rows().row_data(0).unwrap().artwork_identity;
            let previous_revision = image_settings_revision();
            EPISODE_ACTIVE.store(4, Ordering::SeqCst);
            bridge.refresh_remote_artwork_if_revision_changed(previous_revision);
            assert!(EPISODE_QUEUE.lock().unwrap().is_empty());
            let mut settings = active_cache_settings();
            settings.enabled = true;
            settings.quality = if settings.quality == 80 { 81 } else { 80 };
            set_cache_settings(settings);
            bridge.refresh_remote_artwork_if_revision_changed(previous_revision);
            assert_ne!(
                app.get_calendar_rows()
                    .row_data(0)
                    .unwrap()
                    .artwork_identity,
                calendar_identity
            );
            assert_ne!(
                app.get_season_cards().row_data(0).unwrap().artwork_identity,
                season_identity
            );
            assert_ne!(
                app.get_episode_rows().row_data(0).unwrap().artwork_identity,
                episode_identity
            );
            assert_eq!(app.get_episode_rows().row_count(), EPISODE_PAGE_SIZE);
            assert_eq!(app.get_episode_page(), 1);
            let jobs: Vec<_> = EPISODE_QUEUE.lock().unwrap().drain(..).collect();
            assert_eq!(
                jobs.len(),
                EPISODE_PAGE_SIZE + 1,
                "only current page plus season image"
            );
            assert!(
                jobs.iter()
                    .all(|job| job.settings_revision == image_settings_revision())
            );
            for job in jobs {
                Bridge::release_episode_thumb_job(&job);
            }
            bridge.refresh_remote_artwork_if_revision_changed(image_settings_revision());
            assert!(
                EPISODE_QUEUE.lock().unwrap().is_empty(),
                "unchanged recipe starts no work"
            );
            EPISODE_ACTIVE.store(0, Ordering::SeqCst);
        },
    );
}

#[test]
fn home_fallback_completion_upgrades_newly_supplied_preferred_background() {
    isolated(
        "home_fallback_completion_upgrades_newly_supplied_preferred_background",
        |app, bridge| {
            let poster = "https://example.invalid/fallback-poster.jpg";
            let background = "https://example.invalid/new-preferred-background.jpg";
            {
                let mut state = bridge.shared.lock().unwrap();
                let mut item = preview("enriched", background);
                item.poster = Some(poster.into());
                state.home_showcase = vec![item];
                state.home_showcase_pending_index = Some(0);
                state
                    .home_showcase_artwork
                    .entry(0)
                    .or_default()
                    .loading_url = Some(poster.into());
            }
            set_decoded_cache_budget(1);
            decoded_cache_insert(
                &image_cache_key(background, ImageUse::Fixed(None)),
                SharedPixelBuffer::new(7, 4),
            );
            let generation = bridge.shared.lock().unwrap().home_showcase_list_generation;
            bridge.finish_home_showcase_artwork(
                0,
                generation,
                image_settings_revision(),
                poster.into(),
                Some(SharedPixelBuffer::new(2, 3)),
            );
            decoded_cache_clear();
            assert_eq!(dimensions(app.get_home_featured_backdrop()), (7, 4));
            assert_eq!(
                bridge.shared.lock().unwrap().home_showcase_artwork[&0].backdrop_url,
                background
            );
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .home_showcase_pending_index
                    .is_none()
            );
            assert!(
                bridge.shared.lock().unwrap().home_showcase_artwork[&0]
                    .loading_url
                    .is_none()
            );
        },
    );
}

fn colliding_entries(order: [&str; 2], url: &str) -> Vec<LibraryEntry> {
    order
        .into_iter()
        .map(|type_| {
            serde_json::from_value(serde_json::json!({
                "id": "local-42", "type_": type_, "name": type_,
                "poster_url": url, "background_url": format!("old-{type_}"),
                "added_at_secs": 1
            }))
            .unwrap()
        })
        .collect()
}

fn assert_saved_entries(bridge: &Bridge, expected: &[LibraryEntry]) {
    assert_eq!(bridge.shared.lock().unwrap().entries, expected);
    persistence::flush().unwrap();
    // Read the committed JSON directly, bypassing pending-library snapshots.
    assert_eq!(read_json::<Vec<LibraryEntry>>("library").unwrap(), expected);
}

#[test]
fn typed_artwork_helpers_preserve_colliding_titles_and_restart() {
    isolated(
        "typed_artwork_helpers_preserve_colliding_titles_and_restart",
        |_, bridge| {
            let writer = persistence::start().unwrap();
            for order in [["movie", "series"], ["series", "movie"]] {
                for target in ["movie", "series"] {
                    let mut expected = colliding_entries(order, "old-poster");
                    bridge.shared.lock().unwrap().entries = expected.clone();
                    bridge.persist_library();
                    let index = expected
                        .iter()
                        .position(|entry| entry.type_ == target)
                        .unwrap();
                    bridge.persist_poster_for(target, "local-42", "new-poster");
                    expected[index].poster_url = "new-poster".into();
                    assert_saved_entries(&bridge, &expected);
                    bridge.persist_backdrop_for(target, "local-42", "new-backdrop");
                    expected[index].background_url = "new-backdrop".into();
                    assert_saved_entries(&bridge, &expected);
                    bridge.persist_header_for(
                        target,
                        "local-42",
                        &["Drama".into()],
                        "Synopsis",
                        "2026",
                        false,
                    );
                    expected[index].genres = vec!["Drama".into()];
                    expected[index].description = "Synopsis".into();
                    expected[index].year = "2026".into();
                    assert_saved_entries(&bridge, &expected);
                    bridge.persist_header_for(
                        target,
                        "local-42",
                        &["Comedy".into()],
                        "New synopsis",
                        "2027",
                        true,
                    );
                    expected[index].genres = vec!["Comedy".into()];
                    expected[index].description = "New synopsis".into();
                    expected[index].year = "2027".into();
                    assert_saved_entries(&bridge, &expected);
                    // Restart's Library reload uses these persisted typed entries.
                    bridge.shared.lock().unwrap().entries.clear();
                    bridge.shared.lock().unwrap().entries = read_persisted_library();
                    assert_saved_entries(&bridge, &expected);
                }
            }
            let expected = bridge.shared.lock().unwrap().entries.clone();
            drop(writer);
            // No writer or pending snapshot survives this reload.
            bridge.shared.lock().unwrap().entries.clear();
            bridge.shared.lock().unwrap().entries = read_persisted_library();
            assert_saved_entries(&bridge, &expected);
        },
    );
}

#[test]
fn typed_artwork_publication_paints_only_retained_identity() {
    isolated(
        "typed_artwork_publication_paints_only_retained_identity",
        |app, bridge| {
            for order in [["movie", "series"], ["series", "movie"]] {
                for target in ["movie", "series"] {
                    let entries = colliding_entries(order, "same-old-url");
                    {
                        let mut state = bridge.shared.lock().unwrap();
                        state.entries = entries.clone();
                        state.library_displayed = entries
                            .iter()
                            .map(|entry| (entry.type_.clone(), entry.id.clone()))
                            .collect();
                    }
                    let model: slint::ModelRc<MediaCard> = Rc::new(VecModel::from(
                        entries
                            .iter()
                            .map(|entry| MediaCard {
                                id: entry.id.clone().into(),
                                // Deliberately identical translated labels cannot identify a row.
                                media_type: "Localized title".into(),
                                poster_path: entry.poster_url.clone().into(),
                                poster: Image::from_rgba8(SharedPixelBuffer::new(3, 2)),
                                is_loaded: true,
                                ..Default::default()
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into();
                    app.set_library(model.clone());
                    bridge.publish_discover_poster(
                        target,
                        "local-42",
                        "repaired-url",
                        &SharedPixelBuffer::new(8, 5),
                    );
                    for (index, entry) in entries.iter().enumerate() {
                        let row = model.row_data(index).unwrap();
                        assert_eq!(
                            dimensions(row.poster),
                            if entry.type_ == target {
                                (8, 5)
                            } else {
                                (3, 2)
                            }
                        );
                        assert_eq!(
                            row.poster_path.as_str(),
                            if entry.type_ == target {
                                "repaired-url"
                            } else {
                                "same-old-url"
                            }
                        );
                    }
                }
            }
        },
    );
}

#[test]
fn typed_library_recovery_preserves_type_for_equal_failed_urls() {
    isolated(
        "typed_library_recovery_preserves_type_for_equal_failed_urls",
        |_, bridge| {
            for order in [["movie", "series"], ["series", "movie"]] {
                for url in ["", "same-failed-url"] {
                    for target in ["movie", "series"] {
                        {
                            let mut state = bridge.shared.lock().unwrap();
                            state.entries = colliding_entries(order, url);
                            state.installed.clear();
                            state.pending_artwork_recovery.clear();
                        }
                        bridge.recover_library_artwork_for_test(target, "local-42", url);
                        let state = bridge.shared.lock().unwrap();
                        assert_eq!(state.pending_artwork_recovery.len(), 1);
                        assert!(state.pending_artwork_recovery.contains_key(&(
                            target.into(),
                            "local-42".into(),
                            url.into(),
                            String::new()
                        )));
                        drop(state);
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .pending_artwork_recovery
                            .clear();
                        bridge.recover_library_artwork_for_test(
                            target,
                            "local-42",
                            "expired-request-url",
                        );
                        assert!(
                            bridge
                                .shared
                                .lock()
                                .unwrap()
                                .pending_artwork_recovery
                                .is_empty()
                        );
                    }
                }
            }
        },
    );
}

#[test]
fn typed_artwork_completion_from_previous_profile_is_discarded() {
    isolated_impl(
        "typed_artwork_completion_from_previous_profile_is_discarded",
        true,
        |_, bridge| {
            let owner = profiles::ProfileStore::capture();
            bridge.shared.lock().unwrap().entries =
                colliding_entries(["movie", "series"], "original");
            bridge.persist_library();
            let stale = bridge.clone();
            let called = Arc::new(AtomicBool::new(false));
            let observed = called.clone();
            let outgoing = profiles::active_context();
            let (release, delayed) = std::sync::mpsc::channel();
            let completion = std::thread::spawn(move || {
                delayed.recv().unwrap();
                profiles::with_context(outgoing, || {
                    profiles::invoke_from_event_loop(move || {
                        observed.store(true, Ordering::Relaxed);
                        stale.publish_discover_poster(
                            "series",
                            "local-42",
                            "stale-url",
                            &SharedPixelBuffer::new(8, 5),
                        );
                    })
                    .unwrap();
                });
            });
            let other = profiles::create("Other artwork owner").unwrap();
            bridge.switch_profile(&other);
            bridge.wait_profile_activation();
            assert_eq!(profiles::active_context().id, other);
            assert_eq!(profiles::ProfileStore::capture().id(), other);
            assert_eq!(bridge.sessions.active().id, other);
            // Queue only after publication, while retaining the outgoing receipt.
            release.send(()).unwrap();
            completion.join().unwrap();
            let expected = colliding_entries(["series", "movie"], "private");
            bridge.shared.lock().unwrap().entries = expected.clone();
            bridge.persist_library();
            slint::invoke_from_event_loop(|| slint::quit_event_loop().unwrap()).unwrap();
            slint::run_event_loop().unwrap();
            assert!(!called.load(Ordering::Relaxed));
            assert_saved_entries(&bridge, &expected);
            let original: Vec<LibraryEntry> =
                serde_json::from_str(&owner.read("library").unwrap().unwrap()).unwrap();
            assert_eq!(original, colliding_entries(["movie", "series"], "original"));
        },
    );
}

#[test]
fn calendar_worker_reuses_metadata_and_rejects_superseded_and_failed_receipts() {
    isolated_impl(
        "calendar_worker_reuses_metadata_and_rejects_superseded_and_failed_receipts",
        true,
        |app, bridge| {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .with_test_writer()
                .try_init()
                .expect("isolated fixture tracing subscriber");
            let owner = profiles::ProfileStore::capture();
            let day = today_days();
            let (year, month, date) = home::civil_from_days(day);
            let video = |id: &str| Video {
                id: id.into(),
                season: Some(1),
                episode: Some(1),
                released: Some(format!("{year:04}-{month:02}-{date:02}")),
                ..Default::default()
            };
            let entry: LibraryEntry = serde_json::from_value(serde_json::json!({
                "id":"calendar-worker", "type_":"series", "name":"Calendar Worker"
            }))
            .unwrap();
            bridge.shared.lock().unwrap().entries = vec![entry.clone()];
            owner
                .write("library", &serde_json::to_string(&vec![entry]).unwrap())
                .unwrap();
            detail::write_episodes_cache_for("series", "calendar-worker", &[video("first")]);
            app.set_show_calendar(true);
            app.set_calendar_all(true);
            app.set_calendar_first(home::month_first(day) as i32);
            app.set_calendar_selected(day as i32);
            calendar::metadata_delay(100);
            let cold = std::time::Instant::now();
            bridge.apply_calendar_to_ui();
            eprintln!(
                "STATE-02 cold callback {:?} with injected 100ms metadata read",
                cold.elapsed()
            );
            // Supersede a cold request before it can publish its old inventory.
            detail::write_episodes_cache_for("series", "calendar-worker", &[video("latest")]);
            calendar::snapshot_committed(&owner, &episodes_key("series", "calendar-worker"));
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            calendar::metadata_delay(0);
            assert_eq!(app.get_calendar_rows().row_count(), 1);
            assert!(
                app.get_calendar_rows()
                    .row_data(0)
                    .unwrap()
                    .key
                    .contains("latest")
            );
            let baseline = calendar::counters(&bridge);
            let policy_reads = profile_policy::snapshot_capture_count();
            let mut maximum = Duration::ZERO;
            let reads = detail::trace_episode_reads(|| {
                for offset in 0..1000 {
                    let started = std::time::Instant::now();
                    bridge.calendar_pick((day + i64::from(offset % 35)) as i32);
                    app.set_calendar_view(offset % 3);
                    app.set_calendar_search(if offset % 2 == 0 {
                        " worker ".into()
                    } else {
                        "".into()
                    });
                    bridge.calendar_reset();
                    maximum = maximum.max(started.elapsed());
                }
                bridge.calendar_today();
                bridge.calendar_step(1);
                bridge.calendar_step(-1);
            });
            eprintln!(
                "STATE-02 warm callback pair maximum {maximum:?}, 1000 selections/views/queries"
            );
            assert!(
                reads.is_empty(),
                "warm navigation reopened episode wrappers: {reads:?}"
            );
            assert_eq!(
                calendar::counters(&bridge),
                baseline,
                "warm navigation must not capture, read or build"
            );
            assert_eq!(profile_policy::snapshot_capture_count(), policy_reads);

            bridge.shared.lock().unwrap().progress.insert(
                progress_map_key("calendar-worker", "latest"),
                EpisodeProgress {
                    position_secs: 12.0,
                    duration_secs: 120.0,
                    play_count: 5,
                    ..Default::default()
                },
            );
            progress_projection::invalidate();
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            let position = calendar::counters(&bridge);
            assert_eq!(position.1, baseline.1, "positions must retain metadata");
            assert_eq!(position.2, baseline.2, "positions must retain policy");
            assert_eq!(
                position.3, baseline.3,
                "positions must retain the immutable projection"
            );
            bridge
                .shared
                .lock()
                .unwrap()
                .progress
                .get_mut(&progress_map_key("calendar-worker", "latest"))
                .unwrap()
                .watched = true;
            progress_projection::invalidate();
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            let watched = calendar::counters(&bridge);
            assert_eq!(watched.1, baseline.1);
            assert_eq!(
                watched.3,
                baseline.3 + 1,
                "watched facts rebuild without metadata reads"
            );

            // A successfully committed corrupt inventory retires the old rows.
            owner
                .write(&episodes_key("series", "calendar-worker"), "{broken")
                .unwrap();
            calendar::snapshot_committed(&owner, &episodes_key("series", "calendar-worker"));
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            assert_eq!(app.get_calendar_count(), 0);
            assert_eq!(app.get_calendar_rows().row_count(), 0);
            // The ordinary metadata writer first reads the old inventory and
            // intentionally refuses to overwrite corrupt snapshots. Repair the
            // injected fixture explicitly through the same owner used to inject
            // corruption, then publish the successful commit receipt.
            owner
                .write(
                    &episodes_key("series", "calendar-worker"),
                    &serde_json::to_string(&vec![video("recovered")]).unwrap(),
                )
                .unwrap();
            assert!(
                owner
                    .read(&episodes_key("series", "calendar-worker"))
                    .unwrap()
                    .unwrap()
                    .contains("recovered")
            );
            calendar::snapshot_committed(&owner, &episodes_key("series", "calendar-worker"));
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            app.set_calendar_view(0);
            bridge.calendar_pick(day as i32);
            assert_eq!(app.get_calendar_rows().row_count(), 1);

            // The collection layer rejects malformed JSON before commit.
            // Use valid JSON with an invalid policy schema to exercise a
            // committed policy that the foreground must reject closed.
            owner
                .write(profile_policy::KEY, r#"{"version":"broken"}"#)
                .unwrap();
            assert!(profile_policy::load(&owner).is_err());
            calendar::snapshot_committed(&owner, profile_policy::KEY);
            bridge.apply_calendar_to_ui();
            assert_eq!(
                app.get_calendar_rows().row_count(),
                0,
                "changed permissions fail closed before worker publication"
            );
            calendar::settle(&bridge);
            assert_eq!(app.get_calendar_count(), 0);
            assert!(
                app.get_calendar_cells()
                    .iter()
                    .all(|row| row.preview.is_empty())
            );

            // An outgoing activation may finish reading after a new one opens.
            owner.write(profile_policy::KEY, "{}").unwrap();
            assert!(
                profile_policy::load(&owner).is_ok(),
                "fixture policy repair must succeed"
            );
            // Publish the recovered policy through its normal validated owner
            // path. A raw-only snapshot has no sync baseline; target preparation
            // would seed it later and correctly invalidate source authorization.
            profile_policy::save(&owner, &profile_policy::ProfilePolicy::default()).unwrap();
            eprintln!(
                "calendar after policy save pending={:?}",
                nova_sync::local_store()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .pending_domains()
            );
            // The application tick projects and acknowledges pending local
            // policy records. This isolated fixture drives that normal path
            // explicitly before capturing the switch authorization receipt.
            let policy_domain = owner.domain(profile_policy::DOMAIN);
            bridge.sync_apply_and_wait(vec![policy_domain.clone()]);
            assert!(
                !nova_sync::local_store()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .pending_domains()
                    .contains(&policy_domain)
            );
            calendar::snapshot_committed(&owner, profile_policy::KEY);
            let (reading, release_read) = calendar::metadata_barrier();
            calendar::snapshot_committed(&owner, &episodes_key("series", "calendar-worker"));
            bridge.apply_calendar_to_ui();
            reading
                .recv_timeout(Duration::from_secs(10))
                .expect("outgoing metadata read did not start");
            let other = profiles::create("Other Calendar owner").unwrap();
            eprintln!(
                "calendar before switch source={} target={} pending={:?}",
                owner.id(),
                other,
                nova_sync::local_store()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .pending_domains()
            );
            bridge.switch_profile(&other);
            bridge.wait_profile_activation();
            eprintln!(
                "calendar after activation pending={:?}",
                nova_sync::local_store()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .pending_domains()
            );
            assert_eq!(
                profiles::active_context().id,
                other,
                "target activation failed: {}",
                app.get_profiles_error()
            );
            assert_eq!(profiles::ProfileStore::capture().id(), other);
            assert_eq!(bridge.sessions.active().id, other);
            assert!(bridge.shared.lock().unwrap().entries.is_empty());
            release_read.send(()).unwrap();
            app.set_show_calendar(true);
            bridge.apply_calendar_to_ui();
            calendar::settle(&bridge);
            calendar::metadata_delay(0);
            assert_eq!(
                app.get_calendar_rows().row_count(),
                0,
                "outgoing results cannot populate the incoming activation"
            );
            assert_eq!(app.get_calendar_count(), 0);
        },
    );
}

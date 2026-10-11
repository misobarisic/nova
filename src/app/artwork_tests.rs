//! Exercise completion delivery independently of shared-cache residency.
use super::*;

fn isolated(name: &str, test: impl FnOnce(AppWindow, Bridge)) {
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
    decoded_cache_clear();
    set_decoded_cache_budget(0);
    let app = AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    let player = crate::player::Player::setup(&app);
    let downloads = DownloadCoordinator::new(root.join("downloads"));
    #[cfg(feature = "desktop")]
    let bridge = {
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
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

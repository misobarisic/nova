//! Application entry point: window setup and callback wiring.
use super::*;

/// Nav index of the page currently shown: 0 Home, 1 Discover, 2 Library,
/// 3 More (including Settings/About), 4 Calendar, 5 Downloads (Discover is the fallback).
fn current_nav(app: &AppWindow) -> i32 {
    if app.get_more_open() || app.get_show_settings() || app.get_show_about() {
        3
    } else if app.get_show_calendar() {
        4
    } else if app.get_show_downloads() {
        5
    } else if app.get_show_home() {
        0
    } else if app.get_show_library() {
        2
    } else {
        1
    }
}

/// Publish selection feedback before Rust changes page flags. AppWindow owns
/// More origins and transition cancellation; persistent family chrome also
/// observes selection changes directly, while rebuilt bars consume `from`.
fn note_nav_switch(bridge: &Bridge, next: i32) {
    let Some(app) = bridge.app() else {
        return;
    };
    let from = current_nav(&app);
    bridge.remote_disconnect();
    bridge.cancel_pending_for_navigation();
    app.set_show_calendar(false);
    app.set_show_downloads(false);
    if from != next {
        app.global::<crate::NavState>().set_from(from);
    }
}

/// Connect every root destination to its production Bridge behavior. Global
/// rail callbacks enter AppWindow's canonical route before refreshing models,
/// so mouse and keyboard destinations both clear pending More returns.
pub(super) fn wire_page_navigation(app: &AppWindow, bridge: &Bridge) {
    let b = bridge.clone();
    app.on_library_picked(move || {
        note_nav_switch(&b, 2);
        b.show_library_page();
    });

    let b = bridge.clone();
    app.on_discover_picked(move || {
        note_nav_switch(&b, 1);
        b.show_discover_page();
    });

    let b = bridge.clone();
    app.on_home_picked(move || {
        note_nav_switch(&b, 0);
        b.show_home_page();
    });

    let b = bridge.clone();
    app.on_settings_picked(move || {
        note_nav_switch(&b, 3);
        b.show_settings_page();
    });

    let b = bridge.clone();
    app.on_about_picked(move || {
        note_nav_switch(&b, 3);
        b.show_about_page();
    });

    let b = bridge.clone();
    app.on_calendar_picked(move || {
        note_nav_switch(&b, 4);
        b.show_calendar_page();
    });

    let b = bridge.clone();
    app.on_downloads_picked(move || {
        note_nav_switch(&b, 5);
        b.show_downloads_page();
    });

    let weak = app.as_weak();
    app.global::<crate::NavState>().on_calendar_picked(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_navigate_section(4);
        }
    });

    let weak = app.as_weak();
    app.global::<crate::NavState>()
        .on_downloads_picked(move || {
            if let Some(app) = weak.upgrade() {
                app.invoke_navigate_section(5);
            }
        });
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Native desktop: in-app playback renders through mpv's OpenGL underlay
    // (src/player.rs). Like the reference prototype, the app runs on the
    // default femtovg GL renderer (it exposes GraphicsAPI::NativeOpenGL to
    // the rendering notifier); skia's presentation path is *not* compatible
    // with drawing mpv into the default framebuffer at BeforeRendering. Set
    // the renderer before the first Slint window is created (which
    // initializes the platform). Never on Android: it uses Slint's
    // android-activity backend, which these overrides would break.
    // SAFETY: configure the process before initializing services or workers.
    #[cfg(not(target_os = "android"))]
    unsafe {
        std::env::set_var("SLINT_BACKEND", "winit");
        std::env::set_var("SLINT_RENDERER", "femtovg");
        // Winit otherwise delegates context menus to Win32/muda, which
        // ignores Slint's dark palette. Use the same rendered popup path
        // as Linux; the Windows UI is compiled with fluent-dark widgets.
        #[cfg(target_os = "windows")]
        std::env::set_var("SLINT_NO_MUDA", "1");
    }

    crate::diagnostics::init();
    net::init_metadata_transport();
    // Set the native frame theme before its first creation, avoiding a light
    // title-bar flash. Keep Windows decorations and caption-button behavior.
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .with_winit_window_attributes_hook(|attributes| {
            attributes.with_theme(Some(slint::winit_030::winit::window::Theme::Dark))
        })
        .select()?;
    let app = AppWindow::new()?;
    nova_ui::initialize_fonts(app.window());
    app.set_app_version(crate::version::DISPLAY.into());
    app.set_update_is_development(!crate::version::is_release());
    about::prepare_license_text(&app);
    app.set_license_sources(
        Rc::new(VecModel::<LicenseSource>::from(
            open_source_license_sources(),
        ))
        .into(),
    );
    // Inspect existence before redb creates the file; an empty old install
    // still deserves the upgrade path rather than being treated as first run.
    let database_existed = app_data_dir()
        .join("nova.redb")
        .try_exists()
        .unwrap_or(true);
    storage::init_at(&app_data_dir());
    #[cfg(feature = "update-check")]
    let _updates_timer = updates::install(&app);
    persistence::register_collections();
    let onboarding_visible = onboarding::begin(database_existed);
    let owns_addon_setup = onboarding::owns_addon_setup();
    app.set_onboarding_visible(onboarding_visible);
    nova_providers::set_metadata_cache(Arc::new(io::ProviderMetadataCache));
    if let Err(error) = nova_providers::plugin_settings::configure_plugin_settings(Arc::new(
        plugin_settings::Backend,
    )) {
        io::block_unreadable(plugin_settings::KEY);
        storage::report(storage::Error::new(storage::ErrorKind::Schema, error));
    } else {
        plugin_cache::start_gc_worker();
    }
    crate::web_log("nova: window created");

    // Image cache and torrent streaming are always available.
    app.set_show_image_cache(true);
    app.set_show_torrents(true);

    // Discover page structure: on Android the filter header scrolls away
    // with the grid (whole-page scroll — touch devices have no wheel, so
    // drags starting on the filters must scroll too); desktop keeps the
    // header pinned and scrolls only the grid.
    app.set_discover_scroll_page(cfg!(target_os = "android"));

    // Shared stale-response guard for catalog fetches (both targets).
    let catalog_gen = Arc::new(AtomicU64::new(0));

    #[cfg(feature = "desktop")]
    // Poster download pipeline; pixels live only in the shared byte-budgeted cache.
    // Dual intake: `hi` carries near-viewport (pre)loads from `visible_range`,
    // `lo` carries the background full-grid sweeps. Workers always drain `hi`
    // first so scrolling into new rows never waits behind far-away images.
    let tx = {
        let (hi_tx, hi_rx) = mpsc::channel::<PosterJob>();
        let (lo_tx, lo_rx) = mpsc::channel::<PosterJob>();
        let hi_rx = Arc::new(Mutex::new(hi_rx));
        let lo_rx = Arc::new(Mutex::new(lo_rx));
        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        fn next_job(
            hi_rx: &Arc<Mutex<mpsc::Receiver<PosterJob>>>,
            lo_rx: &Arc<Mutex<mpsc::Receiver<PosterJob>>>,
        ) -> Result<PosterJob, ()> {
            // Priority intake: never give up on timeout — an idle timeout
            // just means "loop and check the other lane". Only disconnect
            // (both senders dropped at shutdown) ends the worker.
            loop {
                // Fast path: a preload is already waiting — take it without
                // blocking so scrolls stay responsive while a sweep is queued.
                if let Ok(job) = hi_rx.lock().unwrap().try_recv() {
                    return Ok(job);
                }
                // No preload waiting: block briefly for one (a scroll may land
                // while we idle), then fall back to one background job.
                match hi_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(10))
                {
                    Ok(job) => return Ok(job),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // High-priority lane gone: block on the background lane.
                        return lo_rx.lock().unwrap().recv().map_err(|_| ());
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Ok(job) = lo_rx.lock().unwrap().try_recv() {
                    return Ok(job);
                }
                match lo_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(50))
                {
                    Ok(job) => return Ok(job),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Background lane gone: block on the preload lane.
                        return hi_rx.lock().unwrap().recv().map_err(|_| ());
                    }
                    // Both lanes empty: loop around and poll `hi` again so a
                    // preload that landed during the wait is never stuck
                    // behind the next park.
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
        }

        for _ in 0..num_workers {
            let hi_rx = hi_rx.clone();
            let lo_rx = lo_rx.clone();
            let app_weak = app.as_weak();
            let catalog_gen = catalog_gen.clone();

            thread::spawn(move || {
                while let Ok(job) = next_job(&hi_rx, &lo_rx) {
                    // Drop stale jobs before fetching: fast scrolling queues
                    // downloads for generations the user already left; each
                    // would otherwise burn a full fetch + decode + disk
                    // write for an image that can never be shown.
                    if job.generation != catalog_gen.load(Ordering::Relaxed) {
                        continue;
                    }
                    let revision = image_settings_revision();
                    if let Some(display) =
                        display_pixels(&job.url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)))
                    {
                        let app_weak = app_weak.clone();
                        let catalog_gen = catalog_gen.clone();

                        let _ = slint::invoke_from_event_loop(move || {
                            // Drop stale posters (catalog changed since dispatch).
                            if job.generation != catalog_gen.load(Ordering::Relaxed)
                                || image_settings_revision() != revision
                            {
                                return;
                            }
                            if let Some(app) = app_weak.upgrade() {
                                let model = if job.library {
                                    app.get_library()
                                } else {
                                    app.get_catalog()
                                };
                                if model
                                    .row_data(job.index)
                                    .is_none_or(|row| row.poster_path.as_str() != job.url)
                                {
                                    return;
                                }
                                let img = Image::from_rgba8(display);
                                if job.library {
                                    app.invoke_set_library_poster(job.index as i32, img);
                                } else {
                                    app.invoke_set_card_poster(job.index as i32, img.clone());
                                }
                            }
                        });
                    } else if !job.library {
                        let _ = slint::invoke_from_event_loop(move || {
                            bridge::with_global_bridge(|bridge| {
                                bridge.recover_catalog_artwork(job.generation, job.index, &job.url);
                            });
                        });
                    }
                }
            });
        }
        PosterTx {
            hi: hi_tx,
            lo: lo_tx,
        }
    };

    // In-app player (same window, like the reference prototype): mpv underlay
    // on desktop, HTML5 video element on web. Same public API either way.
    #[cfg(feature = "desktop")]
    let after_first_frame = Rc::new(std::cell::RefCell::new(None::<Box<dyn FnOnce()>>));
    #[cfg(feature = "desktop")]
    let first_frame_job = after_first_frame.clone();
    #[cfg(feature = "desktop")]
    let player = crate::player::Player::setup_with_render_observer(&app, move |state| {
        if matches!(state, slint::RenderingState::AfterRendering)
            && let Some(job) = first_frame_job.borrow_mut().take()
        {
            // Leave the rendering notifier before starting workers or touching
            // models: the GL context still belongs to Slint here.
            slint::Timer::single_shot(Duration::ZERO, job);
        }
    });
    #[cfg(not(feature = "desktop"))]
    let player = crate::player::Player::setup(&app);
    #[cfg(feature = "desktop")]
    let desktop_window = desktop_window::DesktopWindow::restore(&app, &player);
    #[cfg(feature = "desktop")]
    {
        let memory = desktop_window.clone();
        let weak = app.as_weak();
        let p = player.clone();
        app.window().on_close_requested(move || {
            if let Some(app) = weak.upgrade() {
                memory.flush(&app, &p);
            }
            slint::CloseRequestResponse::HideWindow
        });
    }
    let downloads = DownloadCoordinator::new(app_data_dir().join("downloads"));

    #[cfg(feature = "desktop")]
    let mut bridge = Bridge::new(
        app.as_weak(),
        tx,
        catalog_gen.clone(),
        player.clone(),
        downloads,
    );
    #[cfg(not(feature = "desktop"))]
    let mut bridge = Bridge::new(
        app.as_weak(),
        catalog_gen.clone(),
        player.clone(),
        downloads,
    );

    bridge.wire_remote_playback();
    bridge.wire_party();
    bridge.wire_completion_prompts();

    bridge.last_stream = Arc::new(last_stream::Cache::open_synced(
        nova_config::app_cache_dir().join("last-stream-v1.json"),
    ));
    bridge.shared.lock().unwrap().restoring_models = true;

    bridge.initialize_tracking();

    // Publish the bridge for the Android camera scanner's JNI entry point
    // (android_qr.rs), which runs off the UI thread.
    crate::app::bridge::install_global_bridge(&bridge);

    // Handler wiring.
    let b = bridge.clone();
    app.on_addon_added(move |url| b.add_addon(&url));
    app.on_addon_zip_import_requested(super::addon_picker::pick_zip);

    let b = bridge.clone();
    app.on_addon_toggled(move |i| {
        if let Some(idx) = b.installed_row_index(i as usize) {
            b.toggle_addon(idx);
        }
    });

    let b = bridge.clone();
    app.on_addon_remove(move |i| {
        if let Some(idx) = b.installed_row_index(i as usize) {
            b.remove_addon_at(idx);
        }
    });

    let b = bridge.clone();
    app.on_addon_move_up(move |i| {
        if let Some(idx) = b.installed_row_index(i as usize) {
            b.move_addon(idx, -1);
        }
    });

    let b = bridge.clone();
    app.on_addon_move_down(move |i| {
        if let Some(idx) = b.installed_row_index(i as usize) {
            b.move_addon(idx, 1);
        }
    });

    let b = bridge.clone();
    app.on_addon_refresh(move |i| {
        if let Some(idx) = b.installed_row_index(i as usize) {
            b.refresh_addon(idx);
        }
    });

    let b = bridge.clone();
    app.on_addon_filter_changed(move || b.apply_addon_rows());

    // Settings → Addons → "Copy link": the install URL stays out of the row
    // (it used to be the row's description) and is copied from here instead.
    // Clipboard writes are best effort, so nothing is reported back to the UI.
    let b = bridge.clone();
    app.on_addon_copy_link(move |url| b.addon_copy_link(&url));

    // Open the addon's web configuration page in the system browser: the
    // desktop's default browser (`xdg-open`) or, on Android, an `ACTION_VIEW`
    // link intent without a MIME type (an explicit video MIME — what the
    // stream fallback uses — would exclude the browser).
    let b = bridge.clone();
    app.on_addon_capabilities_changed(move |url, streams, metadata| {
        b.apply_provider_capabilities(
            &url,
            nova_providers::ProviderCapabilities { streams, metadata },
            true,
        );
    });
    let b = bridge.clone();
    app.on_plugin_setting_changed({
        let b = bridge.clone();
        move |url, key, value| {
            b.change_plugin_setting(&url, &key, &value);
        }
    });
    app.on_plugin_settings_clear({
        let b = bridge.clone();
        move |url| {
            b.clear_plugin_settings(&url, false);
        }
    });
    app.on_plugin_cache_clear({
        let b = bridge.clone();
        move |url| {
            b.clear_plugin_settings(&url, true);
        }
    });
    app.on_addon_configure(move |url| {
        if nova_providers::supports_provider_configuration(&url) {
            let config = nova_providers::provider_capabilities(&url).unwrap_or_default();
            if let Some(app) = b.app() {
                let label = b
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .find(|a| a.url == url.as_str())
                    .map(|a| a.label.clone())
                    .unwrap_or_default();
                app.set_addon_config_capabilities(nova_providers::supports_capability_config(&url));
                app.set_plugin_settings_notice("".into());
                b.refresh_plugin_settings_ui(&url);
                app.set_addon_config_label(label.into());
                app.set_addon_config_url(url);
                app.set_addon_config_streams(config.streams);
                app.set_addon_config_metadata(config.metadata);
                app.set_addon_config_open(true);
            }
            return;
        }
        if let Err(e) = crate::player::open_browser(&url) {
            eprintln!("nova: could not open the addon configuration page {url}: {e}");
        }
    });

    app.on_license_source_requested(move |url| {
        if !url.starts_with("https://") {
            eprintln!("nova: refusing non-HTTPS license source URL");
            return;
        }
        if let Err(e) = crate::player::open_browser(&url) {
            eprintln!("nova: could not open license source page {url}: {e}");
        }
    });

    let b = bridge.clone();
    app.on_addon_picked(move |label| b.pick_addon(&label));

    let b = bridge.clone();
    app.on_type_picked(move |label| b.pick_type(&label));

    let b = bridge.clone();
    app.on_catalog_picked(move |label| b.pick_catalog(&label));

    let b = bridge.clone();
    app.on_genre_picked(move |label| b.pick_genre(&label));

    let b = bridge.clone();
    app.on_search_activated(move || b.activate_search_filters());

    let b = bridge.clone();
    app.on_search_filter_picked(move |kind, index| b.pick_search_filter(kind, index));

    let b = bridge.clone();
    app.on_search_edited(move |text| b.search_edited(&text));

    let b = bridge.clone();
    app.on_search_submitted(move |text| b.submit_search(&text));

    let b = bridge.clone();
    app.on_search_back_picked(move || b.close_search_results());
    bridge.load_search_history();
    let b = bridge.clone();
    app.on_search_history_cleared(move || b.clear_search_history());
    let b = bridge.clone();
    app.on_search_history_selected(move |index| b.replay_search_history(index));
    let b = bridge.clone();
    app.on_search_history_item_removed_at(move |index| b.remove_search_history_item_at(index));
    let b = bridge.clone();
    app.on_search_history_item_removed(move |query| b.remove_search_history_item(&query));

    let b = bridge.clone();
    app.on_item_selected(move |_id, idx| b.item_selected(idx as usize));

    let b = bridge.clone();
    app.on_load_more(move || b.load_more());

    // Poster unloading for endless-scroll sessions (Discover / Library).
    let b = bridge.clone();
    app.on_visible_range(move |f, l| b.visible_range(false, f, l));

    let b = bridge.clone();
    app.on_library_visible_range(move |f, l| b.visible_range(true, f, l));

    let b = bridge.clone();
    app.on_stream_picked(move |idx| b.stream_picked(idx as usize));
    let b = bridge.clone();
    app.on_stream_page_picked(move |page| b.stream_page_picked(page as usize));

    let b = bridge.clone();
    app.on_stream_action_requested(move |id| b.open_stream_action(id.as_ref()));

    let b = bridge.clone();
    app.on_stream_action_selected(move |id, action| b.stream_action_selected(id.as_ref(), action));

    let b = bridge.clone();
    app.on_stream_filter_picked(move |idx| b.stream_filter_picked(idx as usize));

    let b = bridge.clone();
    app.on_stream_selector_back(move || b.stream_selector_back());

    let b = bridge.clone();
    app.on_season_picked(move |idx| b.season_picked(idx as usize));

    let b = bridge.clone();
    app.on_episode_picked(move |idx| b.episode_picked(idx as usize));

    let b = bridge.clone();
    app.on_episode_page_picked(move |p| b.episode_page_picked(p));

    let b = bridge.clone();
    app.on_episode_toggle_watched(move |idx| b.episode_toggle_watched(idx as usize));

    let b = bridge.clone();
    app.on_episode_watch_action(move |idx, a| b.episode_watch_action(idx as usize, a));

    let b = bridge.clone();
    app.on_season_watch_action(move |idx, a| b.season_watch_action(idx as usize, a));

    let b = bridge.clone();
    app.on_episodes_back(move || b.episodes_back());

    let b = bridge.clone();
    app.on_detail_tab_picked(move |t| b.detail_tab_picked(t));

    let b = bridge.clone();
    app.on_episode_filter_changed(move |t| b.episode_filter_changed(t));

    let b = bridge.clone();
    app.on_watch_now(move || b.watch_now());

    let b = bridge.clone();
    app.on_tracking_connect(move |a0, a1, a2| {
        b.tracking_connect(a0, a1.to_string(), a2.to_string())
    });
    let b = bridge.clone();
    app.on_tracking_complete(move |a0, a1| b.tracking_complete(a0, a1.to_string()));
    let b = bridge.clone();
    app.on_tracking_disconnect(move |a0| b.tracking_disconnect(a0));
    let b = bridge.clone();
    app.on_tracking_cancel(move |a0| b.tracking_cancel(a0));
    let b = bridge.clone();
    app.on_tracking_reset(move || b.tracking_reset());
    let b = bridge.clone();
    app.on_stremio_connect(move || b.stremio_connect());
    let b = bridge.clone();
    app.on_stremio_cancel(move || b.stremio_cancel());
    let b = bridge.clone();
    app.on_stremio_start(move || b.stremio_start());
    let b = bridge.clone();
    app.on_stremio_sync(move || b.stremio_sync());
    let b = bridge.clone();
    app.on_stremio_automatic_changed(move |enabled| b.stremio_automatic(enabled));
    let b = bridge.clone();
    app.on_stremio_disconnect(move || b.stremio_disconnect());
    let b = bridge.clone();
    app.on_stremio_open_link(move || b.stremio_open_link());
    let b = bridge.clone();
    app.on_tracking_automatic(move |service, enabled| b.tracking_automatic(service, enabled));
    let b = bridge.clone();
    app.on_tracking_show(move || b.tracking_show());
    let b = bridge.clone();
    app.on_tracking_close(move || b.tracking_close());
    let b = bridge.clone();
    app.on_tracking_search(move |a0, a1| b.tracking_search(a0, a1.to_string()));
    let b = bridge.clone();
    app.on_tracking_pick(move |a0| b.tracking_pick(a0));
    let b = bridge.clone();
    app.on_tracking_preview(move |a0, a1, a2| {
        b.tracking_preview(a0.to_string(), a1.to_string(), a2.to_string())
    });
    let b = bridge.clone();
    app.on_tracking_confirm(move || b.tracking_confirm());
    let b = bridge.clone();
    app.on_tracking_accept_setup(move |revision, history| {
        b.tracking_accept_setup(revision.to_string(), history)
    });
    let b = bridge.clone();
    app.on_tracking_adjust_setup(move |index| b.tracking_adjust_setup(index));
    let b = bridge.clone();
    app.on_tracking_cancel_adjust(move || b.tracking_cancel_adjust());
    let b = bridge.clone();
    app.on_tracking_assign(move |row, value| b.tracking_assign(row, value.to_string()));
    let b = bridge.clone();
    app.on_tracking_edit(move |a0, a1, a2, a3, a4, a5, a6, a7| {
        b.tracking_edit(
            a0.to_string(),
            a1.to_string(),
            a2,
            a3.to_string(),
            a4,
            a5.to_string(),
            a6,
            a7.to_string(),
        )
    });
    let b = bridge.clone();
    app.on_tracking_action(move |a0, a1| b.tracking_action(a0.to_string(), a1));

    let b = bridge.clone();
    app.on_modal_closed(move || b.modal_closed());

    // Library callbacks.
    let b = bridge.clone();
    // The backend emits save feedback only after accepting the addition;
    // duplicate warnings may delay or cancel this request.
    app.on_add_to_library(move || b.toggle_current_in_library());
    let b = bridge.clone();
    app.on_library_duplicate_action(move |index| b.library_duplicate_action(index));

    let b = bridge.clone();
    app.on_detail_library_action(move |action| b.detail_library_action(action));

    let b = bridge.clone();
    app.on_remove_library_item(move |i| b.remove_library_at(i as usize));

    let b = bridge.clone();
    app.on_library_item_picked(move |i| b.open_library_item(i as usize));

    // Route callbacks share the same production wiring used by headless tests.
    wire_page_navigation(&app, &bridge);

    let b = bridge.clone();
    app.on_home_featured_picked(move || b.home_showcase_picked());

    let b = bridge.clone();
    app.on_home_featured_watch_now(move || b.home_showcase_watch_now());

    let b = bridge.clone();
    app.on_home_featured_step(move |d| b.home_showcase_step(d));

    let b = bridge.clone();
    app.on_continue_picked(move |i| b.continue_picked(i as usize));

    let b = bridge.clone();
    app.on_home_history_action(move |type_, id, action| {
        b.watch_history_action(&type_, &id, action)
    });

    let b = bridge.clone();
    app.on_continue_remove(move |i| b.continue_remove(i as usize));

    let b = bridge.clone();
    app.on_continue_enter(move |i| b.continue_enter(i as usize));

    let b = bridge.clone();
    app.on_upcoming_picked(move |i| b.upcoming_picked(i as usize));

    let b = bridge.clone();
    app.on_home_catalog_card_picked(move |i| b.home_catalog_card_picked(i as usize));

    // Home's Upcoming shortcut enters the same full planner as the navbar.
    let b = bridge.clone();
    app.on_upcoming_cal_open(move || {
        note_nav_switch(&b, 4);
        b.show_calendar_page();
    });

    let b = bridge.clone();
    app.on_upcoming_cal_close(move || b.upcoming_cal_close());

    let b = bridge.clone();
    app.on_upcoming_cal_month(move |d| b.upcoming_cal_month(d));

    let b = bridge.clone();
    app.on_upcoming_cal_pick(move |e| b.upcoming_cal_pick(e));

    let b = bridge.clone();
    app.on_upcoming_cal_activate(move || b.upcoming_cal_activate());

    let b = bridge.clone();
    app.on_library_watch_action(move |i, a| b.library_watch_action(i as usize, a));

    let b = bridge.clone();
    app.on_library_status_action(move |i, s| b.library_status_action(i as usize, s));

    let b = bridge.clone();
    app.on_calendar_changed(move || b.calendar_reset());
    let b = bridge.clone();
    app.on_calendar_agenda_more(move |d| b.calendar_agenda_more(d));
    let b = bridge.clone();
    app.on_calendar_artwork_visible(move |key, retained| {
        b.calendar_artwork_visible(key.as_str(), retained)
    });
    let b = bridge.clone();
    app.on_calendar_artwork_active_changed(move |active| b.calendar_artwork_active(active));
    let b = bridge.clone();
    app.on_downloads_page_artwork_visible(move |key, retained| {
        b.downloads_artwork_visible(key.as_str(), retained)
    });
    let b = bridge.clone();
    app.on_downloads_page_artwork_active_changed(move |active| b.downloads_artwork_active(active));

    let b = bridge.clone();
    app.on_calendar_step(move |d| b.calendar_step(d));
    let b = bridge.clone();
    app.on_calendar_today(move || b.calendar_today());
    let b = bridge.clone();
    app.on_calendar_pick(move |d| b.calendar_pick(d));
    let b = bridge.clone();
    app.on_calendar_release_picked(move |k| b.calendar_release_picked(k));
    let b = bridge.clone();
    app.on_downloads_page_query_changed(move |q| b.downloads_page_query_changed(&q));
    let b = bridge.clone();
    app.on_downloads_page_filter_changed(move |f| b.downloads_page_filter_changed(f));
    let b = bridge.clone();
    app.on_downloads_page_sort_changed(move |r| b.downloads_page_sort_changed(r));
    let b = bridge.clone();
    app.on_downloads_page_group_toggled(move |k| b.downloads_page_group_toggled(&k));
    let b = bridge.clone();
    app.on_downloads_page_action(move |k, a| b.downloads_page_action(&k, a));

    // System back on the root screen (HomePage `exit_to_background`). Only
    // Android synthesizes `Key.Back`; elsewhere this callback is unreachable
    // and stays a no-op. Backgrounding (not finishing) keeps the process —
    // sync engine and UI state — alive for the return.
    app.on_exit_to_background(|| {
        #[cfg(target_os = "android")]
        crate::player::move_task_to_back();
    });

    // Application-lifetime debounce, shared with the player's rate controls.
    // Capture first so page destruction and remote UI refresh cannot lose edits.
    let schedule_settings_save = bridge.wire_settings_autosave();
    let b = bridge.clone();
    app.on_onboarding_addon_added(move |url| b.add_addon(&url));
    let b = bridge.clone();
    app.on_onboarding_finished(move || b.finish_onboarding());

    let b = bridge.clone();
    app.on_clear_cache(move || b.clear_image_cache());

    let b = bridge.clone();
    app.on_reencode_cache(move || b.reencode_now());
    app.on_cancel_reencode(settings::cancel_cache_rewrite);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });

    // Torrent settings callbacks.
    let b = bridge.clone();
    app.on_torrents_changed(move || {
        let Some(app) = b.app() else { return };
        b.update_torrent_settings(|s| {
            s.enabled = app.get_torrent_enabled();
            s.max_mb = app.get_torrent_max_mb().max(0.0).round() as u64;
            s.down_limit_kbps = app.get_torrent_down_limit().max(0.0).round() as u32;
            s.no_cache = app.get_torrent_no_cache();
        });
    });

    let b = bridge.clone();
    app.on_torrent_dir_edited(move |dir| {
        let dir = dir.to_string();
        b.update_torrent_settings(|s| s.dir = dir);
    });

    let b = bridge.clone();
    app.on_download_auto_delete_watched_changed(move |enabled| {
        b.set_download_auto_delete_watched(enabled);
    });

    let b = bridge.clone();
    app.on_download_list_remove(move |id| {
        b.remove_download(id.as_str());
    });

    let b = bridge.clone();
    app.on_clear_torrent_cache(move || {
        if let Some(engine) = crate::torrent::engine() {
            engine.clear_cache();
        }
        b.refresh_torrent_disk_usage();
    });

    // Cross-device sync callbacks (Settings → Sync).
    let b = bridge.clone();
    app.on_sync_changed(move || {
        let Some(app) = b.app() else { return };
        b.sync_set_enabled(app.get_sync_enabled());
    });

    let b = bridge.clone();
    app.on_sync_add_peer(move |id| b.sync_add_peer(&id));

    let b = bridge.clone();
    app.on_sync_remove_peer(move |id| b.sync_remove_peer(&id));

    let b = bridge.clone();
    app.on_sync_now(move || b.sync_now());

    let b = bridge.clone();
    app.on_sync_copy_identity(move || b.sync_copy_identity());

    let b = bridge.clone();
    app.on_sync_device_name_edited(move |name| b.sync_device_name_edited(&name));

    let b = bridge.clone();
    app.on_sync_pair_confirm_changed(move |value| b.sync_pair_confirm_changed(value));

    let b = bridge.clone();
    app.on_sync_local_discovery_changed(move |value| b.sync_local_discovery_changed(value));

    let b = bridge.clone();
    app.on_sync_interval_picked(move |index| b.sync_interval_picked(index));

    let b = bridge.clone();
    app.on_sync_background_changed(move |enabled| b.sync_background_changed(enabled));

    let b = bridge.clone();
    app.on_sync_create_invite(move || b.sync_create_invite());

    let b = bridge.clone();
    app.on_sync_cancel_invite(move |id| b.sync_cancel_invite(&id));

    let b = bridge.clone();
    app.on_sync_copy_ticket(move |ticket| b.sync_copy_ticket(&ticket));

    let b = bridge.clone();
    app.on_sync_join_invite(move |ticket| b.sync_join_invite(&ticket));

    let b = bridge.clone();
    app.on_sync_scan_qr(move || b.sync_scan_qr());

    let b = bridge.clone();
    app.on_sync_respond_pair(move |id, accept| b.sync_respond_pair(&id, accept));

    let b = bridge.clone();
    app.on_new_episode_picked(move |i| b.new_episode_picked(i as usize));
    let b = bridge.clone();
    app.on_new_episode_enter(move |i| b.new_episode_enter(i as usize));
    let b = bridge.clone();
    app.on_new_episode_remove(move |i| b.new_episode_remove(i as usize));

    // Category callbacks.
    let b = bridge.clone();
    app.on_category_added(move |name| b.add_category_to_ui(&name));

    let b = bridge.clone();
    app.on_category_removed(move |i| b.remove_category_from_ui(i as usize));

    let b = bridge.clone();
    app.on_home_catalog_add_requested(move || b.home_catalog_add_requested());

    let b = bridge.clone();
    app.on_home_row_catalog_add_requested(move || b.home_row_catalog_add_requested());

    let b = bridge.clone();
    app.on_home_catalog_candidate_picked(move |i| b.home_catalog_candidate_picked(i));

    let b = bridge.clone();
    app.on_home_catalog_added(move |catalog, genre| b.home_catalog_added(catalog, genre));

    let b = bridge.clone();
    app.on_home_catalog_removed(move |i| b.home_catalog_removed(i as usize));

    let b = bridge.clone();
    app.on_home_row_catalog_removed(move |i| b.home_row_catalog_removed(i as usize));

    let b = bridge.clone();
    app.on_home_row_catalog_action(move |i, action| b.home_row_catalog_action(i as usize, action));

    let b = bridge.clone();
    app.on_toggle_entry_category(move |name| b.toggle_entry_category(&name));

    let b = bridge.clone();
    app.on_library_filter_picked(move |cat| b.filter_library(&cat));

    let b = bridge.clone();
    app.on_library_view_changed(move || b.library_view_changed());

    // Startup addon set: the persisted KV list first, plus any
    // extra URLs from NOVA_ADDONS / --addon. Both are installed from their
    // cached manifest when available (no server ping); the cache is only
    // populated by a live fetch the first time an addon is installed.
    addon_packages::initialize(&bridge);
    let persisted = read_persisted_addons();
    let mut extra = configured_addon_urls();
    if !owns_addon_setup && persisted.is_empty() && extra.is_empty() {
        // First run with no configuration at all: seed with the default.
        extra.push(DEFAULT_ADDON.to_string());
    }

    app.set_empty_hint(SharedString::from("Loading configured add-ons…"));
    app.set_searchable_hint(SharedString::from("Search"));
    app.set_searchable(true);
    app.set_desktop_decoder_available(cfg!(any(target_os = "windows", target_os = "linux")));
    app.set_hw_plus_available(!cfg!(target_os = "windows"));
    // Touch-driven platform: card menus open as bottom sheets on hold;
    // desktop keeps native right-click popups at the cursor.
    #[cfg(target_os = "android")]
    {
        app.set_touch_menus(true);
        // Android-only settings (e.g. the video decoder) are gated on this.
        app.set_is_android(true);
        // The camera QR scanner is Android-only.
        app.set_sync_qr_scan_available(true);
    }

    bridge.shared.lock().unwrap().loading_addons = true;
    for addon in &persisted {
        nova_providers::configure_provider_capabilities(
            &addon.url,
            addon.capabilities.unwrap_or_default(),
        );
        bridge.install_persisted(
            &addon.url,
            addon.enabled,
            addon.configure_ok,
            Some(addon.label.clone()),
        );
    }
    for url in &extra {
        bridge.install_persisted(url, true, None, None);
    }
    bridge.shared.lock().unwrap().loading_addons = false;
    bridge.persist_installed();
    addon_packages::reconcile(&bridge);

    // Restore preferences before deriving Home models: applying Home with
    // temporary defaults would prune the saved poster-rail catalog cache.
    // These settings also control the first artwork decode/encode requests.
    {
        let mut state = bridge.shared.lock().unwrap();
        state.cache_settings = read_settings();
        state.download_settings = read_download_settings();
        set_active_cache_settings(state.cache_settings.clone());
    }
    bridge.settings_to_ui();

    // Library: restore saved items and render them in the Library page.
    {
        let mut state = bridge.shared.lock().unwrap();
        state.entries = read_persisted_library();
        // Episode playback history (watched + resume positions).
        state.progress = read_progress_map();
        // Continue Watching removals (local mirror of the synced domain).
        state.continue_hidden = read_continue_hidden();
        // Backfill the sync-era library order for pre-sync saves (their saved
        // order becomes the `added_at_secs` order).
        if backfill_added_at(&mut state.entries) {
            let entries = state.entries.clone();
            drop(state);
            write_persisted_library(&entries);
        }
    }
    bridge.migrate_viewing_activity();
    // Only unapplied records need replay; snapshots already materialized
    // by previous launches/local writes are authoritative and cheap to load.
    bridge.restore_playback_preferences_schema();
    bridge.restore_last_stream_choice();
    if let Err(error) = plugin_settings::restore_retained_settings() {
        storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
    }
    bridge.seed_playback_default_settings();
    bridge.sync_seed();
    bridge.restore_sync_projection();
    bridge.sync_reconcile_progress_aliases();
    bridge.settings_to_ui();
    let image_gc_worker = cache_gc::start(&bridge);
    let persistence_worker = persistence::start();
    bridge.start_stremio();
    bridge.shared.lock().unwrap().restoring_models = false;
    bridge.apply_library_to_ui();
    // Home landing page: derive Continue Watching, New episodes and Upcoming
    // from viewing activity, playback history and cached episode air dates.
    bridge.rebuild_continue_list();
    bridge.rebuild_upcoming_list();
    bridge.apply_home_to_ui();
    bridge.dispatch_continue_posters();
    bridge.dispatch_upcoming_posters();
    bridge.dispatch_home_catalog_posters();
    // Restore selected featured catalogs from local storage before the first
    // frame, then refresh in the background. Pending manifests do not block
    // cached titles; their completion retries the corresponding live fetch.
    bridge.refresh_home_showcase();
    bridge.refresh_home_catalog_rows();

    // Torrent settings: restore + mirror into the runtime cache read by the
    // engine and the player-close cleanup path.
    {
        let torrent = read_torrent_settings();
        *CURRENT_TORRENT_SETTINGS.lock().unwrap() = torrent;
    }
    bridge.torrent_settings_to_ui();

    #[cfg(not(feature = "desktop"))]
    let sync_settings = nova_sync::read_settings();
    #[cfg(target_os = "android")]
    crate::app::android_bg::set_periodic_sync(
        sync_settings.enabled && sync_settings.background_enabled,
    );
    #[cfg(not(feature = "desktop"))]
    {
        sync_torrent_engine(&active_torrent_settings());
        bridge.downloads.start();
        if sync_settings.enabled {
            bridge.start_sync();
        }
    }
    #[cfg(feature = "desktop")]
    {
        let b = bridge.clone();
        *after_first_frame.borrow_mut() = Some(Box::new(move || {
            b.start_torrent_after_first_frame();
            b.downloads.start();
            if nova_sync::read_settings().enabled {
                b.start_sync();
            }
        }));
    }

    // Local and remote commands share the receiver's behavior and cleanup.
    use nova_playback_api::PlaybackAction;
    let b = bridge.clone();
    app.on_toggle_pause(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::TogglePause);
    });
    let b = bridge.clone();
    app.on_seek(move |position| {
        let _ = b.dispatch_playback_action(PlaybackAction::SeekAbsolute(position as f64));
    });
    let b = bridge.clone();
    app.on_seek_relative(move |delta| {
        let _ = b.dispatch_playback_action(PlaybackAction::SeekRelative(delta as f64));
    });
    let b = bridge.clone();
    app.on_close_player(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::Stop);
    });
    let b = bridge.clone();
    app.on_next_episode_streams(move |token| {
        let _ = b.dispatch_playback_action(PlaybackAction::NextEpisode {
            token: token.to_string(),
        });
    });
    let b = bridge.clone();
    app.on_next_episode_dismiss(move |token| {
        let _ = b.dispatch_playback_action(PlaybackAction::DismissNextEpisode {
            token: token.to_string(),
        });
    });
    let b = bridge.clone();
    app.on_playback_end_stay(move || b.playback_end_stay());
    let b = bridge.clone();
    app.on_playback_end_next(move || b.playback_end_next());
    let b = bridge.clone();
    app.on_auto_next_play(move || b.play_automatic_next());
    let b = bridge.clone();
    app.on_playback_end_details(move || b.playback_end_details());
    let b = bridge.clone();
    app.on_resume_choice(move |resume| {
        let _ = b.dispatch_playback_action(PlaybackAction::Resume(resume));
    });
    let b = bridge.clone();
    app.on_resume_cancel(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::CancelResume);
    });
    let b = bridge.clone();
    app.on_take_screenshot(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::Screenshot);
    });
    let b = bridge.clone();
    app.on_set_volume(move |volume| {
        let _ = b.dispatch_playback_action(PlaybackAction::SetVolume(volume as f64));
    });
    let b = bridge.clone();
    app.on_volume_step(move |delta| {
        let _ = b.dispatch_playback_action(PlaybackAction::VolumeStep(delta as f64));
    });
    let b = bridge.clone();
    app.on_toggle_mute(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::ToggleMute);
    });

    // Settings edits defaults; OSD edits only the active title's override.
    let b = bridge.clone();
    let save_subtitle_defaults = schedule_settings_save.clone();
    app.on_default_subtitle_style_changed(move |index, value| {
        b.edit_subtitle_style(index, value, true);
        save_subtitle_defaults();
    });
    let b = bridge.clone();
    app.on_subtitle_style_changed(move |index, value| b.edit_subtitle_style(index, value, false));
    let b = bridge.clone();
    app.on_playback_speed_changed(move |speed| {
        let _ = b.dispatch_playback_action(PlaybackAction::SetSpeed(speed as f64));
    });
    let b = bridge.clone();
    app.on_default_playback_speed_stepped(move |d| {
        let speed = nova_config::active_cache_settings().playback_speed;
        b.apply_default_playback_speed(nova_config::quantize_playback_speed(
            speed + d as f32 * nova_config::PLAYBACK_SPEED_STEP,
        ));
        schedule_settings_save();
    });
    let b = bridge.clone();
    app.on_playback_language_search(move |query, subtitles| {
        b.filter_playback_languages(query.as_str(), subtitles)
    });
    let b = bridge.clone();
    app.on_playback_language_picked(move |code, subtitles| {
        b.pick_default_playback_language(code.as_str(), subtitles)
    });
    let b = bridge.clone();
    app.on_playback_language_edited(move |code, action, subtitles| {
        b.edit_default_playback_language(code.as_str(), action, subtitles)
    });
    let b = bridge.clone();
    app.on_restore_playback_default(move |control| {
        if control == 3 {
            b.restore_playback_default(control);
            return;
        }
        let action = match control {
            0 => PlaybackAction::ResetAudioTrack,
            1 => PlaybackAction::ResetSubtitleTrack,
            2 => PlaybackAction::ResetSpeed,
            _ => return,
        };
        let _ = b.dispatch_playback_action(action);
    });

    // Transient rate preview (press-and-hold 2×): session only, never stored.
    let b = bridge.clone();
    app.on_playback_speed_preview(move |_speed| {
        let _ = b.dispatch_playback_action(PlaybackAction::BeginSpeedPreview);
    });
    let b = bridge.clone();
    app.on_playback_speed_preview_end(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::EndSpeedPreview);
    });

    // Android swipe gestures (brightness/volume); elsewhere these callbacks
    // never fire (the Slint side gates swipes on `is_android`).
    let b = bridge.clone();
    app.on_android_brightness_step(move |steps| {
        let _ = b.dispatch_playback_action(PlaybackAction::BrightnessStep(steps as f64 * 0.05));
    });

    let b = bridge.clone();
    app.on_android_volume_step(move |dir| {
        let _ = b.dispatch_playback_action(PlaybackAction::SystemVolumeStep(dir));
    });

    let b = bridge.clone();
    app.on_playback_speed_stepped(move |d| {
        let _ = b.dispatch_playback_action(PlaybackAction::SpeedStep(d));
    });

    let b = bridge.clone();
    app.on_pick_audio_track(move |idx| {
        let _ = b.dispatch_playback_action(PlaybackAction::SelectAudioTrack(idx));
    });
    let b = bridge.clone();
    app.on_pick_sub_track(move |idx| {
        let _ = b.dispatch_playback_action(PlaybackAction::SelectSubtitleTrack(idx));
    });

    let b = bridge.clone();
    app.on_pick_decoder(move |idx| {
        let _ = b.dispatch_playback_action(PlaybackAction::SetDecoder(idx));
    });

    // The settings modal opened — query mpv's live decoder state right
    // away so the submenu shows what is actually running, without waiting for
    // the 250 ms tick.
    #[cfg(any(target_os = "android", target_os = "windows", target_os = "linux"))]
    {
        let p = player.clone();
        app.on_settings_opened(move || p.refresh_decoder());
    }
    #[cfg(not(any(target_os = "android", target_os = "windows", target_os = "linux")))]
    {
        app.on_settings_opened(|| {});
    }

    let b = bridge.clone();
    app.on_toggle_fullscreen(move || {
        let _ = b.dispatch_playback_action(PlaybackAction::ToggleFullscreen);
    });

    #[cfg(feature = "desktop")]
    {
        let weak = app.as_weak();
        app.on_osd_opacity_changed(move |_| {
            if let Some(app) = weak.upgrade() {
                desktop_window::sync_cursor(&app);
            }
        });
    }

    let p = player.clone();
    app.on_subtitle_clearance_changed(move |fraction, opacity| {
        p.set_subtitle_clearance(fraction, opacity)
    });

    // Hover previews: the timestamp under the cursor (seekbar) and the
    // volume percentage (volume slider). Formatting lives in Rust because
    // Slint has no int→string conversion for bindings.
    app.on_format_time(move |secs| SharedString::from(crate::player::format_time(secs as f64)));
    app.on_format_volume(move |v| SharedString::from(format!("{:.0}%", v.max(0.0))));

    let b = bridge.clone();
    app.on_tracks_popup_opened(move |kind| {
        b.player.refresh_tracks();
        if kind == 2 {
            b.retry_addon_subtitles();
        }
    });

    // OSD auto-hide: show on activity, hide after inactivity — 3 s while
    // playing, 5 s while paused so the controls linger for resume.
    // Android keeps paused controls visible so Home/screen-off restoration
    // presents Play until the user resumes (loading keeps the bar up too).
    let osd_timer = Rc::new(slint::Timer::default());
    {
        let osd_timer = osd_timer.clone();
        let app_weak = app.as_weak();
        #[cfg(target_os = "android")]
        let osd_player = player.clone();
        app.on_osd_mouse_moved(move || {
            if let Some(app) = app_weak.upgrade() {
                app.set_osd_visible(true);
                #[cfg(feature = "desktop")]
                desktop_window::sync_cursor(&app);
                // Android: the OSD is being woken, so drop the content
                // frame-rate request now — let the panel go back to its normal
                // maximum for the fade/controls instead of waiting for a tick.
                #[cfg(target_os = "android")]
                osd_player.notify_osd_activity();
                // Restart the countdown (longer grace while paused).
                let timeout = match app.get_is_paused() {
                    true => Duration::from_secs(5),
                    false => Duration::from_secs(3),
                };
                let aw = app_weak.clone();
                osd_timer.start(slint::TimerMode::SingleShot, timeout, move || {
                    if let Some(app) = aw.upgrade()
                        && (!app.get_is_android() || !app.get_is_paused())
                    {
                        app.set_osd_visible(false);
                    }
                });
            }
        });
    }

    // 250 ms position/buffer state mirror while the player overlay is up, plus episode
    // playback tracking (resume seeks + throttled progress saves + history
    // finalize on close). Both run on the UI thread.
    let _timer = slint::Timer::default();
    let tick_player = player.clone();
    let tick_bridge = bridge.clone();
    #[cfg(feature = "desktop")]
    let tick_window = desktop_window.clone();
    let mut episode_day = today_days();
    _timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            if let Some(app) = tick_bridge.app() {
                #[cfg(feature = "desktop")]
                tick_window.tick(&app, &tick_player);
                app.set_persistence_failed(
                    storage::last_error().is_some() || persistence::failed(),
                );
            }
            tick_bridge.refresh_stremio_surfaces_if_visible();
            let today = today_days();
            if today != episode_day {
                episode_day = today;
                tick_bridge.refresh_library_progress_ui();
            }
            let was_open = tick_bridge.app().is_some_and(|app| app.get_player_open());
            tick_player.tick();
            tick_bridge.poll_addon_subtitles();
            if was_open
                && tick_bridge.app().is_some_and(|app| !app.get_player_open())
                && tick_bridge
                    .shared
                    .lock()
                    .unwrap()
                    .playback_intent
                    .as_ref()
                    .is_some_and(|intent| intent.in_app)
            {
                tick_bridge.complete_playback_close();
            }
            tick_bridge.remote_tick();
            tick_bridge.party_tick();
            tick_bridge.flush_playback_preferences(false);
            tick_bridge.note_player_progress_from_ui();
            tick_bridge.refresh_playback_end_menu();
            tick_bridge.note_torrent_progress_from_ui();
            tick_bridge.refresh_download_rows();
            if tick_bridge
                .app()
                .is_some_and(|app| app.get_show_downloads())
            {
                tick_bridge.refresh_downloads_page();
            }
        },
    );

    // Keep the Settings → Sync status line fresh while that subpage is open
    // (background syncs complete on the tokio runtime; this mirrors them).
    let sync_timer = slint::Timer::default();
    let sync_tick_bridge = bridge.clone();
    sync_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(2),
        move || {
            sync_tick_bridge.replay_sync_projection();
            addon_packages::reconcile(&sync_tick_bridge);
            // Android doesn't surface connectivity changes to native code, so
            // poll the active network and hand changes to iroh.
            #[cfg(target_os = "android")]
            if crate::player::network_changed() {
                nova_sync::notify_network_change();
            }
            if sync_tick_bridge
                .app()
                .map(|a| a.get_show_settings())
                .unwrap_or(false)
            {
                sync_tick_bridge.sync_status_to_ui();
            }
        },
    );

    // Testing convenience: set NOVA_AUTOPLAY_URL to auto-open a stream shortly
    // after startup (useful for quick manual/headless checks of the player).
    // Native only.
    if let Ok(url) = std::env::var("NOVA_AUTOPLAY_URL") {
        let autoplay_bridge = bridge.clone();
        let autoplay_timer = slint::Timer::default();
        autoplay_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(1500),
            move || {
                eprintln!("[autoplay] opening {url}");
                if !autoplay_bridge.open_player(url.clone()) {
                    eprintln!("[autoplay] playback unavailable");
                }
            },
        );
        std::mem::forget(autoplay_timer);
    }

    crate::web_log("nova: startup complete, entering event loop");
    let result = app.run();
    #[cfg(feature = "desktop")]
    desktop_window.flush(&app, &player);
    bridge.persist_outgoing_playback();
    bridge.party_leave();
    bridge.shutdown_tracking();
    bridge.shutdown_stremio();
    drop(persistence_worker);
    drop(image_gc_worker);
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use slint::platform::{PointerEventButton, WindowEvent};

    fn tap(app: &AppWindow, element: ElementHandle) {
        let position = element.absolute_position();
        let size = element.size();
        let position = slint::LogicalPosition::new(
            position.x + size.width / 2.0,
            position.y + size.height / 2.0,
        );
        for event in [
            WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            },
            WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            },
        ] {
            app.window().dispatch_event(event);
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }

    fn more_link(app: &AppWindow, label: &str) -> ElementHandle {
        ElementHandle::find_by_accessible_label(app, label)
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
            .unwrap_or_else(|| panic!("missing More link {label}"))
    }

    fn assert_one_rail(app: &AppWindow) {
        assert_eq!(
            ElementHandle::find_by_element_type_name(app, "SideNav").count(),
            1,
            "production routing keeps one stationary desktop rail"
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(app, "BottomNav").count(),
            0
        );
    }

    #[test]
    fn production_navigation_preserves_origin_and_stationary_chrome() {
        // Slint and storage are process globals. Run the real callback wiring
        // in an isolated process with an empty local library and no addons,
        // rather than replacing the Bridge's model refreshes with mock flags.
        const TEST_DIR: &str = "NOVA_PRODUCTION_NAV_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-production-nav-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args([
                    "--exact",
                    "app::run::tests::production_navigation_preserves_origin_and_stationary_chrome",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            fs::remove_dir_all(root).unwrap();
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
        let app = AppWindow::new().unwrap();
        app.global::<crate::Anim>().set_enabled(false);
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            // No poster workers consume requests in this empty fixture.
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
        assert!(bridge.shared.lock().unwrap().installed.is_empty());
        wire_page_navigation(&app, &bridge);
        app.show().unwrap();

        for width in [700, 1100] {
            app.window().set_size(slint::PhysicalSize::new(width, 800));
            for (index, slot, label, component) in [
                (4, 3, "Calendar", "CalendarPage"),
                (5, 4, "Downloads", "DownloadsPage"),
            ] {
                app.window().set_size(slint::PhysicalSize::new(width, 800));
                app.invoke_navigate_section(3);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                assert!(app.get_more_open());
                // Mouse rail selection reaches the exact production global
                // NavState callback and must establish a direct Back origin.
                let direct = ElementHandle::find_by_element_type_name(&app, "SideItem")
                    .nth(slot)
                    .unwrap();
                tap(&app, direct);
                assert!(!app.get_more_open(), "{label} must close desktop More");
                assert_eq!(current_nav(&app), index);
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, component).count(),
                    1
                );
                assert_one_rail(&app);
                app.set_system_back_request(app.get_system_back_request() + 1);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                assert!(app.get_show_home(), "direct {label} Back returns Home");
                assert!(!app.get_more_open());

                // Calendar/Downloads are More links only on narrow layouts;
                // the wide shortcut above deliberately has a direct origin.
                app.window().set_size(slint::PhysicalSize::new(699, 800));
                app.invoke_navigate_section(3);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                let bar = ElementHandle::find_by_element_type_name(&app, "BottomNav")
                    .next()
                    .unwrap()
                    .absolute_position();
                tap(&app, more_link(&app, label));
                assert_eq!(current_nav(&app), index);
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, "SideNav").count(),
                    0
                );
                app.set_system_back_request(app.get_system_back_request() + 1);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                assert!(app.get_more_open(), "More-link {label} Back returns More");
                assert!(!app.get_show_home());
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
                    1
                );
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, "BottomNav")
                        .next()
                        .unwrap()
                        .absolute_position(),
                    bar
                );
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, component).count(),
                    0
                );
                app.set_system_back_request(app.get_system_back_request() + 1);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                assert!(app.get_show_home());
                assert!(!app.get_more_open());
            }
        }
        // No startup addon/tracking/download workers were started.
        assert!(bridge.shared.lock().unwrap().installed.is_empty());
        assert!(bridge.downloads.jobs().is_empty());
    }
}

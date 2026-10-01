//! Application entry point: window setup and callback wiring.
use super::*;

/// Nav index of the page currently shown: 0 Home, 1 Discover, 2 Library,
/// 3 Settings (Discover is the fallback page).
fn current_nav(app: &AppWindow) -> i32 {
    if app.get_show_home() {
        0
    } else if app.get_show_library() {
        2
    } else if app.get_show_settings() {
        3
    } else {
        1
    }
}

/// Publish the section a page switch is leaving, before the page flags flip.
///
/// `NavState.from` is what the switch destroys: the nav bar lives inside the
/// page, so the new bar cannot see the old selection by itself. It uses this
/// to pop only an incoming icon, never on a rebuild of the same page (Settings
/// → Look and feel → Navigation feedback). The highlight snaps immediately.
fn note_nav_switch(bridge: &Bridge, next: i32) {
    let Some(app) = bridge.app() else {
        return;
    };
    let from = current_nav(&app);
    if from != next {
        app.global::<crate::NavState>().set_from(from);
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    crate::diagnostics::init();
    // Native desktop: in-app playback renders through mpv's OpenGL underlay
    // (src/player.rs). Like the reference prototype, the app runs on the
    // default femtovg GL renderer (it exposes GraphicsAPI::NativeOpenGL to
    // the rendering notifier); skia's presentation path is *not* compatible
    // with drawing mpv into the default framebuffer at BeforeRendering. Set
    // the renderer before the first Slint window is created (which
    // initializes the platform). Never on Android: it uses Slint's
    // android-activity backend, which these overrides would break.
    #[cfg(not(target_os = "android"))]
    unsafe {
        std::env::set_var("SLINT_BACKEND", "winit");
        std::env::set_var("SLINT_RENDERER", "femtovg");
    }

    let app = AppWindow::new()?;
    app.set_license_catalog(OPEN_SOURCE_LICENSE_CATALOG.into());
    app.set_license_sources(
        Rc::new(VecModel::<LicenseSource>::from(
            open_source_license_sources(),
        ))
        .into(),
    );
    storage::init_at(&app_data_dir());
    crate::web_log("nova: window created");

    // Player scrim: femtovg's gradient fills render as flat fills in this
    // environment, so the 220px fade is baked into a 1×220 alpha-ramp image
    // (black with a quadratic ease from 0 to ~69% alpha) stretched over the
    // OSD area. Image stretching is exact and renderer-independent.
    {
        const H: usize = 220;
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(1, H as u32);
        for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
            let t = i as f32 / (H - 1) as f32;
            *px = Rgba8Pixel::new(0, 0, 0, (175.0 * t * t) as u8);
        }
        app.set_scrim_image(Image::from_rgba8(buf));
    }

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
    // Poster download pipeline (off-thread decode + per-generation cache).
    // Dual intake: `hi` carries near-viewport (pre)loads from `visible_range`,
    // `lo` carries the background full-grid sweeps. Workers always drain `hi`
    // first so scrolling into new rows never waits behind far-away images.
    let (poster_cache, tx): (PosterCache, PosterTx) = {
        let poster_cache: PosterCache = Arc::new(Mutex::new(PosterStore::new(400)));
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
            let cache_clone = poster_cache.clone();
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
                    if let Some(display) = display_pixels(&job.url, Some(DISPLAY_POSTER_SIDE)) {
                        {
                            let mut cache = cache_clone.lock().unwrap();
                            cache.insert((job.generation, job.index), display.clone());
                        }

                        let app_weak = app_weak.clone();
                        let catalog_gen = catalog_gen.clone();

                        let _ = slint::invoke_from_event_loop(move || {
                            // Drop stale posters (catalog changed since dispatch).
                            if job.generation != catalog_gen.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(app) = app_weak.upgrade() {
                                let img = Image::from_rgba8(display);
                                if job.library {
                                    app.invoke_set_library_poster(job.index as i32, img);
                                } else {
                                    app.invoke_set_card_poster(job.index as i32, img.clone());
                                    // The detail modal may be showing this same item;
                                    // fill its poster too (it was reset on open).
                                    if app.get_modal_visible()
                                        && app.get_selected_index() == job.index as i32
                                    {
                                        app.set_selected_poster(img);
                                    }
                                }
                            }
                        });
                    }
                }
            });
        }
        (
            poster_cache,
            PosterTx {
                hi: hi_tx,
                lo: lo_tx,
            },
        )
    };

    // In-app player (same window, like the reference prototype): mpv underlay
    // on desktop, HTML5 video element on web. Same public API either way.
    let player = crate::player::Player::setup(&app);
    let downloads = DownloadCoordinator::new(app_data_dir().join("downloads"));

    #[cfg(feature = "desktop")]
    let bridge = Bridge::new(
        app.as_weak(),
        tx,
        poster_cache.clone(),
        catalog_gen.clone(),
        player.clone(),
        downloads,
    );
    #[cfg(not(feature = "desktop"))]
    let bridge = Bridge::new(
        app.as_weak(),
        catalog_gen.clone(),
        player.clone(),
        downloads,
    );

    // Publish the bridge for the Android camera scanner's JNI entry point
    // (android_qr.rs), which runs off the UI thread.
    crate::app::bridge::install_global_bridge(&bridge);

    // Handler wiring.
    let b = bridge.clone();
    app.on_addon_added(move |url| b.add_addon(&url));

    let b = bridge.clone();
    app.on_addon_toggled(move |i| b.toggle_addon(i as usize));

    let b = bridge.clone();
    app.on_addon_remove(move |i| b.remove_addon_at(i as usize));

    let b = bridge.clone();
    app.on_addon_move_up(move |i| b.move_addon(i as usize, -1));

    let b = bridge.clone();
    app.on_addon_move_down(move |i| b.move_addon(i as usize, 1));

    let b = bridge.clone();
    app.on_addon_refresh(move |i| b.refresh_addon(i as usize));

    // Settings → Addons → "Copy link": the install URL stays out of the row
    // (it used to be the row's description) and is copied from here instead.
    // Clipboard writes are best effort, so nothing is reported back to the UI.
    let b = bridge.clone();
    app.on_addon_copy_link(move |url| b.addon_copy_link(&url));

    // Open the addon's web configuration page in the system browser: the
    // desktop's default browser (`xdg-open`) or, on Android, an `ACTION_VIEW`
    // link intent without a MIME type (an explicit video MIME — what the
    // stream fallback uses — would exclude the browser).
    app.on_addon_configure(move |url| {
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
    app.on_search_edited(move |text| b.search_edited(&text));

    let b = bridge.clone();
    app.on_search_submitted(move |text| b.submit_search(&text));

    let b = bridge.clone();
    app.on_search_back_picked(move || b.close_search_results());
    bridge.load_search_history();
    let b = bridge.clone();
    app.on_search_history_cleared(move || b.clear_search_history());
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
    app.on_modal_closed(move || b.modal_closed());

    // Library callbacks.
    let b = bridge.clone();
    app.on_add_to_library(move || b.toggle_current_in_library());

    let b = bridge.clone();
    app.on_remove_library_item(move |i| b.remove_library_at(i as usize));

    let b = bridge.clone();
    app.on_library_item_picked(move |i| b.open_library_item(i as usize));

    // ---- Page navigation (nav bar / rail) --------------------------------
    // These four callbacks are the only way a page switch happens, so the
    // switch bookkeeping lives here, *before* the page flags flip: each page
    // hosts its own nav bar, which is rebuilt by the switch and therefore
    // reads the section we are leaving from `NavState` to distinguish a real
    // navigation click from a same-page rebuild before popping the new icon.
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
    app.on_home_featured_picked(move || b.home_showcase_picked());

    let b = bridge.clone();
    app.on_home_featured_watch_now(move || b.home_showcase_watch_now());

    let b = bridge.clone();
    app.on_home_featured_step(move |d| b.home_showcase_step(d));

    let b = bridge.clone();
    app.on_continue_picked(move |i| b.continue_picked(i as usize));

    let b = bridge.clone();
    app.on_continue_remove(move |i| b.continue_remove(i as usize));

    let b = bridge.clone();
    app.on_continue_enter(move |i| b.continue_enter(i as usize));

    let b = bridge.clone();
    app.on_upcoming_picked(move |i| b.upcoming_picked(i as usize));

    // Home → Upcoming calendar (month view in the "see all" subpage).
    let b = bridge.clone();
    app.on_upcoming_cal_open(move || b.upcoming_cal_open());

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
    app.on_settings_picked(move || {
        note_nav_switch(&b, 3);
        b.show_settings_page();
    });

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
    app.on_clear_cache(move || b.clear_image_cache());

    let b = bridge.clone();
    app.on_reencode_cache(move || b.reencode_now());

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

    // Category callbacks.
    let b = bridge.clone();
    app.on_category_added(move |name| b.add_category_to_ui(&name));

    let b = bridge.clone();
    app.on_category_removed(move |i| b.remove_category_from_ui(i as usize));

    let b = bridge.clone();
    app.on_home_catalog_add_requested(move || b.home_catalog_add_requested());

    let b = bridge.clone();
    app.on_home_catalog_candidate_picked(move |i| b.home_catalog_candidate_picked(i));

    let b = bridge.clone();
    app.on_home_catalog_added(move |catalog, genre| b.home_catalog_added(catalog, genre));

    let b = bridge.clone();
    app.on_home_catalog_removed(move |i| b.home_catalog_removed(i as usize));

    let b = bridge.clone();
    app.on_toggle_entry_category(move |name| b.toggle_entry_category(&name));

    let b = bridge.clone();
    app.on_library_filter_picked(move |cat| b.filter_library(&cat));

    // Startup addon set: the persisted KV list first, plus any
    // extra URLs from NOVA_ADDONS / --addon. Both are installed from their
    // cached manifest when available (no server ping); the cache is only
    // populated by a live fetch the first time an addon is installed.
    let persisted = read_persisted_addons();
    let mut extra = configured_addon_urls();
    if persisted.is_empty() && extra.is_empty() {
        // First run with no configuration at all: seed with the default.
        extra.push(DEFAULT_ADDON.to_string());
    }

    app.set_empty_hint(SharedString::from("Loading configured add-ons…"));
    app.set_searchable_hint(SharedString::from("Search"));
    app.set_searchable(true);
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
    bridge.apply_library_to_ui();
    // Home landing page: derive Continue Watching + Upcoming from playback
    // history and cached episode air dates.
    bridge.rebuild_continue_list();
    bridge.rebuild_upcoming_list();
    bridge.apply_home_to_ui();
    bridge.dispatch_continue_posters();
    bridge.dispatch_upcoming_posters();

    // Settings: restore the image-cache preferences and mirror them to the
    // Settings page (also used by the cache encode pipeline).
    {
        let mut state = bridge.shared.lock().unwrap();
        state.cache_settings = read_settings();
        state.download_settings = read_download_settings();
        set_active_cache_settings(state.cache_settings.clone());
    }
    bridge.settings_to_ui();
    // Addon manifests may still be loading asynchronously; this first fetch
    // uses every currently available selected catalog, and each later
    // manifest completion invalidates/rebuilds it as needed.
    bridge.refresh_home_showcase();

    // Torrent settings: restore + mirror into the runtime cache read by the
    // engine and the player-close cleanup path.
    {
        let torrent = read_torrent_settings();
        *CURRENT_TORRENT_SETTINGS.lock().unwrap() = torrent;
    }
    bridge.torrent_settings_to_ui();

    // Embedded BitTorrent streaming (native only; the web build has no
    // librqbit): only start the process-wide session when torrent playback
    // is enabled, so the disabled default costs no threads or sockets. The
    // engine is started lazily if the user enables it later.
    sync_torrent_engine(&active_torrent_settings());
    bridge.downloads.start();

    // Cross-device sync (opt-in). Binds no socket unless enabled in Settings.
    // Android also reconciles the periodic JobScheduler job with the enable +
    // background flags, so a disabled sync/background cancels a stale job.
    let sync_settings = nova_sync::read_settings();
    // Initialize/migrate socket-free mutation ownership before networking or
    // projection. Headless changes remain authoritative over old snapshots.
    bridge.sync_seed();
    if let Ok(store) = nova_sync::local_store() {
        let domains = store.lock().unwrap().domains();
        bridge.sync_apply(domains);
    }
    #[cfg(target_os = "android")]
    crate::app::android_bg::set_periodic_sync(
        sync_settings.enabled && sync_settings.background_enabled,
    );
    if sync_settings.enabled {
        bridge.start_sync();
    }

    // In-app player overlay callbacks (same window).
    let p = player.clone();
    app.on_toggle_pause(move || p.toggle());

    let p = player.clone();
    app.on_seek(move |pos| p.seek(pos));

    let p = player.clone();
    let b = bridge.clone();
    app.on_close_player(move || {
        p.close();
        // A swipe-dimmed window must not leak into the catalog.
        #[cfg(target_os = "android")]
        crate::app::android_player::player_brightness_reset();
        if let Some(engine) = crate::torrent::engine() {
            engine.on_playback_stopped(&active_torrent_settings());
        }
        b.shared.lock().unwrap().active_torrent = None;
        b.refresh_torrent_disk_usage();
    });

    let b = bridge.clone();
    app.on_resume_choice(move |resume| b.resume_prompt_choice(resume));

    let b = bridge.clone();
    app.on_resume_cancel(move || b.cancel_resume_prompt());

    let p = player.clone();
    app.on_set_volume(move |v| p.set_volume(v));

    let p = player.clone();
    app.on_toggle_mute(move || p.toggle_mute());

    // Playback rate: one per-device value, edited from the player's settings
    // panel (here) and Settings → Player. Both paths store it and push it into
    // the running session immediately; the KV write is debounced so a slider
    // drag persists once when the user pauses.
    let schedule_speed_save = schedule_settings_save;

    let b = bridge.clone();
    let schedule_speed_save1 = schedule_speed_save.clone();
    app.on_playback_speed_changed(move |speed| {
        b.apply_playback_speed(speed);
        schedule_speed_save1();
    });

    // Transient rate preview (press-and-hold 2×): session only, never stored.
    let b = bridge.clone();
    app.on_playback_speed_preview(move |speed| b.preview_playback_speed(speed));

    // Android swipe gestures (brightness/volume); elsewhere these callbacks
    // never fire (the Slint side gates swipes on `is_android`).
    let b = bridge.clone();
    app.on_android_brightness_step(move |steps| b.android_brightness_step(steps));

    let b = bridge.clone();
    app.on_android_volume_step(move |dir| b.android_volume_step(dir));

    let b = bridge.clone();
    app.on_playback_speed_stepped(move |d| {
        b.nudge_playback_speed(d);
        schedule_speed_save();
    });

    let p = player.clone();
    app.on_pick_audio_track(move |idx| p.pick_audio_track(idx));

    let p = player.clone();
    app.on_pick_sub_track(move |idx| p.pick_sub_track(idx));

    // Android runtime decoder picker (settings modal → decoder submenu). A
    // no-op where the gear/modal is hidden.
    #[cfg(target_os = "android")]
    {
        let p = player.clone();
        app.on_pick_decoder(move |idx| p.pick_decoder(idx));
    }
    #[cfg(not(target_os = "android"))]
    {
        app.on_pick_decoder(|_idx| {});
    }

    // Android: the settings modal opened — query mpv's live decoder state right
    // away so the submenu shows what is actually running, without waiting for
    // the 250 ms tick.
    #[cfg(target_os = "android")]
    {
        let p = player.clone();
        app.on_settings_opened(move || p.refresh_decoder());
    }
    #[cfg(not(target_os = "android"))]
    {
        app.on_settings_opened(|| {});
    }

    let p = player.clone();
    app.on_toggle_fullscreen(move || p.toggle_fullscreen());

    // Hover previews: the timestamp under the cursor (seekbar) and the
    // volume percentage (volume slider). Formatting lives in Rust because
    // Slint has no int→string conversion for bindings.
    app.on_format_time(move |secs| SharedString::from(crate::player::format_time(secs as f64)));
    app.on_format_volume(move |v| SharedString::from(format!("{:.0}%", v.max(0.0))));

    let p = player.clone();
    app.on_tracks_popup_opened(move |_kind| p.refresh_tracks());

    // OSD auto-hide: show on activity, hide after inactivity — 3 s while
    // playing, 5 s while paused so the controls linger for resume. The
    // countdown runs in both states (loading keeps the bar up regardless).
    let osd_timer = Rc::new(slint::Timer::default());
    {
        let osd_timer = osd_timer.clone();
        let app_weak = app.as_weak();
        #[cfg(target_os = "android")]
        let osd_player = player.clone();
        app.on_osd_mouse_moved(move || {
            if let Some(app) = app_weak.upgrade() {
                app.set_osd_visible(true);
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
                    if let Some(app) = aw.upgrade() {
                        app.set_osd_visible(false);
                    }
                });
            }
        });
    }

    // 250 ms state mirror while the player overlay is up, plus episode
    // playback tracking (resume seeks + throttled progress saves + history
    // finalize on close). Both run on the UI thread.
    let _timer = slint::Timer::default();
    let tick_player = player.clone();
    let tick_bridge = bridge.clone();
    _timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            tick_player.tick();
            tick_bridge.note_player_progress_from_ui();
            tick_bridge.note_torrent_progress_from_ui();
            tick_bridge.refresh_download_rows();
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
        let autoplay_player = player.clone();
        let autoplay_timer = slint::Timer::default();
        autoplay_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(1500),
            move || {
                eprintln!("[autoplay] opening {url}");
                if let Err(e) = autoplay_player.play(&url, 0.0) {
                    eprintln!("[autoplay] error: {e}");
                }
            },
        );
        std::mem::forget(autoplay_timer);
    }

    crate::web_log("nova: startup complete, entering event loop");
    app.run()?;
    Ok(())
}

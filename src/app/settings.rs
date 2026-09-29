//! Settings page: cache + torrent settings, maintenance.
use super::*;

impl Bridge {
    /// Mirror the stored cache settings onto the Settings page controls.
    pub(super) fn settings_to_ui(&self) {
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        if let Some(app) = self.app() {
            app.set_cache_images(settings.cache_images);
            app.set_cache_enabled(settings.enabled);
            app.set_cache_format_index(match settings.format {
                CacheImageFormat::Jpeg => 0,
                CacheImageFormat::Webp => 1,
            });
            app.set_cache_quality(settings.quality as f32);
            app.set_cache_downscale(settings.downscale);
            app.set_cache_date_relative(settings.date_relative);
            app.set_cache_show_unwatched_thumbs(settings.show_unwatched_thumbs);
            app.set_cache_lru_mb(settings.lru_cache_mb as f32);
            app.set_cache_prefetch_metadata(settings.prefetch_metadata);
            app.set_cache_lazy_reencode(settings.lazy_reencode);
            app.set_discover_min_cols(settings.discover_min_cols as i32);
            app.set_library_min_cols(settings.library_min_cols as i32);
            app.set_android_hwdec_index(settings.android_hwdec.index());
            app.set_player_backend_index(if settings.player_external { 1 } else { 0 });
            app.set_desktop_external_app_index(settings.desktop_external_app.index());
            app.set_playback_speed(settings.playback_speed);
            app.set_episode_start_index(match settings.episode_start_behavior {
                EpisodeStartBehavior::StartOver => 0,
                EpisodeStartBehavior::Resume => 1,
                EpisodeStartBehavior::Ask => 2,
            });
            app.set_animations(settings.animations);
            app.set_anim_transitions(settings.anim_transitions);
            app.set_anim_hover(settings.anim_hover);
            app.set_anim_player(settings.anim_player);
            app.set_anim_nav_slide(settings.anim_nav_slide);
            app.set_language_index(settings.language.index());
            app.set_language_names(Rc::new(VecModel::<SharedString>::from(language_labels())).into());
        }
        self.download_settings_to_ui();
        self.apply_animations(&settings);
        self.apply_language(&settings);
        self.refresh_cache_disk_usage();
        self.apply_category_rows();
    }

    /// Push the animation switches into the `Anim` global, which every
    /// `animate` duration in the UI reads. Takes effect immediately.
    pub(super) fn apply_animations(&self, settings: &CacheSettings) {
        if let Some(app) = self.app() {
            let anim = app.global::<crate::Anim>();
            anim.set_enabled(settings.animations);
            anim.set_transitions(settings.anim_transitions);
            anim.set_hover(settings.anim_hover);
            anim.set_player(settings.anim_player);
            anim.set_nav_slide(settings.anim_nav_slide);
        }
    }

    /// Mirror the torrent settings (Settings → Torrents) into the UI.
    pub(super) fn torrent_settings_to_ui(&self) {
        let settings = active_torrent_settings();
        if let Some(app) = self.app() {
            app.set_torrent_enabled(settings.enabled);
            app.set_torrent_dir(SharedString::from(&settings.dir));
            app.set_torrent_max_mb(settings.max_mb as f32);
            app.set_torrent_down_limit(settings.down_limit_kbps as f32);
            app.set_torrent_no_cache(settings.no_cache);
        }
        self.refresh_torrent_disk_usage();
    }

    /// Refresh the Settings page torrent-cache readout. Native only.
    pub(super) fn refresh_torrent_disk_usage(&self) {
        if let Some(app) = self.app() {
            if let Some(engine) = crate::torrent::engine() {
                let bytes = engine.cache_bytes(&torrent_cache_dir());
                app.set_torrent_disk_usage(SharedString::from(&format_disk_usage(bytes, 0)));
            }
        }
    }

    /// Apply a change to the torrent settings: persist + update runtime cache
    /// + refresh the UI readouts. Torrent settings autosave, like the grid
    /// settings.
    pub(super) fn update_torrent_settings(&self, f: impl FnOnce(&mut TorrentSettings)) {
        let mut settings = active_torrent_settings();
        f(&mut settings);
        write_torrent_settings(&settings);
        sync_torrent_engine(&settings);
        self.torrent_settings_to_ui();
    }

    /// Refresh the Settings page disk-usage readout from the on-disk image
    /// cache. Native only (the row is hidden on web); a metadata-only walk,
    /// cheap enough to run on page open and after saving.
    pub(super) fn refresh_cache_disk_usage(&self) {
        if let Some(app) = self.app() {
            let (bytes, files) = poster_cache_disk_usage(&poster_cache_dir());
            app.set_cache_disk_usage(SharedString::from(&format_disk_usage(bytes, files)));
        }
    }

    pub(super) fn show_settings_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(true);
        }
        self.settings_to_ui();
        self.refresh_torrent_disk_usage();
        self.apply_addon_rows();
        self.downloads_list_to_ui();
        ensure_device_name();
        self.sync_status_to_ui();
    }

    /// Read the page controls, clamp, persist and remember the settings.
    /// Called automatically (debounced) after any Settings edit — there is
    /// no Save button.
    pub(super) fn save_settings(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let quality = (app.get_cache_quality().round() as i32).clamp(1, 100) as u8;
        let format = if app.get_cache_format_index() <= 0 {
            CacheImageFormat::Jpeg
        } else {
            CacheImageFormat::Webp
        };
        let settings = {
            let state = self.shared.lock().unwrap();
            CacheSettings {
                cache_images: app.get_cache_images(),
                enabled: app.get_cache_enabled(),
                format,
                quality,
                downscale: app.get_cache_downscale(),
                date_relative: app.get_cache_date_relative(),
                show_unwatched_thumbs: app.get_cache_show_unwatched_thumbs(),
                lru_cache_mb: (app.get_cache_lru_mb().round() as i32).clamp(32, 512) as u32,
                prefetch_metadata: app.get_cache_prefetch_metadata(),
                rewrite_existing: false,
                lazy_reencode: app.get_cache_lazy_reencode(),
                categories: state.cache_settings.categories.clone(),
                discover_min_cols: app.get_discover_min_cols().clamp(2, 6) as u32,
                library_min_cols: app.get_library_min_cols().clamp(2, 6) as u32,
                android_hwdec: AndroidHwdec::from_index(app.get_android_hwdec_index()),
                player_external: app.get_player_backend_index() == 1,
                desktop_external_app: DesktopExternalApp::from_index(
                    app.get_desktop_external_app_index(),
                ),
                playback_speed: nova_config::round_playback_speed(app.get_playback_speed()),
                episode_start_behavior: match app.get_episode_start_index() {
                    0 => EpisodeStartBehavior::StartOver,
                    2 => EpisodeStartBehavior::Ask,
                    _ => EpisodeStartBehavior::Resume,
                },
                animations: app.get_animations(),
                anim_transitions: app.get_anim_transitions(),
                anim_hover: app.get_anim_hover(),
                anim_player: app.get_anim_player(),
                anim_nav_slide: app.get_anim_nav_slide(),
                language: Language::from_index(app.get_language_index()),
            }
        };
        // Grids read these props live, so they update the moment settings
        // autosave (no need to reopen the page).
        app.set_discover_min_cols(settings.discover_min_cols as i32);
        app.set_library_min_cols(settings.library_min_cols as i32);
        {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings = settings.clone();
        }
        set_active_cache_settings(settings.clone());
        write_settings(&settings);
        self.apply_animations(&settings);
        self.apply_language(&settings);
        self.refresh_cache_disk_usage();

        // Torrent settings share this debounce (the Settings → Torrents card
        // restarts the same autosave timer), so a slider drag or path edit
        // persists once the user pauses instead of on every keystroke.
        let torrent = TorrentSettings {
            enabled: app.get_torrent_enabled(),
            dir: app.get_torrent_dir().to_string(),
            max_mb: app.get_torrent_max_mb().max(0.0).round() as u64,
            down_limit_kbps: app.get_torrent_down_limit().max(0.0).round() as u32,
            no_cache: app.get_torrent_no_cache(),
        };
        write_torrent_settings(&torrent);
        sync_torrent_engine(&torrent);
    }

    /// Immediate one-shot for the "Re-encode now" button: persist the
    /// current controls, then rewrite already-cached images to that config
    /// in the background (skips entries already matching it).
    /// Desktop-only.
    #[cfg(feature = "desktop")]
    pub(super) fn reencode_now(&self) {
        self.save_settings();
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        if !(settings.cache_images && settings.enabled) {
            return;
        }
        self.rewrite_existing_cache(settings);
    }

    #[cfg(not(feature = "desktop"))]
    pub(super) fn reencode_now(&self) {}

    /// Rewrite every already-cached image to `settings` on a worker thread.
    #[cfg(feature = "desktop")]
    pub(super) fn rewrite_existing_cache(&self, settings: CacheSettings) {
        let dir = poster_cache_dir();
        thread::spawn(move || {
            let _ = rewrite_cache_dir_to_format(&dir, &settings);
        });
    }

    /// Clear the on-disk image cache and drop decoded images from memory
    /// ("Clear" next to the disk-usage readout). The wipe runs on a worker
    /// thread; in-flight downloads may repopulate either cache after.
    /// Desktop only (the button is hidden on web, where the browser owns
    /// image caching; Android has its own variant below).
    #[cfg(all(feature = "desktop", not(target_os = "android")))]
    pub(super) fn clear_image_cache(&self) {
        let bridge = self.clone();
        thread::spawn(move || {
            let _ = clear_poster_cache_dir(&poster_cache_dir());
            decoded_cache_clear();
            let _ = slint::invoke_from_event_loop(move || {
                bridge.refresh_cache_disk_usage();
            });
        });
    }

    #[cfg(target_os = "android")]
    pub(super) fn clear_image_cache(&self) {
        // Same shape as desktop (raw originals only — no derivatives or
        // re-encode on Android).
        let bridge = self.clone();
        thread::spawn(move || {
            let _ = clear_poster_cache_dir(&poster_cache_dir());
            decoded_cache_clear();
            let _ = slint::invoke_from_event_loop(move || {
                bridge.refresh_cache_disk_usage();
            });
        });
    }

    #[cfg(all(not(feature = "desktop"), not(target_os = "android")))]
    pub(super) fn clear_image_cache(&self) {}

    pub(super) fn modal_closed(&self) {
        // Snapshot position first (backing out + reopening restores it).
        self.save_detail_snapshot();
        {
            let mut state = self.shared.lock().unwrap();
            state.modal_item = None;
            state.streams.clear();
            state.stream_all.clear();
            state.stream_addons.clear();
            state.stream_pending.clear();
            state.stream_filter = 0;
        }
        if let Some(app) = self.app() {
            app.set_modal_episodes(false);
            app.set_detail_deep_stream(false);
            app.set_episode_context(SharedString::default());
            app.set_season_names(Rc::new(VecModel::<SharedString>::from(vec![])).into());
            app.set_season_combo_idx(-1);
            app.set_season_cards(Rc::new(VecModel::<SeasonCard>::from(vec![])).into());
            app.set_episode_rows(Rc::new(VecModel::<EpisodeRow>::from(vec![])).into());
            app.set_categories_modal(false);
            app.set_selected_category_count(0);
            app.set_detail_tab(0);
            app.set_episode_filter(SharedString::default());
            app.set_selected_backdrop(Image::default());
            app.set_selected_description(SharedString::default());
            app.set_selected_genre_list(Rc::new(VecModel::from(vec![])).into());
        }
    }

}

/// Delete every file in the on-disk image cache `dir` (leaving the dir
/// itself), returning the freed `(bytes, file_count)`. Missing dir reads
/// as nothing to free. Native only (web has no app-level image cache).
pub(crate) fn clear_poster_cache_dir(dir: &Path) -> (u64, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let len = meta.len();
        if fs::remove_file(entry.path()).is_ok() {
            bytes = bytes.saturating_add(len);
            files += 1;
        }
    }
    (bytes, files)
}
/// Total size of the on-disk image cache: `(bytes, file_count)` over every
/// regular file in `dir` (originals `*.img`, display derivatives
/// `*.d<side>.jpg`, sidecars — the dir is flat and cache-owned).
/// Missing/unreadable dir reads as empty. Native only (web has no
/// app-level image cache).
pub(crate) fn poster_cache_disk_usage(dir: &Path) -> (u64, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
            files += 1;
        }
    }
    (bytes, files)
}
#[cfg(feature = "desktop")]
pub(crate) fn read_settings_file(dir: &Path) -> CacheSettings {
    let Ok(text) = fs::read_to_string(dir.join("settings.toml")) else {
        return CacheSettings::default();
    };
    toml::from_str::<SettingsFile>(&text)
        .map(|f| f.cache)
        .unwrap_or_default()
}
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn write_settings_file(dir: &Path, settings: &CacheSettings) {
    if fs::create_dir_all(dir).is_err() {
        eprintln!("nova: could not create config dir {:?}", dir);
        return;
    }
    let body = toml::to_string_pretty(&SettingsFile {
        cache: settings.clone(),
        torrent: read_torrent_settings_file(dir),
    })
    .unwrap_or_default();
    let header = "# nova — settings.\n\n";
    let contents = format!("{header}{body}");
    if let Err(e) = atomic_write(&dir.join("settings.toml"), &contents) {
        eprintln!("nova: could not write settings.toml: {e}");
    }
}
/// Read the torrent section from `settings.toml` (defaults when absent or
/// unparseable). Native only.
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn read_torrent_settings_file(dir: &Path) -> TorrentSettings {
    let Ok(text) = fs::read_to_string(dir.join("settings.toml")) else {
        return TorrentSettings::default();
    };
    toml::from_str::<SettingsFile>(&text)
        .map(|f| f.torrent)
        .unwrap_or_default()
}
/// Persist the torrent section, preserving the cached image settings.
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn write_torrent_settings_file(dir: &Path, torrent: &TorrentSettings) {
    if fs::create_dir_all(dir).is_err() {
        eprintln!("nova: could not create config dir {:?}", dir);
        return;
    }
    let body = toml::to_string_pretty(&SettingsFile {
        cache: read_settings_file(dir),
        torrent: torrent.clone(),
    })
    .unwrap_or_default();
    let header = "# nova — settings.\n\n";
    let contents = format!("{header}{body}");
    if let Err(e) = atomic_write(&dir.join("settings.toml"), &contents) {
        eprintln!("nova: could not write settings.toml: {e}");
    }
}
/// App-wide cache settings (also mirrored in the settings page).
pub(crate) fn read_settings() -> CacheSettings {
    read_json::<CacheSettings>("settings").unwrap_or_default()
}
pub(crate) fn write_settings(settings: &CacheSettings) {
    write_json("settings", settings);
    if !applying() {
        notify_settings(settings);
    }
}

//! Settings page: cache + torrent settings, maintenance.
use super::*;
static CACHE_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn cancel_cache_rewrite() {
    CACHE_CANCEL.store(true, std::sync::atomic::Ordering::Release);
}

static UNSUPPORTED_SETTINGS: Mutex<Option<HashMap<String, serde_json::Value>>> = Mutex::new(None);

pub(crate) fn protect_setting_field(field: &str, raw: serde_json::Value) {
    UNSUPPORTED_SETTINGS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(field.to_string(), raw);
}

pub(crate) fn allow_setting_field(field: &str) {
    if let Some(fields) = UNSUPPORTED_SETTINGS.lock().unwrap().as_mut() {
        if field == "cache" {
            for key in [
                "cache_images",
                "enabled",
                "format",
                "quality",
                "downscale",
                "lazy_reencode",
                "lru_cache_mb",
            ] {
                fields.remove(key);
            }
        } else {
            fields.remove(field);
        }
    }
}

pub(crate) fn preserve_unsupported_settings(_old: Option<&str>, new: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(new) else {
        return new.to_string();
    };
    if let (Some(map), Some(fields)) = (
        value.as_object_mut(),
        UNSUPPORTED_SETTINGS.lock().unwrap().as_ref(),
    ) {
        for (field, raw) in fields {
            map.insert(field.clone(), raw.clone());
        }
    }
    value.to_string()
}

pub(crate) fn recover_settings(value: serde_json::Value) -> Option<CacheSettings> {
    let source = value.as_object()?;
    // Losing unreadable policy could publish device overrides as account values.
    if source.get("sync_overrides").is_some_and(|v| !v.is_object()) {
        return None;
    }
    let mut current = serde_json::to_value(CacheSettings::default()).ok()?;
    for (key, value) in source {
        let mut trial = current.clone();
        trial.as_object_mut()?.insert(key.clone(), value.clone());
        if serde_json::from_value::<CacheSettings>(trial.clone()).is_ok() {
            current = trial;
        } else {
            protect_setting_field(key, value.clone());
            storage::report(storage::Error::new(
                storage::ErrorKind::Schema,
                format!("unsupported settings field {key}; raw value retained"),
            ));
        }
    }
    serde_json::from_value(current).ok()
}

impl Bridge {
    /// Wire the Settings page to an application-owned timer. Return the same
    /// scheduler for playback-rate edits, which share the settings snapshot.
    pub(super) fn wire_settings_autosave(&self) -> impl Fn() + Clone + 'static {
        let timer = Rc::new(slint::Timer::default());
        let b = self.clone();
        let schedule = move || {
            let b = b.clone();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(600),
                move || b.persist_settings(),
            );
        };
        if let Some(app) = self.app() {
            app.on_settings_search_matches(|query, haystack| {
                nova_ui::settings_search_matches(&query, &haystack)
            });
            let b = self.clone();
            let callback_schedule = schedule.clone();
            app.on_save_settings(move || {
                b.capture_settings();
                callback_schedule();
            });
            let b = self.clone();
            let callback_schedule = schedule.clone();
            app.on_settings_edited(move |field| {
                allow_setting_field(&field);
                b.capture_settings();
                let settings = b.shared.lock().unwrap().cache_settings.clone();
                notify_setting_choice(&field, &settings);
                callback_schedule();
            });
        }
        self.bind_setting_sync();
        schedule
    }

    /// Mirror the stored cache settings onto the Settings page controls.
    pub(super) fn settings_to_ui(&self) {
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        if let Some(app) = self.app() {
            app.set_persistence_failed(storage::last_error().is_some());
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
            app.set_discover_catalog_addon_names(settings.discover_catalog_addon_names);
            app.set_home_continue_enabled(
                settings.home_row_enabled(&HomeRowSource::ContinueWatching),
            );
            app.set_home_upcoming_enabled(settings.home_row_enabled(&HomeRowSource::Upcoming));
            app.set_home_episode_artwork(settings.home_episode_artwork);
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
            app.set_true_black(settings.true_black);
            app.set_animations(settings.animations);
            app.set_anim_transitions(settings.anim_transitions);
            app.set_anim_hover(settings.anim_hover);
            app.set_anim_player(settings.anim_player);
            app.set_anim_nav_slide(settings.anim_nav_slide);
            app.set_language_index(settings.language.index());
            app.set_language_names(
                Rc::new(VecModel::<SharedString>::from(language_labels())).into(),
            );
        }
        self.download_settings_to_ui();
        self.apply_theme(&settings);
        self.apply_animations(&settings);
        self.apply_language(&settings);
        self.apply_catalog_labels_to_ui();
        self.apply_home_catalog_rows();
        self.refresh_setting_sync();
        self.refresh_cache_disk_usage();
        self.apply_category_rows();
    }

    /// Apply the effective palette to all mounted pages.
    pub(super) fn apply_theme(&self, settings: &CacheSettings) {
        if let Some(app) = self.app() {
            nova_ui::apply_theme(&app, settings.true_black);
        }
    }

    /// Push animation switches into the shared global for immediate feedback.
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
        if let Some(app) = self.app()
            && let Some(engine) = crate::torrent::engine()
        {
            let bytes = engine.cache_bytes(&torrent_cache_dir());
            app.set_torrent_disk_usage(SharedString::from(&format_disk_usage(bytes, 0)));
        }
    }

    /// Apply a change to the torrent settings: persist + update runtime cache
    /// + refresh the UI readouts. Torrent settings autosave, like the grid
    ///   settings.
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

    /// Capture controls into authoritative memory before navigation or a
    /// remote refresh can replace them. Only the disk snapshot is debounced.
    pub(super) fn capture_settings(&self) {
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
                home_catalog_sources: state.cache_settings.home_catalog_sources.clone(),
                home_row_sources: state.cache_settings.home_row_sources.clone(),
                home_rows: state.cache_settings.home_rows.clone(),
                home_continue_enabled: app.get_home_continue_enabled(),
                home_upcoming_enabled: app.get_home_upcoming_enabled(),
                home_episode_artwork: app.get_home_episode_artwork(),
                discover_min_cols: app.get_discover_min_cols().clamp(2, 6) as u32,
                discover_catalog_addon_names: app.get_discover_catalog_addon_names(),
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
                sync_overrides: state.cache_settings.sync_overrides.clone(),
                true_black: app.get_true_black(),
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
        if !applying() {
            notify_settings(&settings);
        }
        self.apply_theme(&settings);
        self.apply_animations(&settings);
        self.apply_language(&settings);
        self.apply_catalog_labels_to_ui();
        self.refresh_setting_sync();

        // Torrent controls share the same immediate capture and disk debounce.
        let torrent = TorrentSettings {
            enabled: app.get_torrent_enabled(),
            dir: app.get_torrent_dir().to_string(),
            max_mb: app.get_torrent_max_mb().max(0.0).round() as u64,
            down_limit_kbps: app.get_torrent_down_limit().max(0.0).round() as u32,
            no_cache: app.get_torrent_no_cache(),
        };
        *CURRENT_TORRENT_SETTINGS.lock().unwrap() = torrent.clone();
        sync_torrent_engine(&torrent);
    }

    /// Build the configured Home catalog/genre rows, retaining stale entries
    /// so removed catalogs can still be removed from settings.
    fn home_catalog_choices(&self, home_row: bool) -> Vec<(HomeCatalogSource, HomeCatalogRow)> {
        let state = self.shared.lock().unwrap();
        let sources = if home_row {
            state.cache_settings.home_addon_sources(false)
        } else {
            state.cache_settings.home_catalog_sources.clone()
        };
        sources
            .iter()
            .map(|source| {
                let addon = state
                    .installed
                    .iter()
                    .find(|addon| addon.url == source.addon_url);
                let catalog = addon.and_then(|addon| {
                    addon
                        .manifest
                        .catalog_for(&source.type_, &source.catalog_id)
                });
                let available = addon.is_some_and(|addon| addon.available) && catalog.is_some();
                let enabled = available && addon.is_some_and(|addon| addon.enabled);
                (
                    source.clone(),
                    HomeCatalogRow {
                        title: SharedString::from(
                            catalog
                                .map(|catalog| catalog.name.as_str())
                                .unwrap_or(&source.catalog_id),
                        ),
                        addon: SharedString::from(
                            addon
                                .map(|addon| addon.label.as_str())
                                .unwrap_or(&source.addon_url),
                        ),
                        media_type: SharedString::from(&source.type_),
                        genre: SharedString::from(&source.genre),
                        available,
                        enabled,
                        builtin: false,
                        visible: true,
                    },
                )
            })
            .collect()
    }

    /// Addable choices come only from addons that can currently serve Home.
    /// Each candidate carries the genre options declared by its manifest.
    fn home_catalog_candidates(&self) -> Vec<(HomeCatalogSource, String, Vec<String>)> {
        let state = self.shared.lock().unwrap();
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for addon in state
            .installed
            .iter()
            .filter(|addon| addon.available && addon.enabled)
        {
            for catalog in &addon.manifest.catalogs {
                let source = HomeCatalogSource {
                    addon_url: addon.url.clone(),
                    type_: catalog.type_.clone(),
                    catalog_id: catalog.id.clone(),
                    genre: String::new(),
                };
                if seen.insert(source.clone()) {
                    candidates.push((
                        source,
                        format!("{} — {} · {}", addon.label, catalog.name, catalog.type_),
                        super::catalog::catalog_genres(catalog),
                    ));
                }
            }
        }
        candidates
    }

    fn home_picker_candidates(&self, home_row: bool) -> Vec<(HomeRowSource, String, Vec<String>)> {
        let mut candidates = Vec::new();
        if home_row {
            let configured = self
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .effective_home_rows();
            for (source, label) in [
                (HomeRowSource::ContinueWatching, "Continue Watching"),
                (HomeRowSource::Upcoming, "Upcoming"),
            ] {
                if !configured.iter().any(|row| row.source == source) {
                    candidates.push((
                        source,
                        format!("{} — {}", text::tr("Built-in"), text::tr(label)),
                        Vec::new(),
                    ));
                }
            }
        }
        candidates.extend(
            self.home_catalog_candidates()
                .into_iter()
                .map(|(source, name, genres)| (HomeRowSource::Addon(source), name, genres)),
        );
        candidates
    }

    pub(super) fn apply_home_catalog_rows(&self) {
        if let Some(app) = self.app() {
            let featured = self
                .home_catalog_choices(false)
                .into_iter()
                .map(|(_, row)| row)
                .collect::<Vec<_>>();
            let choices = self.home_catalog_choices(true);
            let configured = self
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .effective_home_rows();
            let home_rows = configured
                .into_iter()
                .map(|entry| match entry.source {
                    HomeRowSource::ContinueWatching | HomeRowSource::Upcoming => HomeCatalogRow {
                        title: text::tr(if entry.source == HomeRowSource::ContinueWatching {
                            "Continue Watching"
                        } else {
                            "Upcoming"
                        })
                        .into(),
                        addon: text::tr("Built-in").into(),
                        available: true,
                        enabled: true,
                        builtin: true,
                        visible: entry.enabled,
                        ..Default::default()
                    },
                    HomeRowSource::Addon(source) => {
                        let mut row = choices
                            .iter()
                            .find(|(choice, _)| choice == &source)
                            .map(|(_, row)| row.clone())
                            .unwrap_or_default();
                        row.visible = entry.enabled;
                        row
                    }
                })
                .collect::<Vec<_>>();
            app.set_home_catalog_rows(Rc::new(VecModel::from(featured)).into());
            app.set_home_row_catalog_rows(Rc::new(VecModel::from(home_rows)).into());
        }
    }

    pub(super) fn home_catalog_add_requested(&self) {
        self.home_catalog_picker_requested(0);
    }

    pub(super) fn home_row_catalog_add_requested(&self) {
        self.home_catalog_picker_requested(1);
    }

    fn home_catalog_picker_requested(&self, target: i32) {
        let candidates = self.home_picker_candidates(target == 1);
        if let Some(app) = self.app() {
            app.set_home_catalog_add_target(target);
            let names = candidates
                .iter()
                .map(|(_, name, _)| SharedString::from(name.as_str()))
                .collect::<Vec<_>>();
            let genres = candidates
                .first()
                .map(|(_, _, genres)| genres.as_slice())
                .unwrap_or_default();
            app.set_home_catalog_candidate_names(Rc::new(VecModel::from(names)).into());
            app.set_home_catalog_candidate_index(if candidates.is_empty() { -1 } else { 0 });
            app.set_home_catalog_genre_names(
                Rc::new(VecModel::from(
                    std::iter::once(SharedString::from(text::tr("All genres")))
                        .chain(
                            genres
                                .iter()
                                .map(|genre| SharedString::from(genre.as_str())),
                        )
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            app.set_home_catalog_genre_index(0);
            app.set_home_catalog_add_error(SharedString::default());
            app.set_home_catalog_add_open(true);
        }
    }

    pub(super) fn home_catalog_candidate_picked(&self, index: i32) {
        let home_row = self
            .app()
            .is_some_and(|app| app.get_home_catalog_add_target() == 1);
        let candidates = self.home_picker_candidates(home_row);
        let Some((_, _, genres)) = usize::try_from(index)
            .ok()
            .and_then(|index| candidates.get(index))
        else {
            return;
        };
        if let Some(app) = self.app() {
            app.set_home_catalog_candidate_index(index);
            app.set_home_catalog_genre_names(
                Rc::new(VecModel::from(
                    std::iter::once(SharedString::from(text::tr("All genres")))
                        .chain(
                            genres
                                .iter()
                                .map(|genre| SharedString::from(genre.as_str())),
                        )
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            app.set_home_catalog_genre_index(0);
            app.set_home_catalog_add_error(SharedString::default());
        }
    }

    /// Persist a catalog and optional declared genre as one synced setting.
    /// Legacy sources deserialize with an empty genre (all genres).
    pub(super) fn home_catalog_added(&self, candidate_index: i32, genre_index: i32) {
        let home_row = self
            .app()
            .is_some_and(|app| app.get_home_catalog_add_target() == 1);
        let candidates = self.home_picker_candidates(home_row);
        let Some((mut kind, _, genres)) = usize::try_from(candidate_index)
            .ok()
            .and_then(|index| candidates.get(index).cloned())
        else {
            return;
        };
        if genre_index > 0 {
            let Some(genre) = usize::try_from(genre_index - 1)
                .ok()
                .and_then(|index| genres.get(index))
            else {
                return;
            };
            let HomeRowSource::Addon(source) = &mut kind else {
                return;
            };
            source.genre = genre.clone();
        } else if genre_index < 0 {
            return;
        }

        if home_row {
            let mut rows = self
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .effective_home_rows();
            if rows.iter().any(|row| row.source == kind) {
                if let Some(app) = self.app() {
                    app.set_home_catalog_add_error(
                        text::tr("This catalog and genre are already added.").into(),
                    );
                }
                return;
            }
            rows.push(HomeRow {
                source: kind,
                enabled: true,
            });
            self.save_home_rows(rows);
            if let Some(app) = self.app() {
                app.set_home_catalog_add_open(false);
            }
            return;
        }
        let HomeRowSource::Addon(source) = kind else {
            return;
        };
        let (settings, duplicate) = {
            let mut state = self.shared.lock().unwrap();
            let selected = &mut state.cache_settings.home_catalog_sources;
            let duplicate = selected.contains(&source);
            if !duplicate {
                selected.push(source);
            }
            (state.cache_settings.clone(), duplicate)
        };
        if duplicate {
            if let Some(app) = self.app() {
                app.set_home_catalog_add_error(SharedString::from(text::tr(
                    "This catalog and genre are already added.",
                )));
            }
            return;
        }
        set_active_cache_settings(settings.clone());
        write_settings(&settings);
        self.apply_home_catalog_rows();
        self.invalidate_home_showcase();
        self.refresh_home_showcase();
        if let Some(app) = self.app() {
            app.set_home_catalog_add_open(false);
            app.set_persistence_failed(storage::last_error().is_some());
        }
    }

    pub(super) fn home_catalog_removed(&self, index: usize) {
        let choices = self.home_catalog_choices(false);
        let Some((source, _)) = choices.get(index) else {
            return;
        };
        let source = source.clone();
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state
                .cache_settings
                .home_catalog_sources
                .retain(|saved| saved != &source);
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings.clone());
        write_settings(&settings);
        self.apply_home_catalog_rows();
        self.invalidate_home_showcase();
        if let Some(app) = self.app() {
            app.set_persistence_failed(storage::last_error().is_some());
        }
    }

    fn save_home_rows(&self, rows: Vec<HomeRow>) {
        allow_setting_field("home_rows");
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.set_home_rows(rows);
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings.clone());
        write_settings(&settings);
        if !applying() {
            notify_setting_choice("home_rows", &settings);
        }
        self.settings_to_ui();
        self.invalidate_home_catalog_rows();
        self.apply_home_to_ui();
    }

    pub(super) fn home_row_catalog_removed(&self, index: usize) {
        self.home_row_catalog_action(index, 3);
    }

    /// One action vocabulary for built-in and addon rows: visibility, up,
    /// down, remove. Bounds checks also cover stale callbacks after a sync.
    pub(super) fn home_row_catalog_action(&self, index: usize, action: i32) {
        let mut rows = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .effective_home_rows();
        if index >= rows.len() {
            return;
        }
        match action {
            0 => rows[index].enabled = !rows[index].enabled,
            1 if index > 0 => rows.swap(index, index - 1),
            2 if index + 1 < rows.len() => rows.swap(index, index + 1),
            3 => {
                rows.remove(index);
            }
            _ => return,
        }
        self.save_home_rows(rows);
    }

    /// Persist current memory, never stale controls captured by a timer.
    pub(super) fn persist_settings(&self) {
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        write_settings(&settings);
        write_torrent_settings(&active_torrent_settings());
    }

    pub(super) fn save_settings(&self) {
        self.capture_settings();
        self.persist_settings();
    }

    /// Snapshot after immediate capture; job ownership outlives the Settings page.
    pub(super) fn reencode_now(&self) {
        let Some(guard) = MaintenanceGuard::try_acquire() else {
            return;
        };
        self.save_settings();
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        if !settings.cache_images || !settings.enabled {
            return;
        }
        CACHE_CANCEL.store(false, std::sync::atomic::Ordering::Release);
        if let Some(app) = self.app() {
            app.set_cache_busy(true);
            app.set_cache_rewriting(true);
            app.set_cache_processed(0);
            app.set_cache_total(0);
            app.set_cache_status(text::cache_rewrite_status(0, 0, 0, 0, 0, false, false).into());
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let mut last_update = std::time::Instant::now() - Duration::from_secs(1);
            let result = guard.rewrite(&poster_cache_dir(), &settings, &CACHE_CANCEL, |p| {
                if p.processed != p.total && last_update.elapsed() < Duration::from_millis(100) {
                    return;
                }
                last_update = std::time::Instant::now();
                let bridge = bridge.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = bridge.app() {
                        app.set_cache_processed(p.processed as i32);
                        app.set_cache_total(p.total as i32);
                        app.set_cache_status(
                            text::cache_rewrite_status(
                                p.processed,
                                p.total,
                                p.converted,
                                p.skipped,
                                p.failed,
                                p.cancelled,
                                false,
                            )
                            .into(),
                        );
                    }
                });
            });
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = bridge.app() {
                    app.set_cache_busy(false);
                    app.set_cache_rewriting(false);
                    app.set_cache_processed(result.processed as i32);
                    app.set_cache_total(result.total as i32);
                    app.set_cache_status(
                        text::cache_rewrite_status(
                            result.processed,
                            result.total,
                            result.converted,
                            result.skipped,
                            result.failed,
                            result.cancelled,
                            true,
                        )
                        .into(),
                    );
                }
                bridge.refresh_cache_disk_usage();
                // Keep the job gate until its final UI state is applied, so
                // a newly started job cannot be overwritten by this callback.
                drop(guard);
            });
        });
    }

    pub(super) fn clear_image_cache(&self) {
        let Some(guard) = MaintenanceGuard::try_acquire() else {
            return;
        };
        if let Some(app) = self.app() {
            app.set_cache_busy(true);
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let _ = guard.clear(&poster_cache_dir());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = bridge.app() {
                    app.set_cache_busy(false);
                    app.set_cache_status(SharedString::default());
                }
                bridge.refresh_cache_disk_usage();
                // Keep the job gate until its final UI state is applied, so
                // a newly started job cannot be overwritten by this callback.
                drop(guard);
            });
        });
    }

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
            app.set_library_duplicates_open(false);
            app.set_library_duplicates(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            app.set_library_duplicate_selection(-1);
            app.set_selected_category_count(0);
            app.set_detail_tab(0);
            app.set_episode_filter(SharedString::default());
            app.set_selected_backdrop(Image::default());
            app.set_selected_description(SharedString::default());
            app.set_selected_genre_list(Rc::new(VecModel::from(vec![])).into());
        }
    }
}

/// App-wide cache settings (also mirrored in the settings page).
pub(crate) fn read_settings() -> CacheSettings {
    match read_json_result::<serde_json::Value>("settings") {
        Ok(None) => CacheSettings::default(),
        Ok(Some(value)) => recover_settings(value).unwrap_or_else(|| {
            block_unreadable("settings");
            storage::report(storage::Error::new(
                storage::ErrorKind::Schema,
                "unreadable settings object retained",
            ));
            CacheSettings::default()
        }),
        Err(e) => {
            block_unreadable("settings");
            storage::report(e);
            CacheSettings::default()
        }
    }
}
pub(crate) fn write_settings(settings: &CacheSettings) {
    write_json("settings", settings);
    if !applying() {
        notify_settings(settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_override_policy_does_not_fall_back_to_shared_publication() {
        assert!(recover_settings(serde_json::json!({"sync_overrides": []})).is_none());
        assert!(recover_settings(serde_json::json!({"sync_overrides": "invalid"})).is_none());
    }

    #[test]
    fn autosave_survives_navigation_and_preserves_current_memory() {
        // Storage and Slint initialize once per process. Isolate this test
        // from unit tests that intentionally run with an uninitialized KV
        // backend, and close redb before removing its files (also on Windows).
        const TEST_DIR: &str = "NOVA_SETTINGS_AUTOSAVE_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-settings-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args([
                    "--exact",
                    "app::settings::tests::autosave_survives_navigation_and_preserves_current_memory",
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
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        app.window().show().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(Mutex::new(PosterStore::new(1))),
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
        let schedule = bridge.wire_settings_autosave();
        bridge.sync_seed();
        let owner = nova_sync::local_store().unwrap();
        assert!(
            owner.lock().unwrap().records(DOMAIN_SETTINGS).is_empty(),
            "untouched defaults must not be published on first sync"
        );
        {
            let mut store = owner.lock().unwrap();
            let version = nova_sync::Version::new(nova_config::now_ms(), 0, 999, false);
            store.apply(
                DOMAIN_SETTINGS,
                "discover_min_cols",
                nova_sync::Record {
                    value: Some("5".into()),
                    version,
                },
            );
            store.save().unwrap();
        }
        bridge.sync_apply(vec![DOMAIN_SETTINGS.into()]);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .discover_min_cols,
            5
        );
        app.invoke_settings_edited("discover_catalog_addon_names".into());
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "discover_catalog_addon_names")
                .unwrap()
                .value
                .as_deref(),
            Some("true"),
            "explicitly choosing the default still records intent"
        );
        bridge.settings_to_ui();
        bridge.torrent_settings_to_ui();
        bridge.persist_settings();
        app.set_true_black(true);
        app.invoke_settings_edited("true_black".into());
        assert!(app.global::<crate::Theme>().get_true_black());
        assert_eq!(
            app.global::<crate::Theme>().get_current().artwork_canvas,
            slint::Color::from_rgb_u8(0, 0, 0)
        );
        bridge.persist_settings();
        assert!(read_settings().true_black);
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "true_black")
                .unwrap()
                .value
                .as_deref(),
            Some("true"),
            "theme edits publish a synced preference"
        );
        app.set_true_black(false);
        bridge.settings_to_ui();
        assert!(app.get_true_black(), "stored theme is restored on reload");
        // A local override leaves the shared record and its clock unchanged,
        // including when another member of the coupled cache group is edited.
        app.set_cache_quality(80.0);
        app.invoke_settings_edited("quality".into());
        let shared_before = owner
            .lock()
            .unwrap()
            .record(DOMAIN_SETTINGS, "cache")
            .unwrap()
            .clone();
        bridge.set_setting_override("quality", true);
        app.set_cache_quality(95.0);
        app.invoke_settings_edited("quality".into());
        bridge.persist_settings();
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "cache")
                .unwrap(),
            &shared_before
        );
        assert_eq!(read_settings().quality, 95);
        assert!(read_settings().sync_overrides.contains_key("quality"));
        {
            let mut store = owner.lock().unwrap();
            store.apply(
                DOMAIN_SETTINGS,
                "cache",
                nova_sync::Record {
                    value: Some(r#"{"quality":60,"format":"webp","downscale":true}"#.into()),
                    version: nova_sync::Version::new(nova_config::now_ms() + 1000, 0, 999, false),
                },
            );
            store.save().unwrap();
        }
        bridge.sync_apply(vec![DOMAIN_SETTINGS.into()]);
        assert_eq!(app.get_cache_quality(), 95.0);
        assert_eq!(read_settings().sync_overrides["quality"], 60);
        app.set_cache_downscale(false);
        app.invoke_settings_edited("downscale".into());
        let raw = owner
            .lock()
            .unwrap()
            .record(DOMAIN_SETTINGS, "cache")
            .unwrap()
            .value
            .clone()
            .unwrap();
        let shared: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(shared["quality"], 60);
        assert_eq!(shared["downscale"], false);
        bridge.set_setting_override("quality", false);
        assert_eq!(app.get_cache_quality(), 60.0);
        assert!(!read_settings().sync_overrides.contains_key("quality"));
        let initial = read_settings();
        app.set_show_home(false);
        app.set_show_settings(true);
        // Exercise a real page control, not just the exported callback: the
        // page must report the edit synchronously, before any debounce fires.
        app.global::<crate::Anim>().set_enabled(false);
        let tap = |element: i_slint_backend_testing::ElementHandle| {
            let p = element.absolute_position();
            let size = element.size();
            let position =
                slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
            for event in [
                slint::platform::WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
                slint::platform::WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
            ] {
                app.window().dispatch_event(event);
            }
        };
        tap(i_slint_backend_testing::ElementQuery::from_root(&app)
            .match_predicate(|e| e.accessible_id().as_deref() == Some("settings:display"))
            .find_first()
            .unwrap());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        tap(
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&app, "ToggleSwitch")
                .next()
                .unwrap(),
        );
        assert_eq!(
            bridge.shared.lock().unwrap().cache_settings.date_relative,
            !initial.date_relative
        );
        app.set_discover_min_cols(6);
        app.set_torrent_dir("pending-torrent-dir".into());
        app.invoke_save_settings();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .discover_min_cols,
            6,
        );
        assert_eq!(active_torrent_settings().dir, "pending-torrent-dir");
        assert_eq!(read_settings().discover_min_cols, initial.discover_min_cols);

        // Destroy the page before its old 600 ms timer would have fired.
        app.set_show_settings(false);
        app.set_show_home(true);
        bridge.settings_to_ui();
        bridge.torrent_settings_to_ui();
        assert_eq!(app.get_discover_min_cols(), 6);
        assert_eq!(app.get_torrent_dir(), "pending-torrent-dir");

        // Simulate the remote projection path updating an unrelated field.
        // The timer must persist the latest backend state, not its old snapshot
        // or controls that existed before the UI refresh.
        bridge.shared.lock().unwrap().cache_settings.date_relative = false;
        bridge.settings_to_ui();
        bridge.apply_playback_speed(1.25);
        schedule();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(601));
        let saved = read_settings();
        assert_eq!(saved.discover_min_cols, 6);
        assert!(!saved.date_relative);
        assert_eq!(saved.playback_speed, 1.25);
        assert_eq!(read_torrent_settings().dir, "pending-torrent-dir");
        // A queued save must not replay stale values over a newer projection.
        app.set_show_settings(true);
        app.set_show_home(false);
        app.set_discover_min_cols(5);
        app.invoke_save_settings();
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .discover_min_cols = 4;
        bridge.settings_to_ui();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(601));
        assert_eq!(read_settings().discover_min_cols, 4);

        // Exercise the actual app persistence and reconciliation paths with a
        // stale mirror while headless changes are waiting for their callback.
        write_continue_hidden(&HashMap::from([("old".into(), 1)]));
        {
            let mut store = owner.lock().unwrap();
            let version = nova_sync::Version::new(nova_config::now_ms() + 1000, 0, 999, true);
            assert!(store.apply(
                DOMAIN_CONTINUE_HIDDEN,
                "old",
                nova_sync::Record {
                    value: None,
                    version
                }
            ));
            assert!(store.apply(
                DOMAIN_CONTINUE_HIDDEN,
                "remote",
                nova_sync::Record {
                    value: Some("3".into()),
                    version: nova_sync::Version {
                        deleted: false,
                        ..version
                    }
                }
            ));
            store.save().unwrap();
        }
        write_continue_hidden(&HashMap::from([("old".into(), 1), ("local".into(), 2)]));
        assert!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_CONTINUE_HIDDEN, "old")
                .unwrap()
                .is_deleted(),
            "unchanged stale mirror must not resurrect a remote tombstone"
        );
        // Hide pruning is meaningful only for items with recorded activity.
        for id in ["remote", "local"] {
            bridge.shared.lock().unwrap().progress.insert(
                id.into(),
                EpisodeProgress {
                    series_id: id.into(),
                    updated_at_secs: 1,
                    ..EpisodeProgress::default()
                },
            );
        }
        bridge.sync_apply(vec![DOMAIN_CONTINUE_HIDDEN.into()]);
        let hidden = read_continue_hidden();
        assert!(!hidden.contains_key("old"));
        assert_eq!(hidden.get("remote"), Some(&3));
        assert_eq!(hidden.get("local"), Some(&2));
        bridge.assert_desired_addon_regression();

        // Unsupported enum values affect only their field, and remain raw on
        // disk when an unrelated supported setting is changed.
        let mut raw = serde_json::to_value(read_settings()).unwrap();
        raw["language"] = "future-language".into();
        raw["date_relative"] = false.into();
        storage::try_set_str("settings", &raw.to_string()).unwrap();
        let mut recovered = read_settings();
        assert!(!recovered.date_relative);
        recovered.date_relative = true;
        write_settings(&recovered);
        let saved: serde_json::Value =
            serde_json::from_str(&storage::try_get_str("settings").unwrap().unwrap()).unwrap();
        assert_eq!(saved["language"], "future-language");
        assert_eq!(saved["date_relative"], true);
        {
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN_SETTINGS,
                "cache",
                Some(r#"{"format":"future-format","cache_images":true}"#.into()),
                0,
                999,
            );
            store.save().unwrap();
        }
        bridge.sync_apply(vec![DOMAIN_SETTINGS.into()]);
        app.set_cache_quality(88.0);
        app.invoke_settings_edited("quality".into());
        bridge.persist_settings();
        let cache: serde_json::Value = serde_json::from_str(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "cache")
                .unwrap()
                .value
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            cache["format"], "future-format",
            "editing a different field in the group must retain unsupported enum intent"
        );
        assert_eq!(cache["quality"], 88);
        app.invoke_settings_edited("format".into());
        let cache: serde_json::Value = serde_json::from_str(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "cache")
                .unwrap()
                .value
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        assert_ne!(
            cache["format"], "future-format",
            "only an explicit format choice replaces the unsupported value"
        );
        drop(bridge);
        drop(app);
    }
}

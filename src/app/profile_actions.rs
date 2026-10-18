//! Profile navigation swaps personal state while account sync stays connected.
use super::*;

pub(super) struct WorkerDone(pub Arc<AtomicBool>);
impl Drop for WorkerDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Bridge {
    fn profile_error(&self, error: String) {
        tracing::warn!(%error, "profile operation failed");
        if let Some(app) = self.app() {
            app.set_profiles_busy(false);
            let message = match error.as_str() {
                "Keep at least one profile."
                | "This profile is unavailable."
                | "Enter a profile name of up to 64 characters." => text::tr(&error),
                _ => text::tr("Could not save profile. Try again."),
            };
            app.set_profiles_error(message.into());
        }
    }

    pub(super) fn refresh_profiles(&self) {
        if !profiles::is_initialized() {
            return;
        }
        let rows = match profiles::list() {
            Ok(rows) => rows,
            Err(error) => {
                self.profile_error(error);
                return;
            }
        };
        let active = profiles::active_context().id;
        if let Some(app) = self.app() {
            app.set_profiles_remember_last(
                profiles::device_preferences().open_last_profile_on_startup,
            );
            app.set_profiles(
                Rc::new(VecModel::from(
                    rows.iter()
                        .map(|row| crate::ProfileRow {
                            id: row.id.clone().into(),
                            name: if row.name == "Default" {
                                text::tr("Default").into()
                            } else {
                                row.name.clone().into()
                            },
                            active: row.id == active,
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
        if !rows.iter().any(|row| row.id == active) {
            // Deletion can arrive inside sync projection. Finish that batch in
            // its captured context before activating a surviving profile.
            if self.profile_recovery_pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let replacement = rows
                .first()
                .map(|row| row.id.clone())
                .map(Ok)
                .unwrap_or_else(|| profiles::create("Default"));
            match replacement {
                Ok(id) => {
                    if let Some(app) = self.app() {
                        app.set_profiles_require_selection(true);
                    }
                    let bridge = self.clone();
                    let _ = profiles::invoke_account(move || {
                        bridge
                            .profile_recovery_pending
                            .store(false, Ordering::Release);
                        if let Err(error) = bridge.activate_profile(&id, true) {
                            bridge.profile_error(error);
                        }
                    });
                }
                Err(error) => {
                    self.profile_recovery_pending
                        .store(false, Ordering::Release);
                    self.profile_error(error);
                }
            }
        }
    }

    pub(super) fn create_profile(&self, name: &str) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::create(name) {
            Ok(_) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn rename_profile(&self, id: &str, name: &str) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::rename(id, name) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn delete_profile(&self, id: &str) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::delete(id) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn switch_profile(&self, id: &str) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        if let Err(error) = self.activate_profile(id, false) {
            self.profile_error(error);
        }
    }

    pub(super) fn remember_profile_on_startup(&self, enabled: bool) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::set_open_last_profile_on_startup(enabled) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    fn activate_profile(&self, id: &str, require_selection: bool) -> Result<(), String> {
        if !profiles::list()?.iter().any(|profile| profile.id == id) {
            return Err("This profile is unavailable.".into());
        }
        let active = profiles::active_context();
        let loaded = self.profile_loaded_epoch.load(Ordering::Acquire) == active.epoch;
        let changed = active.id != id || !loaded;
        if changed {
            // All outgoing checkpoints retain their old profile ownership.
            if loaded && profiles::writable(&profiles::current_id()) {
                self.persist_outgoing_playback();
                self.flush_playback_preferences(true);
                self.persist_settings();
            }
            persistence::flush()?;
            let incoming = profiles::prepare_activation(id)?;
            self.party_leave();
            self.remote_disconnect();
            self.clear_addon_subtitles();
            #[cfg(target_os = "android")]
            android_player::player_brightness_reset();
            self.finish_playback_request();
            self.player.close();
            self.shutdown_tracking();
            self.shutdown_stremio();
            self.catalog_gen.fetch_add(1, Ordering::AcqRel);
            self.home_showcase_gen.fetch_add(1, Ordering::AcqRel);
            self.home_catalog_rows_gen.fetch_add(1, Ordering::AcqRel);
            profiles::commit_activation(incoming);
            if let Some(app) = self.app() {
                app.set_profiles_require_selection(true);
                app.set_profiles_open(true);
                app.set_more_open(true);
            }
            // The caller may itself be an outgoing-profile UI continuation.
            return profiles::with_context(profiles::active_context(), || {
                self.load_profile()?;
                self.finish_profile_selection(require_selection);
                Ok(())
            });
        }
        self.finish_profile_selection(require_selection);
        Ok(())
    }

    fn load_profile(&self) -> Result<(), String> {
        *self.shared.lock().unwrap() = Shared {
            chosen_addon: usize::MAX,
            restoring_models: true,
            cache_settings: read_settings(),
            download_settings: read_download_settings(),
            entries: read_persisted_library(),
            progress: read_progress_map(),
            continue_hidden: read_continue_hidden(),
            ..Shared::default()
        };
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        set_active_cache_settings(settings.clone());
        nova_providers::plugin_settings::configure_plugin_settings(Arc::new(
            plugin_settings::Backend::new(),
        ))
        .map_err(|error| error.to_string())?;
        persistence::register_collections();
        self.settings_to_ui();
        self.load_search_history();
        *self.image_usage_scan.lock().unwrap() = Default::default();
        *self.torrent_usage_scan.lock().unwrap() = Default::default();
        self.clear_streams();
        if let Some(app) = self.app() {
            app.set_catalog(Default::default());
            app.set_addon_zip_import_busy(false);
            app.set_addon_zip_import_status("".into());
            app.set_addon_config_open(false);
            app.set_addon_config_url("".into());
            app.set_addon_config_label("".into());
            app.set_plugin_settings_notice("".into());
            app.set_completion_prompt_open(false);
            app.set_tracking_open(false);
            app.set_tracking_notice("".into());
            app.set_tracking_accounts(Default::default());
            app.set_tracking_links(Default::default());
            app.set_tracking_candidates(Default::default());
            app.set_tracking_setup(Default::default());
            app.set_tracking_setup_active(false);
            app.set_tracking_source_rows(Default::default());
            app.set_tracking_preview_rows(Default::default());
            app.set_tracking_mapping_rows(Default::default());
            app.set_tracking_busy(false);
            app.set_tracking_can_confirm(false);
            app.set_stremio_connected(false);
            app.set_stremio_busy(false);
            app.set_stremio_account("".into());
            app.set_stremio_status("".into());
            app.set_stremio_link_code("".into());
            app.set_stremio_link_url("".into());
            app.set_stremio_preview("".into());
            app.set_stremio_pending("".into());
            app.set_stremio_merge_ready(false);
            app.set_modal_visible(false);
            app.set_stream_selector_open(false);
            app.set_categories_modal(false);
            app.set_discover_search_open(false);
            app.set_discover_search_focused(false);
            app.set_library_search_open(false);
            app.set_party_page_open(false);
            app.set_remote_controls_open(false);
            app.set_remote_devices_open(false);
        }
        self.shared.lock().unwrap().loading_addons = true;
        for addon in read_persisted_addons() {
            nova_providers::configure_provider_capabilities(
                &addon.url,
                addon.capabilities.unwrap_or_default(),
            );
            self.install_persisted(
                &addon.url,
                addon.enabled,
                addon.configure_ok,
                Some(addon.label),
            );
        }
        self.shared.lock().unwrap().loading_addons = false;
        self.refresh_all(true);
        addon_packages::reconcile(self);
        self.migrate_viewing_activity();
        self.restore_playback_preferences_schema();
        self.restore_last_stream_choice();
        plugin_settings::restore_retained_settings().map_err(|error| error.to_string())?;
        self.seed_playback_default_settings();
        self.sync_seed();
        self.restore_sync_projection();
        self.sync_reconcile_progress_aliases();
        self.remote_control_policy_changed(settings.remote_control_enabled);
        self.shared.lock().unwrap().restoring_models = false;
        self.apply_library_to_ui();
        self.profile_loaded_epoch
            .store(profiles::active_context().epoch, Ordering::Release);
        self.refresh_profiles();
        Ok(())
    }

    fn finish_profile_selection(&self, require_selection: bool) {
        if !require_selection {
            self.initialize_tracking();
            self.start_stremio();
        }
        if let Some(app) = self.app() {
            app.set_profiles_require_selection(require_selection);
            app.set_profiles_open(require_selection);
            app.set_show_calendar(false);
            app.set_show_downloads(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_profiles_busy(false);
            self.show_home_page();
            if require_selection {
                app.set_show_home(false);
                app.set_profiles_open(true);
                app.set_more_open(true);
            } else {
                app.set_more_open(false);
            }
        }
    }
}

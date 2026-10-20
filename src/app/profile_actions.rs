//! Profile navigation swaps personal state while account sync stays connected.
use super::*;
use unicode_normalization::UnicodeNormalization;

// Hash the durable ID, not the name or row index: a profile keeps its colors
// after a rename, sync reorder, restart or selection on another device. Both
// colors vary; curated pairs keep even small initials readable on every theme.
const AVATAR_COLORS: [(u32, u32); 12] = [
    (0x3c254b, 0xffd5b5),
    (0xecc3d5, 0x502550),
    (0x254633, 0xc5efcf),
    (0x153b50, 0xb9e9ff),
    (0xe4d2a5, 0x4e361d),
    (0x352c64, 0xded3ff),
    (0xa9ddd2, 0x153d36),
    (0x543025, 0xffdac1),
    (0xd0d8fa, 0x293667),
    (0x3f401b, 0xe5eead),
    (0xedc8b2, 0x5b2c20),
    (0x253f54, 0xc8e2f7),
];

fn profile_row(profile: &profiles::Profile, active: &str) -> crate::ProfileRow {
    let name = if profile.name == "Default" {
        text::tr("Default")
    } else {
        profile.name.as_str()
    };
    let initial = name
        .trim()
        .nfc()
        .next()
        .and_then(|letter| letter.to_uppercase().next())
        .unwrap_or('?')
        .to_string();
    // Explicit FNV-1a avoids the randomized, implementation-dependent default
    // hasher, so the same synced identity has the same appearance everywhere.
    let hash = profile
        .id
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let (background, foreground) = AVATAR_COLORS[(hash % AVATAR_COLORS.len() as u64) as usize];
    crate::ProfileRow {
        id: profile.id.clone().into(),
        name: name.into(),
        active: profile.id == active,
        initial: initial.into(),
        avatar_background: slint::Color::from_argb_encoded(0xff000000 | background),
        avatar_foreground: slint::Color::from_argb_encoded(0xff000000 | foreground),
    }
}

pub(super) struct WorkerDone(pub Arc<AtomicBool>);
impl Drop for WorkerDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn read_owned<T: serde::de::DeserializeOwned + Default>(
    store: &profiles::ProfileStore,
    key: &str,
) -> Result<T, String> {
    store
        .read(key)
        .map_err(|error| error.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|error| format!("Unreadable {key}: {error}")))
        .transpose()
        .map(Option::unwrap_or_default)
}

struct PreparedProfile {
    session: Arc<profile_session::ProfileSession>,
    addons: Vec<AddonStore>,
    plugins: nova_providers::plugin_settings::PreparedPluginSettings,
}

impl PreparedProfile {
    fn load(bridge: &Bridge, id: &str) -> Result<Self, String> {
        let store = profiles::ProfileStore::for_profile(id);
        // Settings recovery retains unknown fields by durable owner. Even
        // detached preparation must attribute that bookkeeping to the incoming
        // profile, without granting it access to an activation's mutable state.
        store.run(|| {
            let settings = match store.read("settings").map_err(|error| error.to_string())? {
                Some(raw) => recover_settings(
                    serde_json::from_str(&raw)
                        .map_err(|error| format!("Unreadable settings: {error}"))?,
                )
                .ok_or("Unreadable settings object retained")?,
                None => CacheSettings::default(),
            };
            let state = Shared {
                chosen_addon: usize::MAX,
                restoring_models: true,
                cache_settings: settings,
                download_settings: read_download_settings(),
                entries: read_owned(&store, "library")?,
                progress: read_owned(&store, EPISODE_PROGRESS_KEY)?,
                continue_hidden: read_owned(&store, CONTINUE_HIDDEN_KEY)?,
                ..Shared::default()
            };
            let addons = read_owned(&store, "addons")?;
            addon_packages::validate_owned(&store)?;
            let plugins = plugin_settings::prepare_backend(store.clone())?;
            Ok(Self {
                session: bridge.sessions.prepare(id, state),
                addons,
                plugins,
            })
        })
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
                        .map(|row| profile_row(row, &active))
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
            // Validate storage and provider configuration before stopping the
            // outgoing player or actors. Failure leaves the selected session live.
            let prepared = PreparedProfile::load(self, id)?;
            let incoming = profiles::prepare_activation(id)?;
            self.cancel_season_download();
            self.party_leave();
            self.remote_disconnect();
            self.clear_addon_subtitles();
            #[cfg(target_os = "android")]
            android_player::player_brightness_reset();
            self.finish_playback_request();
            self.player.close();
            self.shutdown_tracking();
            self.shutdown_stremio();
            self.shutdown_list_services();
            self.catalog_gen.fetch_add(1, Ordering::AcqRel);
            self.home_showcase_gen.fetch_add(1, Ordering::AcqRel);
            self.home_catalog_rows_gen.fetch_add(1, Ordering::AcqRel);
            nova_providers::plugin_settings::activate_plugin_settings(prepared.plugins);
            let _ = prepared
                .session
                .providers
                .set(nova_providers::plugin_settings::capture_settings_capability());
            self.sessions.publish(prepared.session.clone());
            profiles::commit_session(incoming, &prepared.session);
            if let Some(app) = self.app() {
                app.set_profiles_require_selection(true);
                app.set_profiles_open(true);
                app.set_more_open(true);
            }
            // The caller may itself be an outgoing-profile UI continuation.
            return profiles::with_context(profiles::active_context(), || {
                self.load_profile(prepared.addons);
                self.finish_profile_selection(require_selection);
                Ok(())
            });
        }
        self.finish_profile_selection(require_selection);
        Ok(())
    }

    fn load_profile(&self, addons: Vec<AddonStore>) {
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        set_active_cache_settings(settings.clone());
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
            // Directory controls are transient personal view state. Clear the
            // shared UI globals before publishing the incoming inventory.
            app.set_addon_browser_tab(0);
            app.set_addon_browser_source(0);
            app.set_addon_browser_directory_url("".into());
            app.set_addon_browser_search("".into());
            app.set_addon_browser_filter(0);
            app.set_addon_browser_rows(Default::default());
            app.set_addon_browser_source_name("".into());
            app.set_addon_browser_source_url("".into());
            app.set_addon_browser_status("".into());
            app.set_addon_browser_busy(false);
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
            app.set_stremio_mode_index(0);
            app.set_stremio_title_busy(false);
            app.set_stremio_title_linked(false);
            app.set_stremio_title_can_confirm(false);
            app.set_stremio_title_status("".into());
            app.set_stremio_title_match("".into());
            app.set_stremio_title_candidates(Default::default());
            app.set_stremio_title_mapping(Default::default());
            app.set_stremio_title_options(Default::default());
            list_services::clear_ui(&app);
            app.set_stremio_busy(false);
            app.set_stremio_account("".into());
            app.set_stremio_status("".into());
            app.set_stremio_link_code("".into());
            app.set_stremio_link_url("".into());
            app.set_stremio_preview("".into());
            app.set_stremio_pending("".into());
            app.set_stremio_merge_ready(false);
            app.set_modal_visible(false);
            app.set_selected_details(Default::default());
            app.set_stream_selector_open(false);
            app.set_season_download_active(false);
            app.set_season_download_review(false);
            app.set_season_download_status(Default::default());
            app.set_season_download_rows(Default::default());
            app.global::<nova_ui_shared::DetailBridge>()
                .set_season_download_menu(-1);
            app.set_categories_modal(false);
            app.set_discover_search_open(false);
            app.set_discover_search_focused(false);
            app.set_library_search_open(false);
            app.set_party_page_open(false);
            app.set_remote_controls_open(false);
            app.set_remote_devices_open(false);
        }
        self.shared.lock().unwrap().loading_addons = true;
        addon_packages::initialize(self);
        for addon in addons {
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
        self.seed_playback_default_settings();
        self.sync_seed();
        self.restore_sync_projection();
        self.sync_reconcile_progress_aliases();
        self.remote_control_policy_changed(settings.remote_control_enabled);
        self.shared.lock().unwrap().restoring_models = false;
        self.apply_library_to_ui();
        self.refresh_watch_queue();
        self.refresh_viewing_journal();
        self.refresh_playback_tools();
        self.profile_loaded_epoch
            .store(profiles::active_context().epoch, Ordering::Release);
        self.refresh_profiles();
    }

    fn finish_profile_selection(&self, require_selection: bool) {
        if !require_selection {
            self.sessions
                .active()
                .services_requested
                .store(true, Ordering::Release);
            self.retry_profile_services();
        }
        if let Some(app) = self.app() {
            let from_picker = app.get_profiles_open();
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
                if from_picker {
                    app.invoke_animate_profile_home_entry();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_display_names_while_colors_follow_identity() {
        let mut profile = profiles::Profile {
            id: "stable-profile".into(),
            name: "  e\u{301}va  ".into(),
        };
        let original = profile_row(&profile, &profile.id);
        assert_eq!(original.initial, "É");
        for (name, expected) in [("mišo", "M"), ("张伟", "张"), ("", "?")] {
            profile.name = name.into();
            let renamed = profile_row(&profile, "another-profile");
            assert_eq!(renamed.initial, expected);
            assert_eq!(renamed.avatar_background, original.avatar_background);
            assert_eq!(renamed.avatar_foreground, original.avatar_foreground);
        }
        let pairs = (0..128)
            .map(|index| {
                profile.id = format!("device:{index}");
                let row = profile_row(&profile, "");
                (
                    row.avatar_background.as_argb_encoded(),
                    row.avatar_foreground.as_argb_encoded(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(pairs.len(), AVATAR_COLORS.len());
    }

    #[test]
    fn every_avatar_color_pair_meets_small_text_contrast() {
        fn luminance(rgb: u32) -> f64 {
            let linear = |shift: u32| {
                let channel = f64::from((rgb >> shift) & 0xff_u32) / 255.0;
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
        }
        for (background, foreground) in AVATAR_COLORS {
            let a = luminance(background);
            let b = luminance(foreground);
            let contrast = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(
                contrast >= 4.5,
                "{background:06x}/{foreground:06x}: {contrast}"
            );
        }
    }
}

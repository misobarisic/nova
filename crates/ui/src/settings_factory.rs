use nova_ui_settings::SettingsModule;
use nova_ui_shared::SettingsBridge;
use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    crate::forward_app_callbacks!(app, app.global::<SettingsBridge>();
        on_search_matches => invoke_settings_bridge_search_matches(arg0, arg1),
    );
    crate::forward_app_callbacks!(app, app.global::<SettingsBridge>(); recover_focus;
        on_view_changed => invoke_settings_bridge_view_changed(arg0),
        on_tracking_connect => invoke_settings_bridge_tracking_connect(arg0, arg1, arg2),
        on_tracking_complete => invoke_settings_bridge_tracking_complete(arg0, arg1),
        on_tracking_disconnect => invoke_settings_bridge_tracking_disconnect(arg0),
        on_tracking_cancel => invoke_settings_bridge_tracking_cancel(arg0),
        on_tracking_automatic => invoke_settings_bridge_tracking_automatic(arg0, arg1),
        on_playback_language_search => invoke_settings_bridge_playback_language_search(arg0, arg1),
        on_playback_language_picked => invoke_settings_bridge_playback_language_picked(arg0, arg1),
        on_playback_language_edited => invoke_settings_bridge_playback_language_edited(arg0, arg1, arg2),
        on_addon_added => invoke_settings_bridge_addon_added(arg0),
        on_addon_browser_load => invoke_settings_bridge_addon_browser_load(arg0),
        on_addon_browser_filter_changed => invoke_settings_bridge_addon_browser_filter_changed(),
        on_addon_browser_action => invoke_settings_bridge_addon_browser_action(arg0),
        on_addon_browser_source_open => invoke_settings_bridge_addon_browser_source_open(),
        on_addon_toggled => invoke_settings_bridge_addon_toggled(arg0),
        on_addon_remove => invoke_settings_bridge_addon_remove(arg0),
        on_addon_move_up => invoke_settings_bridge_addon_move_up(arg0),
        on_addon_move_down => invoke_settings_bridge_addon_move_down(arg0),
        on_addon_capabilities_changed => invoke_settings_bridge_addon_capabilities_changed(arg0, arg1, arg2),
        on_plugin_setting_changed => invoke_settings_bridge_plugin_setting_changed(arg0,arg1,arg2),
        on_plugin_settings_clear => invoke_settings_bridge_plugin_settings_clear(arg0),
        on_plugin_cache_clear => invoke_settings_bridge_plugin_cache_clear(arg0),
        on_addon_configure => invoke_settings_bridge_addon_configure(arg0),
        on_addon_refresh => invoke_settings_bridge_addon_refresh(arg0),
        on_addon_copy_link => invoke_settings_bridge_addon_copy_link(arg0),
        on_playback_speed_stepped => invoke_settings_bridge_playback_speed_stepped(arg0),
        on_subtitle_style_changed => invoke_settings_bridge_subtitle_style_changed(arg0, arg1),
        on_category_added => invoke_settings_bridge_category_added(arg0),
        on_category_removed => invoke_settings_bridge_category_removed(arg0),
        on_home_catalog_candidate_picked => invoke_settings_bridge_home_catalog_candidate_picked(arg0),
        on_home_catalog_added => invoke_settings_bridge_home_catalog_added(arg0, arg1),
        on_home_catalog_removed => invoke_settings_bridge_home_catalog_removed(arg0),
        on_home_row_catalog_removed => invoke_settings_bridge_home_row_catalog_removed(arg0),
        on_home_row_catalog_action => invoke_settings_bridge_home_row_catalog_action(arg0, arg1),
        on_settings_edited => invoke_settings_bridge_settings_edited(arg0),
        on_torrent_dir_edited => invoke_settings_bridge_torrent_dir_edited(arg0),
        on_download_auto_delete_watched_changed => invoke_settings_bridge_download_auto_delete_watched_changed(arg0),
        on_download_list_remove => invoke_settings_bridge_download_list_remove(arg0),
        on_sync_add_peer => invoke_settings_bridge_sync_add_peer(arg0),
        on_sync_remove_peer => invoke_settings_bridge_sync_remove_peer(arg0),
        on_sync_device_name_edited => invoke_settings_bridge_sync_device_name_edited(arg0),
        on_sync_pair_confirm_changed => invoke_settings_bridge_sync_pair_confirm_changed(arg0),
        on_sync_local_discovery_changed => invoke_settings_bridge_sync_local_discovery_changed(arg0),
        on_sync_interval_picked => invoke_settings_bridge_sync_interval_picked(arg0),
        on_sync_background_changed => invoke_settings_bridge_sync_background_changed(arg0),
        on_sync_cancel_invite => invoke_settings_bridge_sync_cancel_invite(arg0),
        on_sync_copy_ticket => invoke_settings_bridge_sync_copy_ticket(arg0),
        on_sync_join_invite => invoke_settings_bridge_sync_join_invite(arg0),
        on_sync_respond_pair => invoke_settings_bridge_sync_respond_pair(arg0, arg1),
        on_cancel_reencode => invoke_settings_bridge_cancel_reencode(),
        on_tracking_reset => invoke_settings_bridge_tracking_reset(),
        on_list_services_connect => invoke_settings_bridge_list_services_connect(arg0, arg1, arg2),
        on_list_services_disconnect => invoke_settings_bridge_list_services_disconnect(arg0),
        on_list_services_cancel => invoke_settings_bridge_list_services_cancel(arg0),
        on_list_services_sync => invoke_settings_bridge_list_services_sync(arg0),
        on_list_services_automatic_changed => invoke_settings_bridge_list_services_automatic_changed(arg0, arg1),
        on_list_services_scope_changed => invoke_settings_bridge_list_services_scope_changed(arg0, arg1),
        on_list_services_open_account => invoke_settings_bridge_list_services_open_account(arg0),
        on_stremio_connect => invoke_settings_bridge_stremio_connect(),
        on_stremio_cancel => invoke_settings_bridge_stremio_cancel(),
        on_stremio_start => invoke_settings_bridge_stremio_start(),
        on_stremio_sync => invoke_settings_bridge_stremio_sync(),
        on_stremio_automatic_changed => invoke_settings_bridge_stremio_automatic_changed(arg0),
        on_stremio_mode_changed => invoke_settings_bridge_stremio_mode_changed(arg0),
        on_stremio_disconnect => invoke_settings_bridge_stremio_disconnect(),
        on_stremio_open_link => invoke_settings_bridge_stremio_open_link(),

        on_addon_filter_changed => invoke_settings_bridge_addon_filter_changed(),
        on_addon_zip_import_requested => invoke_settings_bridge_addon_zip_import_requested(),
        on_discover_picked => invoke_settings_bridge_discover_picked(),
        on_home_picked => invoke_settings_bridge_home_picked(),
        on_back_picked => invoke_settings_bridge_back_picked(),
        on_library_picked => invoke_settings_bridge_library_picked(),
        on_settings_picked => invoke_settings_bridge_settings_picked(),
        on_about_picked => invoke_settings_bridge_about_picked(),
        on_more_picked => invoke_settings_bridge_more_picked(),
        on_home_catalog_add_requested => invoke_settings_bridge_home_catalog_add_requested(),
        on_home_row_catalog_add_requested => invoke_settings_bridge_home_row_catalog_add_requested(),
        on_save_settings => invoke_settings_bridge_save_settings(),
        on_clear_cache => invoke_settings_bridge_clear_cache(),
        on_reencode_cache => invoke_settings_bridge_reencode_cache(),
        on_torrents_changed => invoke_settings_bridge_torrents_changed(),
        on_clear_torrent_cache => invoke_settings_bridge_clear_torrent_cache(),
        on_sync_changed => invoke_settings_bridge_sync_changed(),
        on_sync_now => invoke_settings_bridge_sync_now(),
        on_sync_copy_identity => invoke_settings_bridge_sync_copy_identity(),
        on_sync_create_invite => invoke_settings_bridge_sync_create_invite(),
        on_sync_scan_qr => invoke_settings_bridge_sync_scan_qr(),
    );
    let weak = app.as_weak();
    app.global::<SettingsBridge>()
        .set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            let shared = app.shared_globals_novauishared();
            nova_ui_settings::prepare_submenus(&shared);
            match SettingsModule::new_embedded_with_shared_globals(context, shared) {
                Ok(page) => {
                    nova_ui_settings::install_submenus(&page);
                    let weak = page.as_weak();
                    app.global::<SettingsBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    // A failed attachment must never fall back to replacing the
                    // window root or creating an unrelated hidden native window.
                    eprintln!("nova-ui: could not attach Settings page: {error}");
                    None
                }
            }
        }));
}

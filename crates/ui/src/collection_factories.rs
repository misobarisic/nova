use nova_ui_shared::{AboutBridge, DownloadsBridge, LibraryBridge, MoreBridge};
use slint::ComponentHandle;

// Factories retain only a weak AppWindow and reuse its exact shared globals bundle.
pub(crate) fn install(app: &crate::generated::AppWindow) {
    {
        let bridge = app.global::<LibraryBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_view_changed => invoke_library_bridge_view_changed(),
            on_discover_picked => invoke_library_bridge_discover_picked(),
            on_home_picked => invoke_library_bridge_home_picked(),
            on_library_picked => invoke_library_bridge_library_picked(),
            on_settings_picked => invoke_library_bridge_settings_picked(),
            on_more_picked => invoke_library_bridge_more_picked(),
            on_about_picked => invoke_library_bridge_about_picked(),
            on_library_filter_picked => invoke_library_bridge_library_filter_picked(arg0),
            on_library_item_picked => invoke_library_bridge_library_item_picked(arg0),
            on_remove_library_item => invoke_library_bridge_remove_library_item(arg0),
            on_library_watch_action => invoke_library_bridge_library_watch_action(arg0, arg1),
            on_library_status_action => invoke_library_bridge_library_status_action(arg0, arg1),
            on_library_visible_range => invoke_library_bridge_library_visible_range(arg0, arg1),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_library::LibraryModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<LibraryBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Library page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<MoreBridge>();
        crate::forward_app_callbacks!(app, &bridge; recover_focus;
            on_profile_create => invoke_profile_create(arg0),
            on_profile_rename => invoke_profile_rename(arg0, arg1),
            on_profile_switch => invoke_profile_switch(arg0),
            on_profile_delete => invoke_profile_delete(arg0),
            on_profile_remember_last_changed => invoke_profile_remember_last_changed(arg0),
        );
        crate::forward_app_callbacks!(app, &bridge;
            on_home_picked => invoke_more_bridge_home_picked(),
            on_discover_picked => invoke_more_bridge_discover_picked(),
            on_library_picked => invoke_more_bridge_library_picked(),
            on_calendar_picked => invoke_more_bridge_calendar_picked(),
            on_downloads_picked => invoke_more_bridge_downloads_picked(),
            on_sync_picked => invoke_more_bridge_sync_picked(),
            on_settings_picked => invoke_more_bridge_settings_picked(),
            on_about_picked => invoke_more_bridge_about_picked(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_more::MoreModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<MoreBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach More page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<AboutBridge>();
        crate::forward_app_callbacks!(app, &bridge; recover_focus;
            on_update_check_requested => invoke_about_bridge_update_check_requested(),
            on_update_automatic_changed => invoke_about_bridge_update_automatic_changed(arg0),
            on_update_release_requested => invoke_about_bridge_update_release_requested(),
            on_home_picked => invoke_about_bridge_home_picked(),
            on_back_picked => invoke_about_bridge_back_picked(),
            on_discover_picked => invoke_about_bridge_discover_picked(),
            on_library_picked => invoke_about_bridge_library_picked(),
            on_settings_picked => invoke_about_bridge_settings_picked(),
            on_about_picked => invoke_about_bridge_about_picked(),
            on_more_picked => invoke_about_bridge_more_picked(),
            on_license_source_requested => invoke_about_bridge_license_source_requested(arg0),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_more::AboutModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<AboutBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach About page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<DownloadsBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_artwork_visible => invoke_downloads_bridge_artwork_visible(arg0, arg1),
            on_artwork_active_changed => invoke_downloads_bridge_artwork_active_changed(arg0),
        );
        crate::forward_app_callbacks!(app, &bridge; recover_focus;
            on_query_changed => invoke_downloads_bridge_query_changed(arg0),
            on_filter_changed => invoke_downloads_bridge_filter_changed(arg0),
            on_sort_changed => invoke_downloads_bridge_sort_changed(arg0),
            on_group_toggled => invoke_downloads_bridge_group_toggled(arg0),
            on_action => invoke_downloads_bridge_action(arg0, arg1),
            on_back => invoke_downloads_bridge_back(),
            on_home_picked => invoke_downloads_bridge_home_picked(),
            on_discover_picked => invoke_downloads_bridge_discover_picked(),
            on_library_picked => invoke_downloads_bridge_library_picked(),
            on_settings_picked => invoke_downloads_bridge_settings_picked(),
            on_about_picked => invoke_downloads_bridge_about_picked(),
            on_more_picked => invoke_downloads_bridge_more_picked(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_more::DownloadsModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<DownloadsBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Downloads page: {error}");
                    None
                }
            }
        }));
    }
}

use crate::SettingsModule;
use nova_ui_shared::{NovaUiSharedSharedGlobals, SettingsContent};
use slint::ComponentHandle;

/// Reset only transient shell state before constructing a fresh Settings tree.
pub fn prepare_submenus(shared: &std::rc::Rc<NovaUiSharedSharedGlobals>) {
    let content = SettingsContent(
        shared.global_SettingsContent.clone(),
        std::marker::PhantomData,
    );
    content.invoke_reset_state();
    content.set_content_min_height(0.0);
    content.set_content_preferred_height(0.0);
    content.set_tracking_factory(slint::ComponentFactory::default());
    content.set_addons_factory(slint::ComponentFactory::default());
    content.set_categories_factory(slint::ComponentFactory::default());
    content.set_image_cache_factory(slint::ComponentFactory::default());
    content.set_display_factory(slint::ComponentFactory::default());
    content.set_p2p_factory(slint::ComponentFactory::default());
    content.set_player_factory(slint::ComponentFactory::default());
    content.set_theme_factory(slint::ComponentFactory::default());
    content.set_look_feel_factory(slint::ComponentFactory::default());
    content.set_sync_factory(slint::ComponentFactory::default());
    content.set_home_factory(slint::ComponentFactory::default());
    content.set_downloads_factory(slint::ComponentFactory::default());
    content.set_downloaded_episodes_factory(slint::ComponentFactory::default());
}

/// Install all submenu factories on the shell’s existing window adapter.
pub fn install_submenus(app: &SettingsModule) {
    let shared = app.shared_globals_novauishared();
    let content = SettingsContent(
        shared.global_SettingsContent.clone(),
        std::marker::PhantomData,
    );
    let weak = app.as_weak();
    content.on_kb_player_audio_language_row(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_kb_player_audio_language_row())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_kb_player_speed_row(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_kb_player_speed_row())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_kb_player_subtitle_language_row(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_kb_player_subtitle_language_row())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_open_page(move |id| {
        weak.upgrade()
            .map(|app| app.invoke_content_open_page(id))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_pick_button_radius(move |index| {
        weak.upgrade()
            .map(|app| app.invoke_content_pick_button_radius(index))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_pick_card_radius(move |index| {
        weak.upgrade()
            .map(|app| app.invoke_content_pick_card_radius(index))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_pick_card_spacing(move |index| {
        weak.upgrade()
            .map(|app| app.invoke_content_pick_card_spacing(index))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_restore_theme_defaults(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_restore_theme_defaults())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_reveal_detail_control(move |y, h| {
        weak.upgrade()
            .map(|app| app.invoke_content_reveal_detail_control(y, h))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_schedule_save(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_schedule_save())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_schedule_setting(move |field| {
        weak.upgrade()
            .map(|app| app.invoke_content_schedule_setting(field))
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.on_focus_shell(move || {
        weak.upgrade()
            .map(|app| app.invoke_content_focus_shell())
            .unwrap_or_default()
    });
    let weak = app.as_weak();
    content.set_tracking_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_tracking::SettingsTrackingModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/tracking: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_addons_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_addons::SettingsAddonsModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/addons: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_categories_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(shared.global_SettingsContent.clone(), std::marker::PhantomData);
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_categories::SettingsCategoriesModule::new_embedded_with_shared_globals(context, shared) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/categories: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_image_cache_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(shared.global_SettingsContent.clone(), std::marker::PhantomData);
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_image_cache::SettingsImageCacheModule::new_embedded_with_shared_globals(context, shared) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/image-cache: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_display_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_display::SettingsDisplayModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/display: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_p2p_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_p2p::SettingsP2pModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/p2p: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_player_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_player::SettingsPlayerModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/player: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_theme_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_theme::SettingsThemeModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/theme: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_look_feel_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_look_feel::SettingsLookFeelModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/look-feel: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_sync_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_sync::SettingsSyncModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/sync: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_home_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_home::SettingsHomeModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/home: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_downloads_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(
            shared.global_SettingsContent.clone(),
            std::marker::PhantomData,
        );
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_downloads::SettingsDownloadsModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/downloads: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_downloaded_episodes_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        app.invoke_content_prepare();
        let shared = app.shared_globals_novauishared();
        let content = SettingsContent(shared.global_SettingsContent.clone(), std::marker::PhantomData);
        content.set_content_min_height(0.0);
        content.set_content_preferred_height(0.0);
        match nova_ui_settings_downloaded_episodes::SettingsDownloadedEpisodesModule::new_embedded_with_shared_globals(context, shared) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Settings/downloaded-episodes: {error}");
                None
            }
        }
    }));
}

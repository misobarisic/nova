use nova_ui_shared::{OnboardingBridge, PlayerOsdBridge};
use slint::ComponentHandle;

// Install once; data and language changes update bindings without remounting.
pub(crate) fn install(app: &crate::generated::AppWindow) {
    {
        let bridge = app.global::<PlayerOsdBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_next_episode_streams => invoke_player_bridge_next_episode_streams(arg0),
            on_next_episode_dismiss => invoke_player_bridge_next_episode_dismiss(arg0),
            on_restore_playback_default => invoke_player_bridge_restore_playback_default(arg0),
            on_osd_opacity_changed => invoke_player_bridge_osd_opacity_changed(arg0),
            on_subtitle_clearance_changed => invoke_player_bridge_subtitle_clearance_changed(arg0, arg1),
            on_toggle_pause => invoke_player_bridge_toggle_pause(),
            on_seek => invoke_player_bridge_seek(arg0),
            on_close_player => invoke_player_bridge_close_player(),
            on_take_screenshot => invoke_player_bridge_take_screenshot(),
            on_set_volume => invoke_player_bridge_set_volume(arg0),
            on_toggle_mute => invoke_player_bridge_toggle_mute(),
            on_pick_audio_track => invoke_player_bridge_pick_audio_track(arg0),
            on_pick_sub_track => invoke_player_bridge_pick_sub_track(arg0),
            on_pick_decoder => invoke_player_bridge_pick_decoder(arg0),
            on_playback_speed_changed => invoke_player_bridge_playback_speed_changed(arg0),
            on_playback_speed_stepped => invoke_player_bridge_playback_speed_stepped(arg0),
            on_playback_speed_preview => invoke_player_bridge_playback_speed_preview(arg0),
            on_android_brightness_step => invoke_player_bridge_android_brightness_step(arg0),
            on_android_volume_step => invoke_player_bridge_android_volume_step(arg0),
            on_settings_opened => invoke_player_bridge_settings_opened(),
            on_toggle_fullscreen => invoke_player_bridge_toggle_fullscreen(),
            on_osd_mouse_moved => invoke_player_bridge_osd_mouse_moved(),
            on_tracks_popup_opened => invoke_player_bridge_tracks_popup_opened(arg0),
            on_format_time => invoke_player_bridge_format_time(arg0),
            on_format_volume => invoke_player_bridge_format_volume(arg0),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_player_osd::PlayerOsdModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<PlayerOsdBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach PlayerOsd page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<OnboardingBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_add_addon => invoke_onboarding_bridge_add_addon(arg0),
            on_cinemeta => invoke_onboarding_bridge_cinemeta(),
            on_setting_edited => invoke_onboarding_bridge_setting_edited(arg0),
            on_language_search => invoke_onboarding_bridge_language_search(arg0, arg1),
            on_language_picked => invoke_onboarding_bridge_language_picked(arg0, arg1),
            on_finished => invoke_onboarding_bridge_finished(),
            on_background_requested => invoke_onboarding_bridge_background_requested(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_onboarding::OnboardingModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<OnboardingBridge>().on_focus_shell(move || {
                        let Some(page) = weak.upgrade() else {
                            return;
                        };
                        let window = i_slint_core::window::WindowInner::from_pub(page.window());
                        // The wizard covers an underlying page that can still
                        // own focus after lazy factory construction. Replace
                        // that focus, but preserve wizard inputs and popups.
                        if !window.active_popups().is_empty() {
                            return;
                        }
                        let page_tree: i_slint_core::item_tree::ItemTreeRc =
                            slint::private_unstable_api::re_exports::VRc::into_dyn(
                                page.clone_strong().into(),
                            );
                        let mut focused = window.focus_item.borrow().upgrade();
                        while let Some(item) = focused {
                            if item.is_root_item_of(&page_tree) {
                                return;
                            }
                            focused = item.parent_item(
                                i_slint_core::item_tree::ParentItemTraversalMode::StopAtPopups,
                            );
                        }
                        page.invoke_focus_shell();
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Onboarding page: {error}");
                    None
                }
            }
        }));
    }
}

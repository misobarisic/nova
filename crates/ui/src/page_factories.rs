use nova_ui_shared::{
    AboutBridge, CalendarBridge, DetailBridge, DiscoverBridge, DownloadsBridge, HomeBridge,
    OnboardingBridge, SettingsBridge,
};
use slint::ComponentHandle;

// Shared globals cache the last value even after the weak host binding dies.
// Only inputs that were one-way in AppWindow may be reset: clearing a linked
// backend property would discard the state needed when returning to the page.
fn release_unmounted_payloads(app: &crate::generated::AppWindow, page: i32) {
    if app.invoke_page_bridge_is_mounted(page) {
        return;
    }
    match page {
        0 => {
            let bridge = app.global::<HomeBridge>();
            drop(bridge.get_continue_rows());
            drop(bridge.get_new_episode_rows());
            drop(bridge.get_upcoming_rows());
            bridge.set_catalog_sections(Default::default());
            bridge.set_catalog_order(Default::default());
            bridge.set_catalog_positions(Default::default());
            bridge.set_catalog_cards(Default::default());
            bridge.set_history_archive_cards(Default::default());
            drop(bridge.get_cal_cells());
            drop(bridge.get_cal_day_rows());
            bridge.set_featured_logo(Default::default());
            drop(bridge.get_featured_backdrop());
        }
        1 => {
            let bridge = app.global::<DiscoverBridge>();
            drop(bridge.get_catalog());
            drop(bridge.get_addon_names());
            drop(bridge.get_type_names());
            drop(bridge.get_catalog_names());
            bridge.set_catalog_labels(Default::default());
            drop(bridge.get_genre_names());
            drop(bridge.get_search_results());
            bridge.set_search_history(Default::default());
            bridge.set_search_history_contexts(Default::default());
            bridge.set_search_addon_names(Default::default());
            bridge.set_search_type_names(Default::default());
            bridge.set_search_catalog_names(Default::default());
            bridge.set_search_catalog_labels(Default::default());
            bridge.set_search_genre_names(Default::default());
        }
        2 => {
            let bridge = app.global::<CalendarBridge>();
            bridge.set_cells(Default::default());
            bridge.set_week_cells(Default::default());
            bridge.set_rows(Default::default());
            bridge.set_later(Default::default());
        }
        3 => {
            let bridge = app.global::<DetailBridge>();
            drop(bridge.get_selected_details());
            bridge.set_tracking_links(Default::default());
            bridge.set_tracking_candidates(Default::default());
            bridge.set_tracking_setup(Default::default());
            bridge.set_tracking_source_rows(Default::default());
            bridge.set_tracking_preview_rows(Default::default());
            bridge.set_tracking_mapping_rows(Default::default());
            drop(bridge.get_selected_poster());
            drop(bridge.get_selected_backdrop());
            bridge.set_selected_logo(Default::default());
            drop(bridge.get_selected_genre_list());
            drop(bridge.get_category_rows());
            drop(bridge.get_streams());
            drop(bridge.get_stream_addons());
            drop(bridge.get_stream_loading());
            drop(bridge.get_stream_action_items());
            drop(bridge.get_season_names());
            drop(bridge.get_season_cards());
            drop(bridge.get_episode_rows());
        }
        4 => {
            let bridge = app.global::<SettingsBridge>();
            drop(bridge.get_page_scrolls());
            bridge.set_tracking_accounts(Default::default());
            drop(bridge.get_addon_rows());
            drop(bridge.get_category_rows());
            drop(bridge.get_home_catalog_rows());
            drop(bridge.get_home_row_catalog_rows());
            drop(bridge.get_home_catalog_candidate_names());
            drop(bridge.get_home_catalog_genre_names());
            drop(bridge.get_image_resolution_names());
            drop(bridge.get_cache_format_names());
            bridge.set_audio_language_codes(Default::default());
            bridge.set_audio_language_names(Default::default());
            bridge.set_subtitle_language_codes(Default::default());
            bridge.set_subtitle_language_names(Default::default());
            bridge.set_audio_language_selected_codes(Default::default());
            bridge.set_audio_language_selected_names(Default::default());
            bridge.set_subtitle_language_selected_codes(Default::default());
            bridge.set_subtitle_language_selected_names(Default::default());
            drop(bridge.get_language_names());
            drop(bridge.get_downloads_rows());
            drop(bridge.get_sync_peers());
            drop(bridge.get_sync_invite_qr());
            drop(bridge.get_sync_invites());
        }
        7 => {
            let bridge = app.global::<AboutBridge>();
            bridge.set_license_sources(Default::default());
            bridge.set_license_text_chunks(Default::default());
        }
        8 => {
            let bridge = app.global::<DownloadsBridge>();
            bridge.set_groups(Default::default());
            bridge.set_transfers(Default::default());
            bridge.set_counts(Default::default());
        }
        10 => {
            let bridge = app.global::<OnboardingBridge>();
            bridge.set_audio_codes(Default::default());
            bridge.set_audio_names(Default::default());
            bridge.set_subtitle_codes(Default::default());
            bridge.set_subtitle_names(Default::default());
        }
        _ => {}
    }
}

// Register callbacks once on the shared globals. Keep ownership weak.
pub(crate) fn install(app: &crate::generated::AppWindow) {
    let weak = app.as_weak();
    app.on_release_page_payloads(move || {
        if let Some(app) = weak.upgrade() {
            let window = i_slint_core::window::WindowInner::from_pub(app.window());
            window.ensure_tree_instantiated();
            for page in 0..11 {
                release_unmounted_payloads(&app, page);
            }
            recover_back_focus(&app);
        }
    });
    {
        let bridge = app.global::<HomeBridge>();
        // Scroll positions are window-local UI state, not profile data to save
        // or sync. Keys supplied by the app include the profile and row source,
        // so late events from a retired page cannot overwrite another row.
        let offsets = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::<
            String,
            f32,
        >::new()));
        let read = offsets.clone();
        bridge.on_catalog_scroll_offset(move |key| {
            read.borrow().get(key.as_str()).copied().unwrap_or_default()
        });
        bridge.on_catalog_scrolled(move |key, x| {
            if !key.is_empty() && x.is_finite() {
                offsets.borrow_mut().insert(key.to_string(), x.min(0.0));
            }
        });
        crate::forward_app_callbacks!(app, &bridge;
            on_home_picked => invoke_home_bridge_home_picked(),
            on_search_picked => invoke_home_bridge_search_picked(),
            on_discover_picked => invoke_home_bridge_discover_picked(),
            on_library_picked => invoke_home_bridge_library_picked(),
            on_settings_picked => invoke_home_bridge_settings_picked(),
            on_more_picked => invoke_home_bridge_more_picked(),
            on_about_picked => invoke_home_bridge_about_picked(),
            on_continue_picked => invoke_home_bridge_continue_picked(arg0),
            on_new_episode_picked => invoke_home_bridge_new_episode_picked(arg0),
            on_new_episode_enter => invoke_home_bridge_new_episode_enter(arg0),
            on_new_episode_remove => invoke_home_bridge_new_episode_remove(arg0),
            on_history_action => invoke_home_bridge_history_action(arg0, arg1, arg2),
            on_home_episode_action => invoke_home_bridge_home_episode_action(arg0, arg1, arg2, arg3, arg4),
            on_continue_remove => invoke_home_bridge_continue_remove(arg0),
            on_continue_enter => invoke_home_bridge_continue_enter(arg0),
            on_upcoming_picked => invoke_home_bridge_upcoming_picked(arg0),
            on_catalog_card_picked => invoke_home_bridge_catalog_card_picked(arg0),
            on_upcoming_cal_open => invoke_home_bridge_upcoming_cal_open(),
            on_upcoming_cal_close => invoke_home_bridge_upcoming_cal_close(),
            on_upcoming_cal_month => invoke_home_bridge_upcoming_cal_month(arg0),
            on_upcoming_cal_pick => invoke_home_bridge_upcoming_cal_pick(arg0),
            on_upcoming_cal_activate => invoke_home_bridge_upcoming_cal_activate(),
            on_featured_picked => invoke_home_bridge_featured_picked(),
            on_featured_watch_now => invoke_home_bridge_featured_watch_now(),
            on_featured_step => invoke_home_bridge_featured_step(arg0),
            on_exit_to_background => invoke_home_bridge_exit_to_background(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_home::HomeModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<HomeBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Home page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<DiscoverBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_addon_picked => invoke_discover_bridge_addon_picked(arg0),
            on_type_picked => invoke_discover_bridge_type_picked(arg0),
            on_catalog_picked => invoke_discover_bridge_catalog_picked(arg0),
            on_genre_picked => invoke_discover_bridge_genre_picked(arg0),
            on_search_activated => invoke_discover_bridge_search_activated(),
            on_search_scope_picked => invoke_discover_bridge_search_scope_picked(arg0),
            on_search_filter_picked => invoke_discover_bridge_search_filter_picked(arg0, arg1),
            on_search_edited => invoke_discover_bridge_search_edited(arg0),
            on_search_submitted => invoke_discover_bridge_search_submitted(arg0),
            on_search_back_picked => invoke_discover_bridge_search_back_picked(),
            on_search_history_cleared => invoke_discover_bridge_search_history_cleared(),
            on_search_history_selected => invoke_discover_bridge_search_history_selected(arg0),
            on_search_history_item_removed_at => invoke_discover_bridge_search_history_item_removed_at(arg0),
            on_search_history_item_removed => invoke_discover_bridge_search_history_item_removed(arg0),
            on_item_selected => invoke_discover_bridge_item_selected(arg0, arg1),
            on_discover_picked => invoke_discover_bridge_discover_picked(),
            on_home_picked => invoke_discover_bridge_home_picked(),
            on_library_picked => invoke_discover_bridge_library_picked(),
            on_settings_picked => invoke_discover_bridge_settings_picked(),
            on_more_picked => invoke_discover_bridge_more_picked(),
            on_about_picked => invoke_discover_bridge_about_picked(),
            on_load_more => invoke_discover_bridge_load_more(),
            on_continue_pagination => invoke_discover_bridge_continue_pagination(),
            on_visible_range => invoke_discover_bridge_visible_range(arg0, arg1),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_discover::DiscoverModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    let anchor_page = page.as_weak();
                    app.global::<DiscoverBridge>()
                        .on_remap_catalog_anchor(move |old, new| {
                            if let Some(page) = anchor_page.upgrade() {
                                page.invoke_remap_catalog_anchor(old, new);
                            }
                        });
                    app.global::<DiscoverBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Discover page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<CalendarBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_artwork_visible => invoke_calendar_bridge_artwork_visible(arg0, arg1),
            on_artwork_active_changed => invoke_calendar_bridge_artwork_active_changed(arg0),
        );
        crate::forward_app_callbacks!(app, &bridge; recover_focus;
            on_refreshed => invoke_calendar_bridge_refreshed(),
            on_step => invoke_calendar_bridge_step(arg0),
            on_agenda_more => invoke_calendar_bridge_agenda_more(arg0),
            on_today => invoke_calendar_bridge_today(),
            on_day_picked => invoke_calendar_bridge_day_picked(arg0),
            on_release_picked => invoke_calendar_bridge_release_picked(arg0),
            on_back => invoke_calendar_bridge_back(),
            on_home_picked => invoke_calendar_bridge_home_picked(),
            on_discover_picked => invoke_calendar_bridge_discover_picked(),
            on_library_picked => invoke_calendar_bridge_library_picked(),
            on_settings_picked => invoke_calendar_bridge_settings_picked(),
            on_about_picked => invoke_calendar_bridge_about_picked(),
            on_more_picked => invoke_calendar_bridge_more_picked(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            match nova_ui_calendar::CalendarModule::new_embedded_with_shared_globals(
                context,
                app.shared_globals_novauishared(),
            ) {
                Ok(page) => {
                    let weak = page.as_weak();
                    app.global::<CalendarBridge>().on_focus_shell(move || {
                        if let Some(page) = weak.upgrade() {
                            page.invoke_focus_shell();
                        }
                    });
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Calendar page: {error}");
                    None
                }
            }
        }));
    }
    {
        let bridge = app.global::<DetailBridge>();
        crate::forward_app_callbacks!(app, &bridge;
            on_tracking_show => invoke_detail_bridge_tracking_show(),
            on_list_services_title_open => invoke_detail_bridge_list_services_title_open(arg0),
            on_list_services_title_select => invoke_detail_bridge_list_services_title_select(arg0, arg1, arg2),
            on_list_services_title_sync => invoke_detail_bridge_list_services_title_sync(arg0),
            on_tracking_close => invoke_detail_bridge_tracking_close(),
            on_tracking_search => invoke_detail_bridge_tracking_search(arg0, arg1),
            on_tracking_pick => invoke_detail_bridge_tracking_pick(arg0),
            on_tracking_preview => invoke_detail_bridge_tracking_preview(arg0, arg1, arg2),
            on_tracking_confirm => invoke_detail_bridge_tracking_confirm(),
            on_stremio_title_unlink => invoke_detail_bridge_stremio_title_unlink(),
            on_stremio_title_confirm => invoke_detail_bridge_stremio_title_confirm(arg0),
            on_stremio_title_assign => invoke_detail_bridge_stremio_title_assign(arg0, arg1),
            on_stremio_title_pick => invoke_detail_bridge_stremio_title_pick(arg0),
            on_stremio_title_search => invoke_detail_bridge_stremio_title_search(arg0),
            on_tracking_accept_setup => invoke_detail_bridge_tracking_accept_setup(arg0, arg1),
            on_tracking_adjust_setup => invoke_detail_bridge_tracking_adjust_setup(arg0),
            on_tracking_cancel_adjust => invoke_detail_bridge_tracking_cancel_adjust(),
            on_tracking_assign => invoke_detail_bridge_tracking_assign(arg0, arg1),
            on_tracking_edit => invoke_detail_bridge_tracking_edit(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7),
            on_tracking_action => invoke_detail_bridge_tracking_action(arg0, arg1),
            on_back => invoke_detail_bridge_back(),
            on_episodes_back => invoke_detail_bridge_episodes_back(),
            on_add_to_library => invoke_detail_bridge_add_to_library(),
            on_library_action => invoke_detail_bridge_library_action(arg0),
            on_open_categories => invoke_detail_bridge_open_categories(),
            on_season_download_start => invoke_detail_bridge_season_download_start(arg0, arg1),
            on_season_download_retry => invoke_detail_bridge_season_download_retry(),
            on_season_download_confirm => invoke_detail_bridge_season_download_confirm(),
            on_season_download_cancel => invoke_detail_bridge_season_download_cancel(),
            on_season_picked => invoke_detail_bridge_season_picked(arg0),
            on_episode_picked => invoke_detail_bridge_episode_picked(arg0),
            on_episode_page_picked => invoke_detail_bridge_episode_page_picked(arg0),
            on_episode_toggle_watched => invoke_detail_bridge_episode_toggle_watched(arg0),
            on_episode_watch_action => invoke_detail_bridge_episode_watch_action(arg0, arg1),
            on_season_watch_action => invoke_detail_bridge_season_watch_action(arg0, arg1),
            on_stream_picked => invoke_detail_bridge_stream_picked(arg0),
            on_stream_page_picked => invoke_detail_bridge_stream_page_picked(arg0),
            on_stream_action_requested => invoke_detail_bridge_stream_action_requested(arg0),
            on_stream_action_selected => invoke_detail_bridge_stream_action_selected(arg0, arg1),
            on_stream_filter_picked => invoke_detail_bridge_stream_filter_picked(arg0),
            on_stream_refresh_requested => invoke_detail_bridge_stream_refresh_requested(),
            on_stream_selector_back => invoke_detail_bridge_stream_selector_back(),
            on_detail_tab_picked => invoke_detail_bridge_detail_tab_picked(arg0),
            on_episode_filter_changed => invoke_detail_bridge_episode_filter_changed(arg0),
            on_detail_extra_picked => invoke_detail_bridge_detail_extra_picked(arg0, arg1),
            on_watch_now => invoke_detail_bridge_watch_now(),
        );
        let weak = app.as_weak();
        bridge.set_factory(slint::ComponentFactory::new(move |context| {
            let app = weak.upgrade()?;
            let shared = app.shared_globals_novauishared();
            nova_ui_detail::prepare_subpages(&shared);
            match nova_ui_detail::DetailModule::new_embedded_with_shared_globals(context, shared) {
                Ok(page) => {
                    nova_ui_detail::install_subpages(&page);
                    Some(page)
                }
                Err(error) => {
                    eprintln!("nova-ui: could not attach Detail page: {error}");
                    None
                }
            }
        }));
    }
}

/// A row/model rebuild can destroy native focus without unmounting its page.
/// Android treats an unhandled Back as permission to finish the activity, so
/// publish removals before checking ownership and recover only missing/hidden
/// focus. Live editors (including scrolled ones) and popups keep their keys.
pub(crate) fn recover_back_focus(app: &crate::generated::AppWindow) {
    recover_focus(app, false);
}

fn recover_focus(app: &crate::generated::AppWindow, check_tree: bool) {
    let window = i_slint_core::window::WindowInner::from_pub(app.window());
    window.ensure_tree_instantiated();
    if app.get_completion_prompt_visible() || !window.active_popups().is_empty() {
        return;
    }
    let focused = window.focus_item.borrow().upgrade();
    let missing_focus = focused.is_none_or(|item| {
        if !item.is_visible() {
            return true;
        }
        if !check_tree {
            return false;
        }
        // During a callback the native event stack can keep a removed row
        // alive and apparently visible. Only the current tree owns input.
        // This walk is limited to user actions, never periodic progress ticks.
        let root = i_slint_core::item_tree::ItemRc::new_root(window.component());
        item != root
            && root
                .visit_descendants(|live| {
                    if live == &item {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                })
                .is_none()
    });
    if missing_focus {
        app.invoke_preserve_back_focus();
    }
    app.invoke_focus_mounted_page();
}

pub(crate) fn recover_after_callback(app: &crate::generated::AppWindow) {
    recover_focus(app, true);
    // A click/key event can retain the removed delegate until its stack
    // unwinds. Recheck once then, retaining any new live focus or popup.
    let weak = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(app) = weak.upgrade() {
            recover_back_focus(&app);
        }
    });
}

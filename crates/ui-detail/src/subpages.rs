use crate::DetailModule;
use nova_ui_shared::{DetailBridge, DetailContent, NovaUiSharedSharedGlobals};
use slint::{ComponentHandle, Model};

/// Clear transient subtree state before a fresh Detail shell is constructed.
pub fn prepare_subpages(shared: &std::rc::Rc<NovaUiSharedSharedGlobals>) {
    let content = DetailContent(
        shared.global_DetailContent.clone(),
        std::marker::PhantomData,
    );
    content.set_tracking_factory(slint::ComponentFactory::default());
    content.set_streams_factory(slint::ComponentFactory::default());
    content.set_episodes_factory(slint::ComponentFactory::default());
    content.set_entry_factory(slint::ComponentFactory::default());
    content.set_episodes_generation(content.get_episodes_generation().wrapping_add(1));
    content.set_entry_generation(content.get_entry_generation().wrapping_add(1));
    content.set_episodes_height(0.0);
    content.set_entry_height(0.0);
    content.set_active_entry_id(slint::SharedString::default());
    content.set_active_entry_index(0);
    content.set_entry_more(false);
    content.set_entry_editing(false);
    content.set_entry_history_confirm(false);
    content.set_entry_unlink_confirm(false);
    content.set_episode_search_open(false);
    content.set_filter_focused(false);
    content.set_kb_edit(false);
    content.set_tracking_back_request(0);
    content.set_tracking_focus_request(0);
}

/// Factories reuse the native parent slot and initialized shared globals.
/// Weak callbacks prevent shared factory state from retaining the page tree.
pub fn install_subpages(app: &DetailModule) {
    let shared = app.shared_globals_novauishared();
    let content = DetailContent(
        shared.global_DetailContent.clone(),
        std::marker::PhantomData,
    );
    let weak = app.as_weak();
    content.on_dispatch_back(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_dispatch_back();
        }
    });
    let weak = app.as_weak();
    content.on_focus_shell(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_focus_shell();
        }
    });
    let weak = app.as_weak();
    content.on_filters_panned(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_filters_panned();
        }
    });
    let weak = app.as_weak();
    content.on_filters_pan_released(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_filters_pan_released();
        }
    });
    let weak = app.as_weak();
    content.on_open_ep_menu(move |index, watched, progress| {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_open_ep_menu(index, watched, progress);
        }
    });
    let weak = app.as_weak();
    content.on_open_season_menu(move |index, watched| {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_open_season_menu(index, watched);
        }
    });
    let weak = app.as_weak();
    content.on_scroll_list_to_top(move |y| {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_scroll_list_to_top(y);
        }
    });
    let weak = app.as_weak();
    content.on_request_stream_action(move |id| {
        if let Some(app) = weak.upgrade() {
            app.invoke_content_request_stream_action(id);
        }
    });
    let weak = app.as_weak();
    content.on_entry_index(move |id| {
        let Some(app) = weak.upgrade() else {
            return -1;
        };
        let shared = app.shared_globals_novauishared();
        let bridge = DetailBridge(shared.global_DetailBridge.clone(), std::marker::PhantomData);
        let rows = bridge.get_tracking_links();
        for index in 0..rows.row_count() {
            if rows.row_data(index).is_some_and(|row| row.id == id) {
                return index as i32;
            }
        }
        -1
    });
    let weak = app.as_weak();
    content.set_tracking_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        let shared = app.shared_globals_novauishared();
        let content = DetailContent(
            shared.global_DetailContent.clone(),
            std::marker::PhantomData,
        );
        match nova_ui_detail_tracking::DetailTrackingModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => {
                let weak = page.as_weak();
                content.on_recover_entry_focus(move || {
                    if let Some(page) = weak.upgrade() {
                        page.invoke_recover_entry_focus();
                    }
                });
                let weak = page.as_weak();
                content.on_validate_entry(move || {
                    if let Some(page) = weak.upgrade() {
                        page.invoke_validate_entry();
                    }
                });
                let weak = page.as_weak();
                content.on_focus_tracking(move || {
                    if let Some(page) = weak.upgrade() {
                        page.invoke_focus_tracking();
                    }
                });
                Some(page)
            }
            Err(error) => {
                eprintln!("nova-ui: could not attach Detail/tracking: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_streams_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        let shared = app.shared_globals_novauishared();
        match nova_ui_detail_streams::DetailStreamsModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => {
                let shared = app.shared_globals_novauishared();
                let content = DetailContent(
                    shared.global_DetailContent.clone(),
                    std::marker::PhantomData,
                );
                let weak = page.as_weak();
                content.on_focus_streams(move || {
                    if let Some(page) = weak.upgrade() {
                        page.invoke_focus_shell();
                    }
                });
                Some(page)
            }
            Err(error) => {
                eprintln!("nova-ui: could not attach Detail/streams: {error}");
                None
            }
        }
    }));
    let weak = app.as_weak();
    content.set_episodes_factory(slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        let shared = app.shared_globals_novauishared();
        match nova_ui_detail_episodes::DetailEpisodesModule::new_embedded_with_shared_globals(
            context, shared,
        ) {
            Ok(page) => {
                let shared = app.shared_globals_novauishared();
                let content = DetailContent(
                    shared.global_DetailContent.clone(),
                    std::marker::PhantomData,
                );
                let weak = page.as_weak();
                content.on_restore_episode_focus(move || {
                    if let Some(page) = weak.upgrade() {
                        page.invoke_restore_episode_focus();
                    }
                });
                Some(page)
            }
            Err(error) => {
                eprintln!("nova-ui: could not attach Detail/episodes: {error}");
                None
            }
        }
    }));
    content.set_entry_factory(entry_factory(app));
    let weak = app.as_weak();
    content.on_activate_entry(move |id, index| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let shared = app.shared_globals_novauishared();
        let content = DetailContent(
            shared.global_DetailContent.clone(),
            std::marker::PhantomData,
        );
        content.set_entry_generation(content.get_entry_generation().wrapping_add(1));
        content.set_entry_height(0.0);
        content.set_active_entry_index(index);
        content.set_active_entry_id(id);
        content.set_entry_more(true);
        content.set_entry_factory(entry_factory(&app));
    });
}

// Only choosing a different stable ID replaces this factory. Model/progress
// updates and resize keep the mounted entry and its unsaved input values.
fn entry_factory(app: &DetailModule) -> slint::ComponentFactory {
    let weak = app.as_weak();
    slint::ComponentFactory::new(move |context| {
        let app = weak.upgrade()?;
        let shared = app.shared_globals_novauishared();
        match nova_ui_detail_tracking_entry::DetailTrackingEntryModule::new_embedded_with_shared_globals(context, shared) {
            Ok(page) => Some(page),
            Err(error) => {
                eprintln!("nova-ui: could not attach Detail/entry: {error}");
                None
            }
        }
    })
}

use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    let bridge = app.global::<nova_ui_shared::CatalogGroupingBridge>();
    crate::forward_app_callbacks!(app, &bridge; recover_focus;
        on_default_changed => invoke_catalog_grouping_bridge_default_changed(arg0),
        on_infer_series_changed => invoke_catalog_grouping_bridge_infer_series_changed(arg0),
        on_override_changed => invoke_catalog_grouping_bridge_override_changed(arg0,arg1,arg2),
        on_override_removed => invoke_catalog_grouping_bridge_override_removed(arg0),
        on_search_changed => invoke_catalog_grouping_bridge_search_changed(arg0),
        on_member_picked => invoke_catalog_grouping_bridge_member_picked(arg0),
        on_chooser_closed => invoke_catalog_grouping_bridge_chooser_closed(),
        on_detail_all_seasons => invoke_catalog_grouping_bridge_detail_all_seasons(),
    );
    let weak = app.as_weak();
    bridge.on_visual_closed(move || {
        if let Some(app) = weak.upgrade() {
            crate::page_factories::recover_after_callback(&app);
        }
    });
}

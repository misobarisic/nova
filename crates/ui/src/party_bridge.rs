use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    let bridge = app.global::<nova_ui_shared::PartyBridge>();
    crate::forward_app_callbacks!(app, &bridge; recover_focus;
        on_create => invoke_party_bridge_create(),
        on_join => invoke_party_bridge_join(arg0),
        on_leave => invoke_party_bridge_leave(),
        on_choose_media => invoke_party_bridge_choose_media(),
        on_choose_stream => invoke_party_bridge_choose_stream(),
        on_copy_invite => invoke_party_bridge_copy_invite(),
        on_return_to_player => invoke_party_bridge_return_to_player(),
        on_set_paused => invoke_party_bridge_set_paused(arg0),
        on_seek => invoke_party_bridge_seek(arg0),
        on_remove_member => invoke_party_bridge_remove_member(arg0),
        on_set_wait_for_guests => invoke_party_bridge_set_wait_for_guests(arg0),
        on_continue_without_waiting => invoke_party_bridge_continue_without_waiting(),
    );
}

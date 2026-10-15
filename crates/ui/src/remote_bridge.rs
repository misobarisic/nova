use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    let bridge = app.global::<nova_ui_shared::RemoteBridge>();
    crate::forward_app_callbacks!(app, &bridge;
        on_action => invoke_remote_bridge_action(arg0, arg1, arg2),
        on_device_refresh => invoke_remote_bridge_device_refresh(),
        on_device_open => invoke_remote_bridge_device_open(arg0),
        on_device_selected => invoke_remote_bridge_device_selected(arg0),
        on_device_close => invoke_remote_bridge_device_close(),
        on_stream_picked => invoke_remote_bridge_stream_picked(arg0),
        on_replacement_confirmed => invoke_remote_bridge_replacement_confirmed(arg0, arg1),
    );
}

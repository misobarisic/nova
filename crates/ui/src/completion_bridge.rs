use slint::ComponentHandle;

pub(crate) fn install(app: &crate::generated::AppWindow) {
    crate::forward_app_callbacks!(app, app.global::<nova_ui_shared::CompletionBridge>();
        on_score_changed => invoke_completion_bridge_score_changed(arg0, arg1, arg2),
        on_save => invoke_completion_bridge_save(arg0),
        on_dismiss => invoke_completion_bridge_dismiss(arg0),
    );
}

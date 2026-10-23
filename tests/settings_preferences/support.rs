//! Mock-time interaction helpers shared by the Player preference regressions.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{
    ComponentHandle,
    platform::{Key, WindowEvent},
};
use std::time::Duration;

pub fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
    }
}

pub fn toggle(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
}

pub fn reveal(
    app: &nova::AppWindow,
    find: impl Fn() -> Option<ElementHandle>,
    description: &str,
) -> ElementHandle {
    for attempt in 0..80 {
        settle(app);
        if let Some(item) = find() {
            let y = item.absolute_position().y;
            app.set_settings_scroll_y((app.get_settings_scroll_y() - y + 300.0).min(0.0));
            settle(app);
            // Scrolling can recycle a list item; use the current handle.
            return find().unwrap_or_else(|| panic!("lost settings control {description}"));
        }
        app.set_settings_scroll_y(-80.0 * (attempt + 1) as f32);
    }
    panic!("missing settings control {description}");
}

pub fn key(app: &nova::AppWindow, key: Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}

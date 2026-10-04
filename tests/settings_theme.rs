//! Theme is searchable and its toggle applies without remounting.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..20 {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.size(), e.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn key(app: &nova::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}
#[test]
fn theme_search_toggle_resize_and_android_back() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.on_settings_edited(move |field| {
        recorded.borrow_mut().push(field.to_string());
        let app = weak.upgrade().unwrap();
        nova_ui::apply_theme(&app, app.get_true_black());
    });
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    app.set_settings_search_query("AMOLED".into());
    settle(&app);
    let link = ElementQuery::from_root(&app)
        .match_predicate(|e| e.accessible_id().as_deref() == Some("settings:theme"))
        .find_all()
        .into_iter()
        .next()
        .expect("search finds Theme");
    link.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(app.get_settings_selected_id(), 14);
    assert!(app.get_settings_detail_open());
    let toggle = ElementHandle::find_by_accessible_label(&app, "True black")
        .last()
        .unwrap();
    let p = toggle.absolute_position();
    assert!(p.x >= 0.0 && p.x + toggle.size().width <= 390.0);
    toggle.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(app.get_true_black());
    assert!(app.global::<nova::Theme>().get_true_black());
    assert_eq!(edits.borrow().as_slice(), ["true_black"]);
    let glow = ElementHandle::find_by_element_id(&app, "SettingsPage::settings_backdrop").next();
    assert!(glow.is_none(), "opaque blue header artwork must be hidden");
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    settle(&app);
    assert_eq!(app.get_settings_selected_id(), 14);
    let toggle = ElementHandle::find_by_accessible_label(&app, "True black")
        .last()
        .unwrap();
    let p = toggle.absolute_position();
    assert!(p.x >= 0.0 && p.x + toggle.size().width <= 1280.0);
    // The same mounted row remains reachable by the Settings keyboard scope.
    key(&app, slint::platform::Key::Return.into());
    assert!(!app.get_true_black());
    assert!(!app.global::<nova::Theme>().get_true_black());
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    settle(&app);
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!app.get_settings_detail_open());
    assert_eq!(app.get_settings_search_query(), "AMOLED");
}

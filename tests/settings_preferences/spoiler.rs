//! Spoiler protection remains an explicit profile preference in every layout.
use crate::preference_support::{key, reveal, settle, toggle};
use slint::{ComponentHandle, platform::Key};
use std::{cell::RefCell, rc::Rc};
#[test]
fn spoiler_setting_routes_pointer_keyboard_and_bridge_state() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::spoiler_setting_routes_pointer_keyboard_and_bridge_state"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(3);
    app.set_settings_detail_open(true);
    let fields = Rc::new(RefCell::new(Vec::new()));
    let recorded = fields.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for (width, height, android) in [(390, 844, true), (1280, 800, false), (900, 360, true)] {
        app.set_is_android(android);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_spoiler_protection(false);
        let control = reveal(
            &app,
            || toggle(&app, "Spoiler protection"),
            "Spoiler protection",
        );
        assert_eq!(control.accessible_checked(), Some(false));
        control.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(app.get_spoiler_protection());
        assert!(
            app.global::<nova::SettingsBridge>()
                .get_spoiler_protection()
        );
        assert_eq!(
            fields.borrow().last().map(String::as_str),
            Some("spoiler_protection")
        );
        key(&app, Key::LeftArrow);
        assert!(!app.get_spoiler_protection());
        key(&app, Key::RightArrow);
        assert!(app.get_spoiler_protection());
        // The new row has a stable place between thumbnails and layout in TV
        // navigation while existing catalog grouping indices remain intact.
        key(&app, Key::UpArrow);
        key(&app, Key::DownArrow);
        key(&app, Key::Return);
        assert!(!app.get_spoiler_protection());
    }
}

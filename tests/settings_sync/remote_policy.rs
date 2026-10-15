//! The receiver opt-out is local and usable in both bundled languages.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..16 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

#[test]
fn receiver_opt_out_fits_and_dispatches_in_both_languages() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::receiver_opt_out_fits_and_dispatches_in_both_languages"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.set_sync_enabled(true);
    app.set_settings_selected_id(7);
    app.set_settings_detail_open(true);
    app.global::<nova::Anim>().set_enabled(false);
    let edited = Rc::new(RefCell::new(Vec::new()));
    let captured = edited.clone();
    app.on_settings_edited(move |key| captured.borrow_mut().push(key.to_string()));
    app.window().show().unwrap();
    for (locale, label) in [
        ("en", "Allow remote control"),
        ("hr", "Dopustite daljinsko upravljanje"),
    ] {
        slint::select_bundled_translation(locale).unwrap();
        for width in [320, 390, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1400));
            app.set_settings_scroll_y(0.0);
            app.set_remote_control_enabled(true);
            settle(&app);
            let toggle = ElementHandle::find_by_accessible_label(&app, label)
                .find(|e| e.accessible_role() == Some(AccessibleRole::Checkbox))
                .expect("translated opt-out toggle");
            let position = toggle.absolute_position();
            assert!(position.x >= 0.0 && position.x + toggle.size().width <= width as f32 + 1.0);
            toggle.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert!(!app.get_remote_control_enabled());
            assert!(app.get_sync_enabled(), "opting out must leave sync enabled");
            assert_eq!(
                edited.borrow_mut().pop().as_deref(),
                Some("remote_control_enabled")
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

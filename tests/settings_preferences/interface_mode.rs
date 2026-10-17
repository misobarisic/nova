//! The device-local interface selector shares its state across compiled pages.
#![cfg(not(feature = "live-preview"))]
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.absolute_position(), element.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
    }
}

#[test]
fn selector_publishes_local_edits_and_tv_uses_wide_layout() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::selector_publishes_local_edits_and_tv_uses_wide_layout"
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
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let captured = edits.clone();
    app.on_settings_edited(move |field| captured.borrow_mut().push(field.to_string()));
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    settle(&app);
    assert_eq!(app.get_interface_mode_index(), 0);
    for (label, index) in [("TV", 2), ("Standard", 1), ("Automatic", 0)] {
        ElementHandle::find_by_accessible_label(&app, label)
            .last()
            .expect(label)
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(app.get_interface_mode_index(), index);
        assert_eq!(
            app.global::<nova::SettingsBridge>()
                .get_interface_mode_index(),
            index
        );
        assert_eq!(
            edits.borrow().last().map(String::as_str),
            Some("interface_mode")
        );
    }
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_is_android(true);
    settle(&app);
    assert!(
        app.get_settings_two_pane(),
        "Android TV may use the wide settings layout"
    );
    app.global::<nova::InterfaceStyle>().set_tv(false);
    settle(&app);
    assert!(
        !app.get_settings_two_pane(),
        "Android phone retains its original settings layout"
    );
}

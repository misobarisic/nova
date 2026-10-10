//! Rebuilding addon rows must not leave Android Back without a focus route.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn idle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
}

fn click(app: &nova::AppWindow, label: &str) {
    let control = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label);
    let p = control.absolute_position();
    let s = control.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    idle();
}

fn back(app: &nova::AppWindow) {
    let before = app.get_system_back_request();
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyPressRepeated {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        assert!(
            matches!(
                app.window().dispatch_event_with_result(event),
                Ok(slint::platform::WindowEventDispatchResult::Accepted)
            ),
            "Back must be handled before Android finishes the activity"
        );
    }
    idle();
    assert_eq!(app.get_system_back_request(), before + 1);
}

#[test]
fn addon_actions_keep_back_routed_after_the_focused_row_is_removed() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(0);
    app.set_settings_detail_open(true);
    app.set_addon_rows(
        Rc::new(VecModel::from(vec![nova::AddonRow {
            label: "Cinemeta".into(),
            initial: "C".into(),
            url: "https://example.test/manifest.json".into(),
            enabled: true,
            ..Default::default()
        }]))
        .into(),
    );
    let copies = Rc::new(Cell::new(0));
    let recorded = copies.clone();
    app.on_addon_copy_link(move |_| recorded.set(recorded.get() + 1));
    let weak = app.as_weak();
    app.on_addon_remove(move |_| weak.upgrade().unwrap().set_addon_rows(Default::default()));
    let homes = Rc::new(Cell::new(0));
    let recorded = homes.clone();
    let weak = app.as_weak();
    app.on_home_picked(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_show_settings(false);
        app.set_show_home(true);
    });
    let backgrounds = Rc::new(Cell::new(0));
    let recorded = backgrounds.clone();
    app.on_exit_to_background(move || recorded.set(recorded.get() + 1));
    app.window().set_size(slint::PhysicalSize::new(390, 1300));
    app.window().show().unwrap();
    idle();
    click(&app, "Copy link");
    assert_eq!(copies.get(), 1);
    click(&app, "Remove");
    assert_eq!(app.get_addon_rows().row_count(), 0);
    back(&app);
    assert!(!app.get_settings_detail_open());
    assert!(app.get_show_settings());
    assert_eq!(homes.get(), 0);
    assert_eq!(backgrounds.get(), 0);
    back(&app);
    assert!(app.get_show_home());
    assert_eq!(homes.get(), 1);
    assert_eq!(backgrounds.get(), 0);
}

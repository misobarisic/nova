//! Bundled configuration uses the ordinary gear and retains Settings navigation.
#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}
fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    assert!(p.x >= 0.0 && p.y >= 0.0 && s.width > 0.0 && s.height > 0.0);
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
    settle();
}
fn label(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label)
}

fn toggle(app: &nova::AppWindow, label: &str) -> ElementHandle {
    // The underlying Addons capability filters use the same translated labels.
    // Target the sheet's checkbox, rather than the earlier filter button.
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
        .expect(label)
}

#[test]
fn gear_opens_two_independent_toggles_and_back_closes_the_sheet() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_addon_rows(
        Rc::new(VecModel::from(vec![nova::AddonRow {
            label: "AniKoto".into(),
            url: "nova-provider://anikoto".into(),
            config_url: "nova-provider://anikoto".into(),
            enabled: true,
            ..Default::default()
        }]))
        .into(),
    );
    let weak = app.as_weak();
    app.on_addon_configure(move |url| {
        let app = weak.upgrade().unwrap();
        app.set_addon_config_url(url);
        app.set_addon_config_label("AniKoto".into());
        app.set_addon_config_streams(true);
        app.set_addon_config_metadata(true);
        app.set_addon_config_open(true);
    });
    let changes = Rc::new(RefCell::new(Vec::new()));
    let capture = changes.clone();
    app.on_addon_capabilities_changed(move |url, streams, metadata| {
        capture
            .borrow_mut()
            .push((url.to_string(), streams, metadata));
    });
    app.window().set_size(slint::PhysicalSize::new(390, 1000));
    app.window().show().unwrap();
    settle();
    click(
        &app,
        destinations::find(&app, "settings:addons").next().unwrap(),
    );
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        settle();
        click(&app, label(&app, "Configure"));
        assert!(app.get_addon_config_open());
        assert!(app.get_show_settings());
        click(&app, toggle(&app, "Metadata"));
        assert!(app.get_addon_config_streams());
        assert!(!app.get_addon_config_metadata());
        click(&app, toggle(&app, "Streams"));
        assert!(!app.get_addon_config_streams());
        assert!(!app.get_addon_config_metadata());
        let latest = changes.borrow().last().cloned().unwrap();
        assert_eq!(latest, ("nova-provider://anikoto".into(), false, false));
        let key: SharedString = slint::platform::Key::Escape.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
        settle();
        assert!(!app.get_addon_config_open());
        assert!(
            app.get_show_settings(),
            "Back must close configuration before leaving Addons"
        );
    }
}

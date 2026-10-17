//! Production About bridge and nonmodal update notice, without network requests.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { UpdateNotice } from "../../crates/ui/update-notice.slint";
    export component NoticeHarness inherits Window {
        width: 500px; height: 400px;
        out property <int> background_clicks: 0;
        TouchArea { width: 100%; height: 100%; clicked => { root.background_clicks += 1; } }
        UpdateNotice {
            x: 20px; y: 20px; width: 460px;
            installed_version: "v0.0.29";
            latest_version: "v0.0.30";
        }
    }
}

#[test]
fn notice_body_consumes_pointer_events_without_blocking_the_page_outside_it() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::notice_body_consumes_pointer_events_without_blocking_the_page_outside_it"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = NoticeHarness::new().unwrap();
    app.show().unwrap();
    let click = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
    };
    for (x, y) in [(25.0, 25.0), (55.0, 55.0), (150.0, 55.0), (150.0, 90.0)] {
        click(x, y);
    }
    assert_eq!(
        app.get_background_clicks(),
        0,
        "notice body must shield the page"
    );
    click(250.0, 350.0);
    assert_eq!(
        app.get_background_clicks(),
        1,
        "the surrounding page remains interactive"
    );
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .last()
        .unwrap_or_else(|| panic!("missing action {label}"))
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn hold_ok(app: &nova::AppWindow) {
    let text: slint::SharedString = slint::platform::Key::Return.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    for _ in 0..5 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressRepeated { text: text.clone() });
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

#[test]
fn tv_notice_actions_and_about_activation_handle_held_ok_once() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_notice_actions_and_about_activation_handle_held_ok_once"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.set_show_home(false);
    app.set_show_about(true);
    app.set_update_checks_enabled(true);
    app.set_update_is_development(false);
    app.set_update_available(true);
    app.set_update_latest_version("v0.0.30".into());
    let releases = Rc::new(Cell::new(0));
    let count = releases.clone();
    app.on_update_release_requested(move || count.set(count.get() + 1));
    let automatic = Rc::new(Cell::new(0));
    let count = automatic.clone();
    app.on_update_automatic_changed(move |_| count.set(count.get() + 1));
    app.window().show().unwrap();
    settle(&app);
    key(&app, slint::platform::Key::DownArrow);
    hold_ok(&app);
    assert_eq!(
        automatic.get(),
        1,
        "held OK must toggle automatic checks once"
    );
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::DownArrow);
    hold_ok(&app);
    assert_eq!(releases.get(), 1, "held OK must open the release once");
    app.set_update_notice_visible(true);
    settle(&app);
    key(&app, slint::platform::Key::Menu);
    key(&app, slint::platform::Key::RightArrow);
    hold_ok(&app);
    assert_eq!(releases.get(), 2);
    assert!(!app.get_update_notice_visible());
    app.set_update_notice_visible(true);
    settle(&app);
    key(&app, slint::platform::Key::Back);
    assert!(!app.get_update_notice_visible());
    assert!(
        app.get_show_about(),
        "dismissing the notice must retain the current page"
    );
}

#[test]
fn about_displays_version_and_forwards_update_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::about_displays_version_and_forwards_update_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1000, 1000));
    app.set_show_home(false);
    app.set_show_about(true);
    app.set_app_version("v0.0.29-abc12345".into());
    app.set_update_checks_enabled(true);
    app.set_update_is_development(false);
    app.set_update_available(true);
    app.set_update_latest_version("v0.0.30".into());
    app.set_update_status(3);
    let checks = Rc::new(Cell::new(0));
    let count = checks.clone();
    app.on_update_check_requested(move || count.set(count.get() + 1));
    let releases = Rc::new(Cell::new(0));
    let count = releases.clone();
    app.on_update_release_requested(move || count.set(count.get() + 1));
    let automatic = Rc::new(Cell::new(true));
    let value = automatic.clone();
    app.on_update_automatic_changed(move |enabled| value.set(enabled));
    app.window().show().unwrap();
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Version v0.0.29-abc12345")
            .next()
            .is_some()
    );
    button(&app, "Check for updates").mock_single_click(slint::platform::PointerEventButton::Left);
    button(&app, "View release").mock_single_click(slint::platform::PointerEventButton::Left);
    button(&app, "Automatically check for updates")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(checks.get(), 1);
    assert_eq!(releases.get(), 1);
    assert!(!automatic.get());
    app.set_show_about(false);
    settle(&app);
    app.set_show_about(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Version v0.0.29-abc12345")
            .next()
            .is_some()
    );
    app.set_update_checks_enabled(false);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Check for updates")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Update checks are disabled in this build.")
            .next()
            .is_some()
    );
}

#[test]
fn notice_actions_fit_phone_landscape_and_desktop_and_defer_during_playback() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::notice_actions_fit_phone_landscape_and_desktop_and_defer_during_playback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_app_version("v0.0.29-abc12345-dirty".into());
    app.set_update_checks_enabled(true);
    app.set_update_available(true);
    app.set_update_latest_version("v0.0.30".into());
    let releases = Rc::new(Cell::new(0));
    let count = releases.clone();
    app.on_update_release_requested(move || count.set(count.get() + 1));
    app.window().show().unwrap();
    for (language, later, view) in [
        ("en", "Later", "View release"),
        ("hr", "Kasnije", "Otvori izdanje"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height) in [(360, 640), (640, 360), (1280, 800)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            app.set_update_notice_visible(true);
            settle(&app);
            for label in [later, view] {
                let action = button(&app, label);
                let pos = action.absolute_position();
                let size = action.size();
                assert!(
                    pos.x >= 0.0
                        && pos.y >= 0.0
                        && pos.x + size.width <= width as f32
                        && pos.y + size.height <= height as f32,
                    "{label} escapes {width}x{height}: {pos:?} {size:?}"
                );
            }
            button(&app, later).mock_single_click(slint::platform::PointerEventButton::Left);
            assert!(!app.get_update_notice_visible());
            app.set_update_notice_visible(true);
            settle(&app);
            button(&app, view).mock_single_click(slint::platform::PointerEventButton::Left);
            assert!(!app.get_update_notice_visible());
        }
    }
    slint::select_bundled_translation("en").unwrap();
    assert_eq!(releases.get(), 6);
    app.set_update_notice_visible(true);
    app.set_player_open(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Later")
            .next()
            .is_none()
    );
    app.set_player_open(false);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Later")
            .next()
            .is_some()
    );
    app.set_update_checks_enabled(false);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Later")
            .next()
            .is_none()
    );
}

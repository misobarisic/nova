//! More-page routing and its Settings/About/Devices/Watch party/Profiles destinations.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    // Force geometry evaluation, as the renderer normally does each frame.
    let _ = ElementHandle::find_by_element_id(app, "AppWindow::system_back_scope")
        .next()
        .map(|e| e.size());
}

fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
    settle(app);
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}

fn switch(app: &nova::AppWindow, page: i32) {
    app.set_show_home(page == 0);
    app.set_show_library(page == 2);
    app.set_show_settings(page == 3);
    app.set_show_about(page == 4);
}

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    if app.get_more_open() && label != "Back" {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(
                    app.window().size().width as f32 / 2.0,
                    180.0,
                ),
                delta_x: 0.0,
                delta_y: 10000.0,
            });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        for _ in 0..20 {
            let found = ElementHandle::find_by_accessible_label(app, label).any(|e| {
                let p = e.absolute_position();
                let h = e.size().height;
                e.accessible_role() == Some(AccessibleRole::Button)
                    && p.y >= 0.0
                    && p.y + h <= app.window().size().height as f32 - 68.0
            });
            if found {
                break;
            }
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(
                        app.window().size().width as f32 / 2.0,
                        180.0,
                    ),
                    delta_x: 0.0,
                    delta_y: -80.0,
                });
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
    }
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

fn open_more(app: &nova::AppWindow) {
    click(
        app,
        ElementHandle::find_by_element_id(app, "BottomNav::tab_more")
            .next()
            .unwrap(),
    );
    assert!(app.get_more_open());
    assert!(
        ElementHandle::find_by_element_type_name(app, "MorePage")
            .next()
            .is_some()
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(app, "BottomNav").count(),
        1
    );
    assert!(
        ElementHandle::find_by_element_type_name(app, "HomePage")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_element_type_name(app, "DiscoverPage")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_element_type_name(app, "LibraryPage")
            .next()
            .is_none()
    );
}

fn assert_no_bar(app: &nova::AppWindow) {
    assert!(
        ElementHandle::find_by_element_type_name(app, "BottomNav")
            .next()
            .is_none()
    );
}

#[test]
fn more_and_its_subpages_route_back_without_backgrounding_home() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "more_and_its_subpages_route_back_without_backgrounding_home"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(430, 850));
    app.window().show().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let weak = app.as_weak();
    app.on_home_picked(move || switch(&weak.upgrade().unwrap(), 0));
    let weak = app.as_weak();
    app.on_discover_picked(move || switch(&weak.upgrade().unwrap(), 1));
    let weak = app.as_weak();
    app.on_library_picked(move || switch(&weak.upgrade().unwrap(), 2));
    let weak = app.as_weak();
    app.on_settings_picked(move || switch(&weak.upgrade().unwrap(), 3));
    let weak = app.as_weak();
    app.on_about_picked(move || switch(&weak.upgrade().unwrap(), 4));
    let backgrounds = Rc::new(Cell::new(0));
    let count = backgrounds.clone();
    app.on_exit_to_background(move || count.set(count.get() + 1));
    settle(&app);

    // More replaces each root page and Back returns to Home without exiting.
    for page in 0..3 {
        switch(&app, page);
        settle(&app);
        open_more(&app);
        key(&app, slint::platform::Key::Back);
        assert!(!app.get_more_open());
        assert!(app.get_show_home());
        assert!(!app.get_show_library());
        assert_eq!(backgrounds.get(), 0);
    }

    switch(&app, 0);
    settle(&app);
    open_more(&app);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Back").count(),
        1,
        "phone More retains its header Back arrow"
    );
    // A held Android Back cannot dismiss More and then background Home.
    for _ in 0..2 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            });
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        });
    assert!(!app.get_more_open());
    assert_eq!(backgrounds.get(), 0);

    open_more(&app);
    click(&app, button(&app, "Devices"));
    assert!(app.get_remote_devices_open());
    assert!(
        ElementHandle::find_by_element_type_name(&app, "RemoteDevicesPage")
            .next()
            .is_some()
    );
    assert_no_bar(&app);
    // A control owns native focus, so Back must still reach window capture.
    click(&app, button(&app, "Refresh"));
    key(&app, slint::platform::Key::Back);
    assert!(!app.get_remote_devices_open());
    assert!(app.get_more_open(), "Devices Back returns to More");
    assert_eq!(backgrounds.get(), 0);
    key(&app, slint::platform::Key::Back);
    assert!(app.get_show_home());

    open_more(&app);
    click(&app, button(&app, "Sync"));
    assert!(app.get_show_settings() && !app.get_more_open());
    assert!(app.global::<nova::SettingsBridge>().get_standalone_sync());
    assert_eq!(app.get_settings_selected_id(), 7);
    assert_no_bar(&app);
    assert!(
        ElementHandle::find_by_element_id(&app, "SettingsPage::settings_search")
            .next()
            .is_none()
    );
    key(&app, slint::platform::Key::Back);
    assert!(app.get_more_open() && !app.get_show_settings());
    click(&app, button(&app, "Settings"));
    assert!(app.get_show_settings());
    assert_no_bar(&app);
    assert!(!app.global::<nova::SettingsBridge>().get_standalone_sync());
    app.set_settings_search_query("Sync".into());
    settle(&app);
    assert!(
        i_slint_backend_testing::ElementQuery::from_root(&app)
            .match_predicate(|e| e.accessible_id().as_deref() == Some("settings:sync"))
            .find_first()
            .is_none()
    );
    app.set_settings_search_query("Display".into());
    settle(&app);
    click(&app, button(&app, "Display"));
    assert!(app.get_settings_detail_open());
    assert_no_bar(&app);
    key(&app, slint::platform::Key::Back);
    assert!(app.get_show_settings() && !app.get_settings_detail_open());
    assert_no_bar(&app);
    // Back returns a submenu to More, then the root page to Home.
    click(&app, button(&app, "Back"));
    assert!(app.get_more_open());
    assert!(!app.get_show_settings());
    key(&app, slint::platform::Key::Back);
    assert!(app.get_show_home());

    open_more(&app);
    // A fresh keyboard choice starts at Calendar; child returns retain it.
    app.set_more_choice(0);
    app.set_more_focus_zone(1);
    // Keyboard selection belongs to the More page.
    for _ in 0..7 {
        key(&app, slint::platform::Key::DownArrow);
    }
    key(&app, slint::platform::Key::Return);
    assert!(app.get_show_about() && !app.get_more_open());
    assert_no_bar(&app);
    click(
        &app,
        ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
            .next()
            .unwrap(),
    );
    assert!(app.get_about_licenses_open());
    assert_no_bar(&app);
    for (width, android) in [(700, false), (1100, true), (699, true)] {
        app.set_is_android(android);
        app.window().set_size(slint::PhysicalSize::new(width, 850));
        settle(&app);
        assert!(app.get_about_licenses_open(), "rotation retains Licenses");
        assert_no_bar(&app);
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "SideItem").count(),
            if width >= 700 { 6 } else { 0 }
        );
    }
    key(&app, slint::platform::Key::Back);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
    key(&app, slint::platform::Key::Back);
    assert!(app.get_more_open());
    key(&app, slint::platform::Key::Back);
    assert!(app.get_show_home());
    assert_eq!(backgrounds.get(), 0);

    // More remains a full page when the layout crosses to wide.
    open_more(&app);
    app.window().set_size(slint::PhysicalSize::new(700, 850));
    settle(&app);
    assert!(app.get_more_open());
    assert!(
        ElementHandle::find_by_element_type_name(&app, "MorePage")
            .next()
            .is_some()
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SideItem").count(),
        6
    );
    assert_no_bar(&app);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Back").count(),
        0,
        "desktop More is a sidebar destination without a header Back arrow"
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SettingsLink").count(),
        6,
        "wide More contains Profiles, Sync, Devices, Watch party, Settings and About"
    );
    let calendar_y = button(&app, "Calendar").absolute_position().y;
    let downloads_y = button(&app, "Downloads").absolute_position().y;
    let more_y = button(&app, "More").absolute_position().y;
    let settings_y = button(&app, "Settings").absolute_position().y;
    let about_y = button(&app, "About").absolute_position().y;
    assert!(calendar_y < downloads_y && downloads_y < more_y);
    assert!(settings_y < about_y);
    let devices_y = button(&app, "Devices").absolute_position().y;
    let party_y = button(&app, "Watch party").absolute_position().y;
    let profiles_y = button(&app, "Profiles").absolute_position().y;
    let sync_y = button(&app, "Sync").absolute_position().y;
    assert!(
        profiles_y < sync_y
            && sync_y < devices_y
            && devices_y < party_y
            && party_y < settings_y
            && settings_y < about_y
    );
    click(&app, button(&app, "Profiles"));
    assert!(app.get_more_open() && app.get_profiles_open());
    key(&app, slint::platform::Key::Back);
    assert!(app.get_more_open() && !app.get_profiles_open());
    assert_eq!(backgrounds.get(), 0);
    for label in ["Settings", "About", "Devices", "Watch party"] {
        assert_ne!(button(&app, label).accessible_enabled(), Some(false));
    }
    click(&app, button(&app, "Settings"));
    assert!(app.get_show_settings());
    assert!(!app.get_more_open());
    click(
        &app,
        ElementHandle::find_by_element_type_name(&app, "SideItem")
            .nth(5)
            .unwrap(),
    );
    assert!(
        app.get_more_open(),
        "Settings rail More opens the full page"
    );
    click(&app, button(&app, "Settings"));
    assert!(app.get_show_settings() && !app.get_more_open());
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Back").count(),
        1,
        "Settings remains a desktop submenu of More"
    );
    key(&app, slint::platform::Key::Back);
    assert!(
        app.get_more_open(),
        "desktop destination Back returns to More"
    );
    click(&app, button(&app, "About"));
    assert!(app.get_show_about() && !app.get_more_open());
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Back").count(),
        1,
        "About remains a desktop submenu of More"
    );
    key(&app, slint::platform::Key::Back);
    assert!(app.get_more_open(), "About Back returns to desktop More");
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SideItem").count(),
        6,
        "wide More retains the six-item rail"
    );
    click(&app, button(&app, "Devices"));
    assert!(app.get_remote_devices_open());
    key(&app, slint::platform::Key::Back);
    assert!(!app.get_remote_devices_open());
    assert!(app.get_more_open(), "desktop Devices Back returns to More");
    click(&app, button(&app, "Watch party"));
    assert!(app.get_party_page_open());
    assert!(
        ElementHandle::find_by_element_type_name(&app, "PartyPage")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Back);
    assert!(!app.get_party_page_open());
    assert!(
        app.get_more_open(),
        "desktop Watch party Back returns to More"
    );
    assert!(button(&app, "Watch party").size().height >= 44.0);
    assert_eq!(backgrounds.get(), 0);
}

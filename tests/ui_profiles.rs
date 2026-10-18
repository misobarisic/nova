//! Profile selection keeps startup mandatory and names routed by stable IDs.
#[path = "support/isolation.rs"]
mod isolation;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

const PROFILE_NAME: &str = "A long profile name that wraps on a compact phone";

fn settle(app: &nova_ui::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

fn key(app: &nova_ui::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn picker(required: bool) -> nova_ui::AppWindow {
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    app.set_profiles_require_selection(required);
    app.set_profiles_open(true);
    app.set_profiles(
        Rc::new(slint::VecModel::from(vec![nova_ui::ProfileRow {
            id: "stable-profile".into(),
            name: PROFILE_NAME.into(),
            active: true,
        }]))
        .into(),
    );
    app.window().show().unwrap();
    settle(&app);
    app
}

#[test]
fn required_picker_keeps_back_blocked_and_current_profile_selectable() {
    if !isolation::run(concat!(
        module_path!(),
        "::required_picker_keeps_back_blocked_and_current_profile_selectable"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    app.on_profile_switch(move |id| recorded.borrow_mut().push(id.to_string()));
    assert!(app.get_profiles_remember_last());
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        for back in [slint::platform::Key::Escape, slint::platform::Key::Back] {
            key(&app, back.into());
            assert!(app.get_more_open() && app.get_profiles_open());
        }
        assert_eq!(
            button(&app, &format!("Delete profile: {PROFILE_NAME}")).accessible_enabled(),
            Some(false)
        );
        let select = button(&app, &format!("Use profile: {PROFILE_NAME}"));
        assert_eq!(select.accessible_enabled(), Some(true));
        let position = select.absolute_position();
        assert!(position.x >= 0.0);
        assert!(position.x + select.size().width <= width as f32 + 0.5);
        select.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
    }
    assert_eq!(selected.borrow().as_slice(), &["stable-profile"; 3]);
}

#[test]
fn profile_editor_back_preserves_required_picker_and_create_routes_name() {
    if !isolation::run(concat!(
        module_path!(),
        "::profile_editor_back_preserves_required_picker_and_create_routes_name"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    let created = Rc::new(RefCell::new(Vec::new()));
    let recorded = created.clone();
    app.on_profile_create(move |name| recorded.borrow_mut().push(name.to_string()));
    button(&app, "Create profile").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, slint::platform::Key::Back.into());
    assert!(app.get_profiles_open() && app.get_profiles_require_selection());
    button(&app, "Create profile").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, "New profile".into());
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(created.borrow().as_slice(), &["New profile"]);
    assert!(app.get_profiles_open() && app.get_profiles_require_selection());
}

#[test]
fn required_picker_hides_chrome_and_blocks_global_routes() {
    if !isolation::run(concat!(
        module_path!(),
        "::required_picker_hides_chrome_and_blocks_global_routes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        for chrome in ["SideNav", "BottomNav"] {
            assert!(
                ElementHandle::find_by_element_type_name(&app, chrome)
                    .next()
                    .is_none(),
                "required picker must omit {chrome} at {width}px"
            );
        }
        for section in 0..10 {
            app.invoke_navigate_section(section);
            assert!(app.get_more_open() && app.get_profiles_open());
            assert!(!app.get_show_home() && !app.get_show_library());
            assert!(!app.get_show_settings() && !app.get_show_about());
            assert!(!app.get_show_calendar() && !app.get_show_downloads());
        }
        app.invoke_more_bridge_settings_picked();
        app.invoke_more_bridge_about_picked();
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle(&app);
        assert!(app.get_more_open() && app.get_profiles_open());
        assert!(!app.get_show_settings() && !app.get_show_about());
    }
}

#[test]
fn remember_last_defaults_on_and_dispatches_its_own_preference_callback() {
    if !isolation::run(concat!(
        module_path!(),
        "::remember_last_defaults_on_and_dispatches_its_own_preference_callback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(false);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    settle(&app);
    let changed = Rc::new(RefCell::new(Vec::new()));
    let recorded = changed.clone();
    let weak = app.as_weak();
    app.on_profile_remember_last_changed(move |remember| {
        recorded.borrow_mut().push(remember);
        weak.upgrade().unwrap().set_profiles_remember_last(remember);
    });
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    app.on_profile_switch(move |id| recorded.borrow_mut().push(id.to_string()));
    let toggle = || {
        ElementHandle::find_by_accessible_label(&app, "Open last profile on startup")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
            .unwrap()
    };
    assert_eq!(toggle().accessible_checked(), Some(true));
    for expected in [false, true] {
        toggle().mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(app.get_profiles_remember_last(), expected);
        assert_eq!(toggle().accessible_checked(), Some(expected));
        assert!(!app.get_profiles_require_selection());
        assert!(app.get_more_open() && app.get_profiles_open());
    }
    assert_eq!(changed.borrow().as_slice(), &[false, true]);
    assert!(selected.borrow().is_empty());
}

#[test]
fn profiles_preserves_more_scroll_and_ordinary_navigation_returns_to_landing() {
    if !isolation::run(concat!(
        module_path!(),
        "::profiles_preserves_more_scroll_and_ordinary_navigation_returns_to_landing"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(false);
    app.set_profiles_open(false);
    app.window().set_size(slint::PhysicalSize::new(390, 360));
    app.set_more_scroll_y(-120.0);
    settle(&app);
    let scroll = app.get_more_scroll_y();
    assert!(scroll < 0.0);
    app.set_profiles_open(true);
    settle(&app);
    key(&app, slint::platform::Key::Back.into());
    assert!(app.get_more_open() && !app.get_profiles_open());
    assert!((app.get_more_scroll_y() - scroll).abs() < 0.5);
    app.set_profiles_open(true);
    app.invoke_navigate_section(0);
    assert!(!app.get_profiles_open());
    app.invoke_navigate_section(3);
    assert!(app.get_more_open() && !app.get_profiles_open());
}

#[test]
fn failed_startup_preference_write_restores_the_saved_toggle_value() {
    if !isolation::run(concat!(
        module_path!(),
        "::failed_startup_preference_write_restores_the_saved_toggle_value"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(false);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    settle(&app);
    let changed = Rc::new(RefCell::new(Vec::new()));
    let recorded = changed.clone();
    let weak = app.as_weak();
    app.on_profile_remember_last_changed(move |remember| {
        recorded.borrow_mut().push(remember);
        weak.upgrade()
            .unwrap()
            .set_profiles_error("Could not save profile. Try again.".into());
    });
    let toggle = ElementHandle::find_by_accessible_label(&app, "Open last profile on startup")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
        .unwrap();
    assert_eq!(toggle.accessible_checked(), Some(true));
    toggle.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(changed.borrow().as_slice(), &[false]);
    assert!(app.get_profiles_remember_last());
    assert_eq!(toggle.accessible_checked(), Some(true));
}

#[test]
fn compact_profile_actions_keep_readable_widths_and_touch_targets() {
    if !isolation::run(concat!(
        module_path!(),
        "::compact_profile_actions_keep_readable_widths_and_touch_targets"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(false);
    app.set_is_android(true);
    for (language, rename, delete) in [
        ("en", "Rename profile", "Delete profile"),
        ("hr", "Preimenuj profil", "Izbriši profil"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, tv) in [(320, false), (390, false), (1280, true)] {
            app.global::<nova_ui::InterfaceStyle>().set_tv(tv);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            for required in [false, true] {
                app.set_profiles_require_selection(required);
                settle(&app);
                let rename = button(&app, &format!("{rename}: {PROFILE_NAME}"));
                let delete = button(&app, &format!("{delete}: {PROFILE_NAME}"));
                for action in [&rename, &delete] {
                    let size = action.size();
                    let position = action.absolute_position();
                    let minimum = if tv { 120.0 } else { 88.0 };
                    assert!(
                        size.width >= minimum - 0.5,
                        "{language} action at {width}px needs room for a single-line label: {size:?}"
                    );
                    assert!(
                        (size.height - if tv { 56.0 } else { 44.0 }).abs() < 0.5,
                        "short actions retain the normal one-line hit target"
                    );
                    assert!(position.x >= 0.0);
                    assert!(position.x + size.width <= width as f32 + 0.5);
                }
                assert!(
                    rename.absolute_position().x + rename.size().width
                        <= delete.absolute_position().x - 7.5,
                    "profile actions retain their gap without overlapping"
                );
                assert!((rename.absolute_position().y - delete.absolute_position().y).abs() < 0.5);
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

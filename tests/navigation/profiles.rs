//! Profile selection keeps startup mandatory and names routed by stable IDs.
use crate::isolation;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model};
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
        .unwrap_or_else(|| {
            let available = ElementQuery::from_root(app)
                .match_predicate(|element| {
                    element.accessible_role() == Some(AccessibleRole::Button)
                })
                .find_all()
                .into_iter()
                .map(|element| element.accessible_label())
                .collect::<Vec<_>>();
            panic!(
                "missing button {label}; available: {available:?}; picker open: {}",
                app.get_profiles_open()
            )
        })
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
            initial: "A".into(),
            avatar_background: slint::Color::from_rgb_u8(60, 37, 75),
            avatar_foreground: slint::Color::from_rgb_u8(255, 213, 181),
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
        assert!(
            ElementHandle::find_by_accessible_label(
                &app,
                &format!("Delete profile: {PROFILE_NAME}")
            )
            .next()
            .is_none()
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
    button(&app, "Add profile").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, slint::platform::Key::Back.into());
    assert!(app.get_profiles_open() && app.get_profiles_require_selection());
    button(&app, "Add profile").mock_single_click(slint::platform::PointerEventButton::Left);
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
    button(&app, "Manage profiles").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
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

#[test]
fn compact_rows_keep_letters_readable_and_management_nested() {
    if !isolation::run(concat!(
        module_path!(),
        "::compact_rows_keep_letters_readable_and_management_nested"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    app.set_profiles(
        Rc::new(slint::VecModel::from(vec![
            nova_ui::ProfileRow {
                id: "miso".into(),
                name: "Mišo".into(),
                initial: "M".into(),
                active: true,
                avatar_background: slint::Color::from_rgb_u8(60, 37, 75),
                avatar_foreground: slint::Color::from_rgb_u8(255, 213, 181),
            },
            nova_ui::ProfileRow {
                id: "family".into(),
                name: "Family".into(),
                initial: "F".into(),
                active: false,
                avatar_background: slint::Color::from_rgb_u8(236, 195, 213),
                avatar_foreground: slint::Color::from_rgb_u8(80, 37, 80),
            },
            nova_ui::ProfileRow {
                id: "guest".into(),
                name: "Guest".into(),
                initial: "G".into(),
                active: false,
                avatar_background: slint::Color::from_rgb_u8(37, 70, 51),
                avatar_foreground: slint::Color::from_rgb_u8(197, 239, 207),
            },
        ]))
        .into(),
    );
    for (width, tv) in [(320, false), (390, false), (1280, true)] {
        app.global::<nova_ui::InterfaceStyle>().set_tv(tv);
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        for name in ["Mišo", "Family", "Guest"] {
            let row = button(&app, &format!("Use profile: {name}"));
            let size = row.size();
            let position = row.absolute_position();
            assert!(
                (size.height - if tv { 104.0 } else { 84.0 }).abs() < 0.5,
                "compact row at {width}px: {size:?}"
            );
            assert!(position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5);
        }
        let letters = ElementHandle::find_by_element_id(&app, "ProfileAvatar::avatar_letter")
            .collect::<Vec<_>>();
        assert_eq!(letters.len(), 3);
        for letter in letters {
            assert_eq!(letter.size().width, letter.size().height);
            assert!(letter.size().width >= 48.0);
        }
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Rename profile: Mišo")
                .next()
                .is_none()
        );
        if !tv {
            let manage = button(&app, "Manage profiles");
            assert!(
                manage.absolute_position().y + manage.size().height <= 900.5,
                "phone footer should fit without scrolling: {:?} {:?}",
                manage.absolute_position(),
                manage.size()
            );
        }
    }
    app.global::<nova_ui::InterfaceStyle>().set_tv(false);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    settle(&app);
    button(&app, "Manage profiles").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Use profile: Mišo")
            .next()
            .is_none()
    );
    let renamed = Rc::new(RefCell::new(Vec::new()));
    let recorded = renamed.clone();
    app.on_profile_rename(move |id, name| {
        recorded
            .borrow_mut()
            .push((id.to_string(), name.to_string()))
    });
    button(&app, "Rename profile: Family")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, " room".into());
    let name_input = ElementHandle::find_by_accessible_label(&app, "Profile name")
        .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    assert_eq!(
        name_input.accessible_value().as_deref(),
        Some("Family room")
    );
    // Existing editor text is selected with Ctrl+A before entering the new name.
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Control.into(),
        });
    key(&app, "a".into());
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Control.into(),
        });
    key(&app, "Shared".into());
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(
        renamed.borrow().as_slice(),
        &[("family".to_string(), "Shared".to_string())]
    );
    button(&app, "Rename profile: Mišo")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, " 2".into());
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(
        renamed.borrow().last(),
        Some(&("miso".to_string(), "Mišo 2".to_string()))
    );
    // Each profile has Rename, Delete and PIN actions. The footer can sit
    // below a phone viewport; keyboard traversal must reveal it again.
    for _ in 0..10 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Done")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Back.into());
    assert!(app.get_profiles_require_selection() && app.get_profiles_open());
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Use profile: Mišo")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Back.into());
    assert!(app.get_profiles_require_selection() && app.get_profiles_open());
}

#[test]
fn keyboard_and_tv_navigation_reveal_picker_actions_and_skip_last_delete() {
    if !isolation::run(concat!(
        module_path!(),
        "::keyboard_and_tv_navigation_reveal_picker_actions_and_skip_last_delete"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    app.window().set_size(slint::PhysicalSize::new(320, 360));
    app.set_is_android(true);
    app.global::<nova_ui::InterfaceStyle>().set_tv(true);
    settle(&app);
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    app.on_profile_switch(move |id| recorded.borrow_mut().push(id.to_string()));
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(selected.borrow().as_slice(), &["stable-profile"]);
    app.set_profiles_busy(true);
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(selected.borrow().len(), 1);
    app.set_profiles_busy(false);
    settle(&app);
    for _ in 0..3 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let manage = button(&app, "Manage profiles");
    assert!(manage.absolute_position().y >= 0.0);
    assert!(
        manage.absolute_position().y + manage.size().height <= 360.5,
        "focused action should fit: position={:?}, size={:?}, body={:?}, viewport={:?}",
        manage.absolute_position(),
        manage.size(),
        ElementHandle::find_by_element_id(&app, "ProfilesPage::profile_body")
            .next()
            .map(|e| (e.absolute_position(), e.size())),
        ElementHandle::find_by_element_id(&app, "ProfilesPage::profile_scroll")
            .next()
            .map(|e| (e.absolute_position(), e.size()))
    );
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(
        button(&app, &format!("Delete profile: {PROFILE_NAME}")).accessible_enabled(),
        Some(false)
    );
    // Rename -> PIN -> Add skips the disabled last-profile Delete action.
    let policy_edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = policy_edits.clone();
    app.global::<nova_ui::MoreBridge>()
        .on_policy_edit(move |id| recorded.borrow_mut().push(id.to_string()));
    key(&app, slint::platform::Key::DownArrow.into());
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(policy_edits.borrow().as_slice(), &["stable-profile"]);
    key(&app, slint::platform::Key::DownArrow.into());
    key(&app, slint::platform::Key::Return.into());
    key(&app, "New profile".into());
    let created = Rc::new(RefCell::new(Vec::new()));
    let recorded = created.clone();
    app.on_profile_create(move |name| recorded.borrow_mut().push(name.to_string()));
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(created.borrow().as_slice(), &["New profile"]);
}

#[test]
fn profile_picker_animates_entry_return_and_motion_disable() {
    if !isolation::run(concat!(
        module_path!(),
        "::profile_picker_animates_entry_return_and_motion_disable"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        for width in [390, 1280] {
            let app = picker(false);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.global::<nova_ui::Anim>().set_enabled(false);
            app.set_profiles_open(false);
            settle(&app);
            app.global::<nova_ui::Anim>().set_enabled(enabled);
            app.global::<nova_ui::Anim>().set_transitions(transitions);
            let landing = ElementHandle::find_by_element_id(&app, "MorePage::landing")
                .next()
                .unwrap();
            let origin = ElementHandle::find_by_element_type_name(&app, "MorePage")
                .next()
                .unwrap()
                .absolute_position()
                .x;
            assert!(
                landing.computed_opacity() > 0.99,
                "More landing at {width}px, motion {enabled}/{transitions}: opacity {}, position {:?}, profiles {}, bridge {}, required {}, more {}",
                landing.computed_opacity(),
                landing.absolute_position(),
                app.get_profiles_open(),
                app.global::<nova_ui::MoreBridge>().get_profiles_open(),
                app.get_profiles_require_selection(),
                app.get_more_open()
            );
            button(&app, "Profiles").mock_single_click(slint::platform::PointerEventButton::Left);
            let page = ElementHandle::find_by_element_type_name(&app, "ProfilesPage")
                .next()
                .unwrap();
            if enabled && transitions {
                assert!(page.absolute_position().x > width as f32 * 0.8);
                assert!(page.computed_opacity() < 0.1);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
                assert!(page.computed_opacity() > 0.1 && page.computed_opacity() < 0.99);
                // Back can reverse an incomplete entry without changing routes.
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: slint::platform::Key::Back.into(),
                    });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                        text: slint::platform::Key::Back.into(),
                    });
                assert!(!app.get_profiles_open() && app.get_more_open());
                settle(&app);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
                assert!(landing.computed_opacity() > 0.99);
                button(&app, "Profiles")
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                let _ = (page.absolute_position(), page.computed_opacity());
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
                app.global::<nova_ui::Anim>().set_transitions(false);
            }
            assert!(
                (page.absolute_position().x - origin).abs() < 0.5,
                "profile endpoint at {width}px: {:?}, origin {origin}",
                page.absolute_position()
            );
            assert!(page.computed_opacity() > 0.99);
            if enabled && transitions {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
                app.global::<nova_ui::Anim>().set_transitions(true);
                assert!((page.absolute_position().x - origin).abs() < 0.5);
                assert!(page.computed_opacity() > 0.99);
            }
            key(&app, slint::platform::Key::Back.into());
            assert!(app.get_more_open() && !app.get_profiles_open());
        }
    }
}

#[test]
fn selecting_profiles_reveals_home_and_cancels_interrupted_motion() {
    if !isolation::run(concat!(
        module_path!(),
        "::selecting_profiles_reveals_home_and_cancels_interrupted_motion"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        for required in [true, false] {
            let app = picker(required);
            app.global::<nova_ui::Anim>().set_enabled(enabled);
            app.global::<nova_ui::Anim>().set_transitions(transitions);
            let weak = app.as_weak();
            app.on_profile_switch(move |_| {
                let app = weak.upgrade().unwrap();
                app.set_profiles_require_selection(false);
                app.set_profiles_open(false);
                app.set_show_home(true);
                app.set_more_open(false);
                app.invoke_animate_profile_home_entry();
            });
            button(&app, &format!("Use profile: {PROFILE_NAME}"))
                .mock_single_click(slint::platform::PointerEventButton::Left);
            let home = ElementHandle::find_by_element_type_name(&app, "HomeHost")
                .next()
                .unwrap();
            if enabled && transitions {
                assert!(home.absolute_position().y > 23.0 && home.computed_opacity() < 0.01);
                // The first draw may occur after the deferred entry tick.
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2));
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
                assert!(home.absolute_position().y > 0.1 && home.absolute_position().y < 23.0);
                assert!(home.computed_opacity() > 0.01 && home.computed_opacity() < 0.99);
                app.global::<nova_ui::Anim>().set_enabled(false);
                assert!(home.absolute_position().y.abs() < 0.1 && home.computed_opacity() > 0.99);
                app.global::<nova_ui::Anim>().set_enabled(true);
                app.invoke_animate_profile_home_entry();
                let _ = home.computed_opacity();
                app.set_more_open(true);
                settle(&app);
                app.set_more_open(false);
            }
            settle(&app);
            let home = ElementHandle::find_by_element_type_name(&app, "HomeHost")
                .next()
                .unwrap();
            assert!(home.absolute_position().y.abs() < 0.1 && home.computed_opacity() > 0.99);
            assert!(app.get_show_home() && !app.get_profiles_require_selection());
        }
    }
}

#[test]
fn pin_keypad_tv_digits_back_and_phone_safe_bounds() {
    if !isolation::run(concat!(
        module_path!(),
        "::pin_keypad_tv_digits_back_and_phone_safe_bounds"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    let ui = app.global::<nova_ui::MoreBridge>();
    let digits = Rc::new(RefCell::new(Vec::new()));
    let recorded = digits.clone();
    ui.on_pin_input_edit(move |digit| recorded.borrow_mut().push(digit));
    let cancelled = Rc::new(RefCell::new(0));
    let count = cancelled.clone();
    let weak = app.as_weak();
    ui.on_pin_cancel(move || {
        *count.borrow_mut() += 1;
        if let Some(app) = weak.upgrade() {
            app.global::<nova_ui::MoreBridge>().set_pin_open(false);
        }
    });
    for (width, height, tv) in [(320, 568, false), (1280, 720, true)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.global::<nova_ui::InterfaceStyle>().set_tv(tv);
        // LogicalEdges::new takes top/bottom/left/right. Named fields keep
        // this asymmetric cutout fixture and its geometric expectations clear.
        let safe_area = i_slint_core::lengths::LogicalEdges {
            top: 28.0,
            bottom: 12.0,
            left: 8.0,
            right: 16.0,
        };
        i_slint_core::window::WindowInner::from_pub(app.window())
            .set_window_item_safe_area(safe_area);
        ui.set_pin_open(true);
        settle(&app);
        let first = button(&app, "1");
        let first_x = first.absolute_position().x;
        let cell_width = first.size().width;
        assert!(cell_width >= 44.0 && first.size().height >= 44.0);
        for (column, label) in ["1", "2", "3"].into_iter().enumerate() {
            let cell = button(&app, label);
            assert!((cell.size().width - cell_width).abs() < 0.5);
            assert!(
                (cell.absolute_position().x - first_x - column as f32 * (cell_width + 12.0)).abs()
                    < 0.5
            );
        }
        let grid_center = first_x + (cell_width * 3.0 + 24.0) / 2.0;
        let safe_center = safe_area.left + (width as f32 - safe_area.left - safe_area.right) / 2.0;
        assert!(
            (grid_center - safe_center).abs() < 0.5,
            "keypad center {grid_center} must match safe center {safe_center}; first_x={first_x}, cell_width={cell_width}, safe_area={safe_area:?}"
        );
        // A late page mount must return navigation to the topmost PIN layer.
        app.invoke_preserve_back_focus();
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(
            digits.borrow().last(),
            Some(&1),
            "D-pad starts on first digit"
        );
        key(&app, slint::platform::Key::RightArrow.into());
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(digits.borrow().last(), Some(&2));
        for _ in 0..4 {
            key(&app, slint::platform::Key::DownArrow.into());
        }
        // An empty PIN disables Unlock; traverse past it in both directions.
        key(&app, slint::platform::Key::LeftArrow.into());
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(digits.borrow().last(), Some(&-2));
        key(&app, slint::platform::Key::RightArrow.into());
        for (column, label) in ["Clear", "0", "Delete"].into_iter().enumerate() {
            let cell = button(&app, label);
            assert!((cell.size().width - cell_width).abs() < 0.5);
            assert!(
                (cell.absolute_position().x - first_x - column as f32 * (cell_width + 12.0)).abs()
                    < 0.5
            );
        }
        let cancel = button(&app, "Cancel");
        let p = cancel.absolute_position();
        let size = cancel.size();
        assert!(p.x >= safe_area.left && p.x + size.width <= width as f32 - safe_area.right + 0.5);
        assert!(
            p.y >= safe_area.top && p.y + size.height <= height as f32 - safe_area.bottom + 0.5
        );
        key(&app, slint::platform::Key::Escape.into());
        assert!(!ui.get_pin_open());
        assert!(app.get_profiles_require_selection());
    }
    assert_eq!(*cancelled.borrow(), 2);
}

#[test]
fn policy_editor_wraps_focus_back_to_pin_and_reveals_add_title() {
    if !isolation::run(concat!(
        module_path!(),
        "::policy_editor_wraps_focus_back_to_pin_and_reveals_add_title"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(false);
    app.window().set_size(slint::PhysicalSize::new(320, 568));
    app.global::<nova_ui::InterfaceStyle>().set_tv(true);
    let ui = app.global::<nova_ui::MoreBridge>();
    ui.set_policy_titles(
        Rc::new(slint::VecModel::from(
            (0..12)
                .map(|i| format!("Long library title {i}").into())
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    ui.set_policy_open(true);
    settle(&app);
    // Up wraps from the PIN field to Cancel at the bottom of the long form.
    key(&app, slint::platform::Key::UpArrow.into());
    let cancel = button(&app, "Cancel");
    assert!(cancel.absolute_position().y >= 0.0);
    assert!(cancel.absolute_position().y + cancel.size().height <= 568.5);
    key(&app, slint::platform::Key::DownArrow.into());
    let input = ElementHandle::find_by_accessible_label(&app, "Profile PIN")
        .find(|e| e.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    assert!(input.absolute_position().y >= 0.0);
    assert!(input.absolute_position().y + input.size().height <= 568.5);
    for _ in 0..3 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let add = button(&app, "Add title");
    assert!(add.absolute_position().y >= 0.0);
    assert!(add.absolute_position().y + add.size().height <= 568.5);
}

#[test]
fn profile_selection_indicator_crossfades_and_master_off_is_instant() {
    if !isolation::run(concat!(
        module_path!(),
        "::profile_selection_indicator_crossfades_and_master_off_is_instant"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = picker(true);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(false);
    app.global::<nova::Anim>().set_hover(false);
    let check = || {
        ElementHandle::find_by_element_id(&app, "ProfileChoiceRow::selection_check")
            .next()
            .unwrap()
    };
    // Exercise property tracking once per simulated render frame, including
    // the changed callbacks that drive the retained animated intermediates.
    let advance = |milliseconds| {
        for _ in 0..milliseconds {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
            let current = check();
            let _ = (
                current.computed_opacity(),
                current.absolute_position(),
                current.size(),
            );
        }
    };
    let geometry = (check().absolute_position(), check().size());
    assert!(
        check().computed_opacity() > 0.99,
        "expected selected indicator, opacity={}",
        check().computed_opacity()
    );
    let profiles = app.get_profiles();
    let mut row = profiles.row_data(0).unwrap();
    row.active = false;
    profiles.set_row_data(0, row.clone());
    let _ = check().computed_opacity();
    advance(70);
    let middle = check().computed_opacity();
    assert!(
        middle > 0.05 && middle < 0.95,
        "profile check crossfades while page transitions and hover are off: {middle}"
    );
    advance(120);
    assert!(
        check().computed_opacity() < 0.01,
        "expected hidden indicator, opacity={}",
        check().computed_opacity()
    );
    assert_eq!((check().absolute_position(), check().size()), geometry);
    app.global::<nova::Anim>().set_enabled(false);
    row.active = true;
    profiles.set_row_data(0, row);
    assert!(
        check().computed_opacity() > 0.99,
        "master-off selection is instant; opacity={}",
        check().computed_opacity()
    );
    app.global::<nova::Anim>().set_enabled(true);
    let _ = check().computed_opacity();
    advance(170);
    assert!(
        check().computed_opacity() > 0.99,
        "expected selected indicator, opacity={}",
        check().computed_opacity()
    );
    let mut row = profiles.row_data(0).unwrap();
    row.active = false;
    profiles.set_row_data(0, row);
    let _ = check().computed_opacity();
    advance(35);
    let moving = check().computed_opacity();
    assert!(
        moving > 0.01 && moving < 0.99,
        "expected in-flight indicator, opacity={moving}"
    );
    app.global::<nova::Anim>().set_enabled(false);
    advance(16);
    assert!(
        check().computed_opacity() < 0.01,
        "disabling motion snaps an in-flight profile check on the next frame; opacity={}",
        check().computed_opacity()
    );
    app.global::<nova::Anim>().set_enabled(true);
    assert!(
        check().computed_opacity() < 0.01,
        "quick re-enable keeps the snapped visual state; opacity={}",
        check().computed_opacity()
    );
    advance(16);
    assert!(
        check().computed_opacity() < 0.01,
        "quick re-enable must not resume stale interpolation; opacity={}",
        check().computed_opacity()
    );
}

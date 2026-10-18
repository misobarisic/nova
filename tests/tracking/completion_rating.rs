//! Completion ratings keep independent tracker drafts, respect active layers,
//! and leave the action bar reachable on compact phones and short landscapes.

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, Model};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
}

fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

fn click(app: &nova_ui::AppWindow, label: &str) {
    button(app, label).mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
}

fn destination(id: &str, max: f32, step: f32) -> nova_ui::CompletionDestination {
    nova_ui::CompletionDestination {
        id: id.into(),
        label: id.into(),
        max,
        step,
        ..Default::default()
    }
}

fn prompt() -> nova_ui::AppWindow {
    let app = nova_ui::AppWindow::new().unwrap();
    app.global::<nova_ui::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(580, 900));
    app.set_completion_prompt_token("draft-a".into());
    app.set_completion_prompt_title("A completed release".into());
    app.set_completion_prompt_subtitle("Season 1 · Episode 12".into());
    app.set_completion_prompt_open(true);
    app.window().show().unwrap();
    settle();
    app
}

#[test]
fn selection_routes_each_scale_and_save_requires_a_draft() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::selection_routes_each_scale_and_save_requires_a_draft"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = prompt();
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.on_completion_score_changed(move |token, id, score| {
        recorded
            .borrow_mut()
            .push(format!("score:{token}:{id}:{score}"));
    });
    let recorded = events.clone();
    app.on_completion_save(move |token| recorded.borrow_mut().push(format!("save:{token}")));
    assert_eq!(
        button(&app, "Save ratings").accessible_enabled(),
        Some(false)
    );
    for max in [3.0, 5.0, 10.0] {
        app.set_completion_prompt_open(false);
        settle();
        app.set_completion_prompt_destinations(
            Rc::new(slint::VecModel::from(vec![destination(
                "AniList", max, 1.0,
            )]))
            .into(),
        );
        app.set_completion_prompt_open(true);
        settle();
        assert_eq!(
            button(&app, "Save ratings").accessible_enabled(),
            Some(false)
        );
        assert_eq!(button(&app, "AniList 1").accessible_checked(), Some(false));
        click(&app, &format!("AniList {}", max as i32));
        assert_eq!(
            events.borrow().last().unwrap(),
            &format!("score:draft-a:AniList:{}", max as i32)
        );
    }
    for (max, step, expected) in [(10.0, 0.1, "0.1"), (100.0, 1.0, "1")] {
        app.set_completion_prompt_open(false);
        settle();
        let model = Rc::new(slint::VecModel::from(vec![destination(
            "AniList", max, step,
        )]));
        app.set_completion_prompt_destinations(model.clone().into());
        app.set_completion_prompt_open(true);
        settle();
        click(&app, "Increase rating · AniList");
        assert_eq!(
            events.borrow().last().unwrap(),
            &format!("score:draft-a:AniList:{expected}")
        );
        click(&app, "Increase rating · AniList");
        assert_eq!(
            events.borrow().last().unwrap(),
            if step < 1.0 {
                "score:draft-a:AniList:0.2"
            } else {
                "score:draft-a:AniList:2"
            }
        );
        let input = ElementHandle::find_by_accessible_label(&app, "Rating · AniList")
            .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
            .expect("numeric rating editor");
        input.set_accessible_value(if step < 1.0 { "8.2" } else { "82" });
        assert_eq!(
            events.borrow().last().unwrap(),
            if step < 1.0 {
                "score:draft-a:AniList:8.2"
            } else {
                "score:draft-a:AniList:82"
            }
        );
        let mut row = model.row_data(0).unwrap();
        row.score = if step < 1.0 { "7.5" } else { "75" }.into();
        row.selected = true;
        model.set_row_data(0, row);
        app.set_completion_prompt_can_save(true);
        settle();
        click(&app, "Decrease rating · AniList");
        // An older actor acknowledgement must not replace the typed value.
        let expected = if step < 1.0 { "8.1" } else { "81" };
        assert_eq!(
            events.borrow().last().unwrap(),
            &format!("score:draft-a:AniList:{expected}")
        );
        click(&app, "Save ratings");
        assert_eq!(events.borrow().last().unwrap(), "save:draft-a");
        app.set_completion_prompt_can_save(false);
    }
}

#[test]
fn responsive_translations_keep_the_footer_inside_the_window() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::responsive_translations_keep_the_footer_inside_the_window"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = prompt();
    app.set_completion_prompt_title(
        "A completed release with a long title that wraps on phones".into(),
    );
    app.set_completion_prompt_error(
        "The rating could not be saved. Please try again. Your selected scores are still here."
            .into(),
    );
    app.set_completion_prompt_destinations(
        Rc::new(slint::VecModel::from(vec![
            destination("MyAnimeList", 10.0, 1.0),
            destination("AniList", 100.0, 1.0),
        ]))
        .into(),
    );
    for (language, heading, dismiss, save) in [
        ("en", "Finished watching", "Dismiss", "Save ratings"),
        ("hr", "Gledanje završeno", "Zatvori", "Spremi ocjene"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height, android) in [(320, 720, true), (840, 360, true), (1280, 800, false)] {
            app.set_is_android(android);
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            let (top, bottom, left, right) = if android {
                (24.0, 16.0, 32.0, 20.0)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
                i_slint_core::lengths::LogicalEdges::new(top, bottom, left, right),
            );
            settle();
            for label in [dismiss, save] {
                let action = button(&app, label);
                let position = action.absolute_position();
                let size = action.size();
                assert!(
                    position.x >= left && position.x + size.width <= width as f32 - right + 0.5,
                    "{language} {width}×{height}: {label} overflows horizontally"
                );
                assert!(
                    position.y >= top && position.y + size.height <= height as f32 - bottom + 0.5,
                    "{language} {width}×{height}: {label} overflows vertically"
                );
                assert!(
                    size.height >= 44.0 && size.width >= 44.0,
                    "footer lost its touch target"
                );
            }
            // A short landscape viewport scrolls its content independently of
            // the pinned footer; the heading is reachable at the top.
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(width as f32 / 2.0, height as f32 / 2.0),
                    delta_x: 0.0,
                    delta_y: 10000.0,
                });
            settle();
            assert!(
                ElementHandle::find_by_accessible_label(&app, heading)
                    .next()
                    .is_some()
            );
            assert_eq!(button(&app, save).accessible_enabled(), Some(false));
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(width as f32 / 2.0, height as f32 / 2.0),
                    delta_x: 0.0,
                    delta_y: -10000.0,
                });
            settle();
            let error = ElementHandle::find_by_accessible_label(&app, "The rating could not be saved. Please try again. Your selected scores are still here.").next().expect("retry error reachable in scroll body");
            assert!(error.size().width > 0.0 && error.size().height > 0.0);
            assert!(
                error.absolute_position().x >= left
                    && error.absolute_position().x + error.size().width
                        <= width as f32 - right + 0.5
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn dismissal_and_layer_gating_preserve_the_pending_prompt() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::dismissal_and_layer_gating_preserve_the_pending_prompt"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = prompt();
    let dismissed = Rc::new(RefCell::new(Vec::new()));
    let recorded = dismissed.clone();
    let weak = app.as_weak();
    app.on_completion_dismiss(move |token| {
        recorded.borrow_mut().push(token.to_string());
        weak.upgrade().unwrap().set_completion_prompt_open(false);
    });
    app.set_completion_rating_prompts_enabled(false);
    settle();
    assert!(!app.get_completion_prompt_visible());
    assert!(
        app.get_completion_prompt_open(),
        "opt-out hides the pending prompt immediately without consuming it in the UI"
    );
    app.set_completion_rating_prompts_enabled(true);
    settle();
    app.set_modal_visible(true);
    settle();
    assert!(
        app.get_completion_prompt_visible(),
        "Detail is an eligible normal route"
    );
    app.set_player_open(true);
    settle();
    assert!(!app.get_completion_prompt_visible());
    assert!(
        app.get_completion_prompt_open(),
        "playback preserves the draft"
    );
    app.set_player_open(false);
    app.set_stream_selector_open(true);
    settle();
    assert!(!app.get_completion_prompt_visible());
    app.set_stream_selector_open(false);
    settle();
    assert!(app.get_completion_prompt_visible());
    app.set_show_settings(true);
    settle();
    assert!(
        !app.get_completion_prompt_visible(),
        "Settings tasks keep ownership"
    );
    app.set_show_settings(false);
    app.global::<nova_ui::SettingsSync>().set_open(true);
    settle();
    assert!(
        !app.get_completion_prompt_visible(),
        "sync comparison owns its modal"
    );
    app.global::<nova_ui::SettingsSync>().set_open(false);
    settle();
    for key in [slint::platform::Key::Escape, slint::platform::Key::Back] {
        app.set_completion_prompt_open(true);
        settle();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
        assert!(!app.get_completion_prompt_open());
    }
    app.set_completion_prompt_open(true);
    settle();
    click(&app, "Dismiss");
    app.set_completion_prompt_open(true);
    settle();
    click(&app, "Close rating prompt");
    app.set_completion_prompt_open(true);
    settle();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(1.0, 1.0),
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(1.0, 1.0),
            button: slint::platform::PointerEventButton::Left,
        });
    assert!(!app.get_completion_prompt_open());
    assert_eq!(
        &*dismissed.borrow(),
        &["draft-a", "draft-a", "draft-a", "draft-a", "draft-a"]
    );
    app.set_tracking_library_prompt_open(true);
    app.set_completion_prompt_open(true);
    settle();
    assert!(app.get_completion_prompt_visible());
    assert!(!app.get_tracking_library_prompt_visible());
    click(&app, "Dismiss");
    settle();
    assert!(app.get_tracking_library_prompt_visible());
    assert!(!app.get_completion_prompt_visible());
}

fn native_focus(app: &nova_ui::AppWindow) -> String {
    i_slint_core::window::WindowInner::from_pub(app.window())
        .focus_item
        .borrow()
        .upgrade()
        .map(|item| format!("{:?}", item.element_type_names_and_ids(0)))
        .unwrap_or_else(|| "none".into())
}

fn key(app: &nova_ui::AppWindow, text: impl Into<slint::SharedString>) {
    let text = text.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle();
}

#[test]
fn keyboard_navigation_stays_in_dialog_and_numeric_text_survives_old_acknowledgements() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::keyboard_navigation_stays_in_dialog_and_numeric_text_survives_old_acknowledgements"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = prompt();
    let model = Rc::new(slint::VecModel::from(vec![destination(
        "AniList", 3.0, 1.0,
    )]));
    app.set_completion_prompt_destinations(model.clone().into());
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.on_completion_score_changed(move |_, _, score| {
        recorded.borrow_mut().push(score.to_string())
    });
    let saves = Rc::new(RefCell::new(0));
    let recorded = saves.clone();
    app.on_completion_save(move |_| *recorded.borrow_mut() += 1);
    settle();
    // Initial focus is the non-input close button. Native Tab and Shift+Tab
    // move through the score pills and wrap without entering the covered page.
    key(&app, slint::platform::Key::Tab);
    key(&app, slint::platform::Key::Return);
    assert_eq!(
        events.borrow().last().map(String::as_str),
        Some("1"),
        "Tab then Enter: {}",
        native_focus(&app)
    );
    key(&app, slint::platform::Key::Tab);
    key(&app, slint::platform::Key::Return);
    assert_eq!(events.borrow().last().map(String::as_str), Some("2"));
    key(&app, "\u{19}"); // Shift+Tab (Backtab).
    key(&app, slint::platform::Key::Return);
    assert_eq!(events.borrow().last().map(String::as_str), Some("1"));
    for _ in 0..8 {
        key(&app, slint::platform::Key::Tab);
        key(&app, slint::platform::Key::Return);
    }
    assert_eq!(
        *saves.borrow(),
        0,
        "disabled Save cannot activate with Enter"
    );

    app.set_completion_prompt_open(false);
    settle();
    model.set_row_data(0, destination("AniList", 10.0, 0.1));
    app.set_completion_prompt_open(true);
    settle();
    let input = ElementHandle::find_by_accessible_label(&app, "Rating · AniList")
        .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    input.mock_single_click(slint::platform::PointerEventButton::Left);
    key(&app, "8");
    let mut old_ack = model.row_data(0).unwrap();
    old_ack.score = "1".into();
    old_ack.selected = true;
    model.set_row_data(0, old_ack);
    settle();
    key(&app, ".");
    key(&app, "2");
    assert_eq!(input.accessible_value().as_deref(), Some("8.2"));
    assert_eq!(events.borrow().last().map(String::as_str), Some("8.2"));
    key(&app, slint::platform::Key::Tab);
    key(&app, slint::platform::Key::Return);
    assert_eq!(events.borrow().last().map(String::as_str), Some("8.3"));
    input.set_accessible_value("8,2");
    assert_eq!(events.borrow().last().map(String::as_str), Some("8,2"));
    // The Croatian decimal separator also steps from the immediate local
    // draft, without waiting for a normalized actor acknowledgement.
    click(&app, "Increase rating · AniList");
    assert_eq!(events.borrow().last().map(String::as_str), Some("8.3"));

    let weak = app.as_weak();
    app.on_completion_dismiss(move |_| weak.upgrade().unwrap().set_completion_prompt_open(false));
    let discovered = Rc::new(RefCell::new(false));
    let recorded = discovered.clone();
    app.on_discover_picked(move || *recorded.borrow_mut() = true);
    app.set_home_kb_zone(0);
    app.set_home_kb_nav(0);
    key(&app, slint::platform::Key::Escape);
    assert!(!app.get_completion_prompt_open());
    // Dismissal restores the page scope after the temporary window scope has
    // preserved release/repeat handling; arrows and Enter work immediately.
    key(&app, slint::platform::Key::RightArrow);
    assert_eq!(
        app.get_home_kb_nav(),
        1,
        "page keys after dismiss: {}",
        native_focus(&app)
    );
    key(&app, slint::platform::Key::Return);
    assert!(
        *discovered.borrow(),
        "Home's keyboard navigation opens Discover after dismiss"
    );
}

#[test]
fn entry_motion_fades_and_rises_only_when_transitions_are_enabled() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::entry_motion_fades_and_rises_only_when_transitions_are_enabled"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().show().unwrap();
    for (phone, enabled) in [(true, true), (false, true), (true, false)] {
        app.set_completion_prompt_open(false);
        settle();
        app.set_is_android(phone);
        app.window().set_size(if phone {
            slint::PhysicalSize::new(360, 720)
        } else {
            slint::PhysicalSize::new(1280, 800)
        });
        app.global::<nova_ui::Anim>().set_enabled(enabled);
        app.set_completion_prompt_open(true);
        let overlay = || {
            ElementHandle::find_by_element_type_name(&app, "CompletionRatingPrompt")
                .next()
                .unwrap()
        };
        let panel = || {
            ElementHandle::find_by_element_id(&app, "CompletionRatingPrompt::completion_panel")
                .next()
                .unwrap()
        };
        let start_y = panel().absolute_position().y;
        if enabled {
            assert!(
                overlay().computed_opacity() < 0.01,
                "phone={phone}: initial opacity {}",
                overlay().computed_opacity()
            );
        } else {
            assert!(overlay().computed_opacity() > 0.99);
        }
        let mut faded = false;
        let mut rose = false;
        for _ in 0..35 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            let alpha = overlay().computed_opacity();
            faded |= alpha > 0.01 && alpha < 0.99;
            rose |= panel().absolute_position().y < start_y - 0.1;
        }
        assert!(overlay().computed_opacity() > 0.99);
        assert_eq!(faded, enabled);
        assert_eq!(rose, enabled && phone);
    }
}

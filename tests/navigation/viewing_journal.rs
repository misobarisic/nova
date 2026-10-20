//! The More journal keeps recap actions reachable and consumes Back locally.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_ui_shared::JournalBridge;
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova_ui::AppWindow) {
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

fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing journal button: {label}"))
}

#[test]
fn compact_year_actions_route_selected_year_and_keep_touch_targets_visible() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::compact_year_actions_route_selected_year_and_keep_touch_targets_visible"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let journal = app.global::<JournalBridge>();
    journal.set_current_year(2026);
    journal.set_oldest_year(2024);
    journal.set_year(0);
    journal.set_watched("0:00:00".into());
    journal.set_open(true);
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    let weak = app.as_weak();
    journal.on_select_year(move |year| {
        recorded.borrow_mut().push(year);
        if let Some(app) = weak.upgrade() {
            app.global::<JournalBridge>().set_year(year);
        }
    });
    app.window().show().unwrap();
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        journal.set_year(0);
        settle(&app);
        for label in ["All time", "Yearly recap"] {
            let control = button(&app, label);
            let position = control.absolute_position();
            assert!(position.x >= 0.0 && position.x + control.size().width <= width as f32 + 0.5);
            assert!(
                control.size().height >= 44.0,
                "journal controls retain a touch target"
            );
        }
        button(&app, "Yearly recap").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(journal.get_year(), 2026);
        assert_eq!(button(&app, "Next year").accessible_enabled(), Some(false));
        button(&app, "Previous year").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(journal.get_year(), 2025);
        button(&app, "All time").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(journal.get_year(), 0);
    }
    assert_eq!(
        selected.borrow().as_slice(),
        &[2026, 2025, 0, 2026, 2025, 0, 2026, 2025, 0]
    );
}

#[test]
fn back_closes_journal_and_returns_to_more_without_navigating_home() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::back_closes_journal_and_returns_to_more_without_navigating_home"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    app.global::<JournalBridge>().set_open(true);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    settle(&app);
    let text: slint::SharedString = slint::platform::Key::Escape.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(&app);
    assert!(!app.global::<JournalBridge>().get_open());
    assert!(app.get_more_open());
    assert!(!app.get_show_home());
}

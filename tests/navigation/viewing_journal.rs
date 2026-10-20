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

#[test]
fn dated_cards_contain_every_text_row_and_large_clocks_fit_compact_stats() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::dated_cards_contain_every_text_row_and_large_clocks_fit_compact_stats"
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
    journal.set_watched("123456:12:34".into());
    journal.set_session_count(18);
    journal.set_titles(
        Rc::new(slint::VecModel::from(vec![
            nova_ui_shared::JournalTitleRow {
                title: "A longer title with dates and a completed rewatch".into(),
                first_date: "2025-10-20".into(),
                finished_date: "2026-10-20".into(),
                watched: "12:34:56".into(),
                sessions: 18,
                rewatches: 2,
                ..Default::default()
            },
        ]))
        .into(),
    );
    journal.set_sessions(
        Rc::new(slint::VecModel::from(vec![
            nova_ui_shared::JournalSessionRow {
                id: "session".into(),
                title: "A longer episode title".into(),
                episode: "S2 E4 · The other side of midnight".into(),
                date: "2026-10-20 21:34 UTC".into(),
                watched: "0:43:20".into(),
                completed: true,
                rewatch: true,
            },
        ]))
        .into(),
    );
    journal.set_open(true);
    app.show().unwrap();
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1800));
        settle(&app);
        for kind in ["JournalTitle", "JournalSession"] {
            let cards = ElementHandle::find_by_element_type_name(&app, kind).collect::<Vec<_>>();
            assert_eq!(
                cards.len(),
                1,
                "dated fixture card must be measured at {width}px"
            );
            for card in cards {
                let origin = card.absolute_position();
                let size = card.size();
                let text_rows = card.query_descendants().match_type_name("Text").find_all();
                assert!(text_rows.len() >= 4);
                for text in text_rows {
                    let position = text.absolute_position();
                    let text_size = text.size();
                    assert!(
                        position.x >= origin.x + 13.0
                            && position.x + text_size.width <= origin.x + size.width - 13.0,
                        "{kind} text stays inside horizontal padding at {width}px: {position:?} {text_size:?}"
                    );
                    assert!(
                        position.y >= origin.y + 13.0
                            && position.y + text_size.height <= origin.y + size.height - 13.0,
                        "{kind} text stays inside vertical padding at {width}px: {position:?} {text_size:?}, card {origin:?} {size:?}"
                    );
                }
            }
        }
        let clock = ElementHandle::find_by_element_id(&app, "JournalStat::stat_value")
            .find(|element| element.accessible_label().as_deref() == Some("123456:12:34"))
            .unwrap();
        assert!(clock.size().width > 0.0);
        assert!(clock.absolute_position().x + clock.size().width <= width as f32 - 18.0);
        assert_eq!(clock.accessible_label().as_deref(), Some("123456:12:34"));
    }
}

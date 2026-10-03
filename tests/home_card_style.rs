//! Reference-style Home cards preserve their original dimensions and gestures.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn elements(app: &nova::AppWindow, kind: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, kind).collect()
}

fn child(card: &ElementHandle, id: &str) -> ElementHandle {
    card.query_descendants().match_id(id).find_first().unwrap()
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let p = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0)
}

fn tap(app: &nova::AppWindow, element: &ElementHandle) {
    let position = center(element);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(150);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(100);
}

#[test]
fn home_card_geometry_menus_and_menu_target_flick_remain_usable() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.set_touch_menus(true);
    app.set_show_home(true);
    app.set_home_continue(
        Rc::new(VecModel::from(
            (0..7)
                .map(|i| nova::ContinueRow {
                    id: format!("continue-{i}").into(),
                    title: "KonoSuba".into(),
                    episode_title: "A Friend for This Crimson Demon Girl! With a Very Long Title"
                        .into(),
                    ep_no: format!("S2 E{}", i + 1).into(),
                    remaining: "23 min left".into(),
                    progress: if i == 1 { 0.0 } else { 0.85 },
                    badge: if i == 1 { 1 } else { 0 },
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_home_upcoming(
        Rc::new(VecModel::from(
            (0..7)
                .map(|i| nova::UpcomingRow {
                    id: format!("upcoming-{i}").into(),
                    title: "KonoSuba".into(),
                    episode_title: "A Betrothed for This Noble Daughter! With a Very Long Title"
                        .into(),
                    ep_no: format!("S2 E{}", i + 4).into(),
                    date: "May 3, 2017".into(),
                    index: i,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.window().show().unwrap();

    for width in [320, 360, 390, 480, 620] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        idle(400);
        // These dimensions are from the pre-restyle cards: 18px page insets,
        // 18px row gaps and the original 2.5-stride card-width calculation.
        let viewport = width as f32 - 36.0;
        let expected_width = ((viewport + 18.0) / 2.5 - 18.0).max(96.0);
        for (kind, footer, title, status) in [
            ("ContinueCard", 76.0, "continue_title", "continue_status"),
            ("UpcomingCard", 88.0, "upcoming_title", "upcoming_date"),
        ] {
            let cards = elements(&app, kind);
            assert!(cards.len() >= 2);
            for card in &cards {
                assert!((card.size().width - expected_width).abs() < 0.5);
                assert!((card.size().height - (expected_width * 1.5 + footer)).abs() < 0.5);
                let title = child(card, &format!("{kind}::{title}"));
                let status = child(card, &format!("{kind}::{status}"));
                assert!(
                    title.absolute_position().y + title.size().height
                        <= status.absolute_position().y + 0.5
                );
                assert!(
                    status.absolute_position().y + status.size().height
                        <= card.absolute_position().y + card.size().height
                );
            }
            let stride = cards[1].absolute_position().x - cards[0].absolute_position().x;
            assert!(
                (stride - expected_width - 18.0).abs() < 0.5,
                "preserve row spacing"
            );
            let visible = 2.0 + (viewport - 2.0 * stride) / expected_width;
            assert!(
                (2.35..2.5).contains(&visible),
                "preserve roughly 2.45 visible cards"
            );
        }
        let cards = elements(&app, "ContinueCard");
        assert!(
            cards[0]
                .query_descendants()
                .match_id("ContinueCard::continue_progress")
                .find_first()
                .is_some()
        );
        assert!(
            cards[1]
                .query_descendants()
                .match_id("ContinueCard::continue_progress")
                .find_first()
                .is_none()
        );
    }

    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    idle(400);
    let picks = Rc::new(RefCell::new(Vec::new()));
    let callback = picks.clone();
    app.on_continue_picked(move |i| callback.borrow_mut().push(i));
    let upcoming = Rc::new(RefCell::new(Vec::new()));
    let callback = upcoming.clone();
    app.on_upcoming_picked(move |i| callback.borrow_mut().push(i));

    tap(
        &app,
        &child(
            &elements(&app, "ContinueCard")[0],
            "ContinueCard::continue_menu",
        ),
    );

    assert!(
        ElementHandle::find_by_accessible_label(&app, "Play")
            .next()
            .is_some()
    );
    assert!(
        picks.borrow().is_empty(),
        "menu button must not play the card"
    );
    app.set_system_back_request(app.get_system_back_request() + 1);
    idle(400);

    tap(
        &app,
        &child(
            &elements(&app, "UpcomingCard")[0],
            "UpcomingCard::upcoming_menu",
        ),
    );
    let action = ElementHandle::find_by_accessible_label(&app, "Enter series")
        .next()
        .unwrap();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Remove from Continue Watching")
            .next()
            .is_none()
    );
    assert!(upcoming.borrow().is_empty());
    tap(&app, &action);
    assert_eq!(upcoming.borrow().as_slice(), [0]);

    // A short viewport lets a stolen vertical gesture actually scroll the page.
    app.window().set_size(slint::PhysicalSize::new(390, 650));
    idle(400);
    // A quick diagonal touch over the new menu affordance remains a row flick.
    use i_slint_core::input::TouchPhase;
    let start = center(&child(
        &elements(&app, "ContinueCard")[0],
        "ContinueCard::continue_menu",
    ));
    let touch = |position: LogicalPosition, phase| {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::internal(
                slint::platform::InternalEvent::Touch {
                    id: 1,
                    position: i_slint_core::lengths::LogicalPoint::new(position.x, position.y),
                    phase,
                },
            ));
    };
    touch(start, TouchPhase::Started);
    idle(16);
    let mut end = start;
    for step in 1..=8 {
        end = LogicalPosition::new(start.x - step as f32 * 12.0, start.y - step as f32 * 2.0);
        touch(end, TouchPhase::Moved);
        idle(16);
    }
    touch(end, TouchPhase::Ended);
    idle(200);
    assert!(app.get_home_continue_x() < -20.0);
    assert!(app.get_home_scroll_y().abs() < 1.0);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Play")
            .next()
            .is_none()
    );
    assert!(picks.borrow().is_empty());
}

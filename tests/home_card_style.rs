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

fn hold(app: &nova::AppWindow, element: &ElementHandle) {
    let position = center(element);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    // The Flickable forwards the press after its own delay. Advance frames so
    // the card's hold timer starts before the remaining hold time elapses.
    for _ in 0..10 {
        idle(100);
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(100);
}

#[test]
fn home_card_geometry_menus_and_footer_flick_remain_usable() {
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
                "preserve the card size from the original roughly 2.45-card layout"
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

    // Artwork fills the window even with a status bar and landscape cutouts;
    // captions, actions and cards retain safe padding.
    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(28.0, 16.0, 12.0, 8.0),
    );
    app.set_home_featured_title("Featured title".into());
    app.set_home_featured_count(3);
    app.set_home_featured_revision(1);
    idle(400);
    let banner = elements(&app, "FeaturedShowcase").remove(0);
    assert!(banner.absolute_position().x.abs() < 0.5);
    assert!(banner.absolute_position().y.abs() < 0.5);
    assert!((banner.size().width - 390.0).abs() < 0.5);
    let caption = child(&banner, "FeaturedShowcase::current_caption");
    assert!(caption.absolute_position().x >= 12.0 + 18.0 - 0.5);
    assert!(caption.absolute_position().y >= 28.0);
    let action = child(&banner, "FeaturedShowcase::featured_actions");
    assert!(action.absolute_position().x >= 12.0 + 18.0 - 0.5);
    for id in ["HomePage::rail_continue", "HomePage::rail_upcoming"] {
        let rail = i_slint_backend_testing::ElementHandle::find_by_element_id(&app, id)
            .next()
            .unwrap();
        assert!(rail.absolute_position().x.abs() < 0.5, "{id}: left edge");
        assert!((rail.size().width - 390.0).abs() < 0.5, "{id}: right edge");
    }
    let card = &elements(&app, "ContinueCard")[0];
    assert!((card.absolute_position().x - 30.0).abs() < 0.5);
    assert!((card.size().width - ((390.0 - 36.0 - 20.0 + 18.0) / 2.5 - 18.0)).abs() < 0.5);
    // Keyboard follow includes the new content padding, otherwise the focused
    // card's right edge would still be clipped by exactly that inset.
    let card_width = card.size().width;
    app.set_kb_active(true);
    for zone in [1, 2] {
        app.set_home_kb_zone(zone);
        if zone == 1 {
            app.set_home_kb_idx(3);
        } else {
            app.set_home_kb_up_idx(3);
        }
        idle(100);
        let offset = if zone == 1 {
            app.get_home_continue_x()
        } else {
            app.get_home_upcoming_x()
        };
        let left = 30.0 + 3.0 * (card_width + 18.0) + offset;
        assert!(left >= -0.5 && left + card_width <= 390.5);
        if zone == 1 {
            app.set_home_kb_idx(0);
        } else {
            app.set_home_kb_up_idx(0);
        }
        idle(100);
    }
    app.set_kb_active(false);
    app.set_home_featured_count(0);
    i_slint_core::window::WindowInner::from_pub(app.window())
        .set_window_item_safe_area(i_slint_core::lengths::LogicalEdges::default());
    idle(400);
    let picks = Rc::new(RefCell::new(Vec::new()));
    let callback = picks.clone();
    app.on_continue_picked(move |i| callback.borrow_mut().push(i));
    let upcoming = Rc::new(RefCell::new(Vec::new()));
    let callback = upcoming.clone();
    app.on_upcoming_picked(move |i| callback.borrow_mut().push(i));

    hold(&app, &elements(&app, "ContinueCard")[0]);

    assert!(
        ElementHandle::find_by_accessible_label(&app, "Play")
            .next()
            .is_some()
    );
    assert!(
        picks.borrow().is_empty(),
        "holding a card must not also play it"
    );
    app.set_system_back_request(app.get_system_back_request() + 1);
    idle(400);

    tap(&app, &elements(&app, "UpcomingCard")[0]);
    assert_eq!(upcoming.borrow().as_slice(), [0]);

    // A short viewport lets a stolen vertical gesture actually scroll the page.
    app.window().set_size(slint::PhysicalSize::new(390, 650));
    idle(400);
    // The footer's right edge uses the card's normal gesture handling now
    // that the unsupported menu glyph/button has been removed.
    use i_slint_core::input::TouchPhase;
    let title = child(
        &elements(&app, "ContinueCard")[0],
        "ContinueCard::continue_title",
    );
    let start = LogicalPosition::new(
        title.absolute_position().x + title.size().width - 10.0,
        title.absolute_position().y + title.size().height / 2.0,
    );
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

    // A flick starting in the newly exposed left gutter must reach the rail,
    // even though that gutter is outside the padded row host's bounds.
    let before = app.get_home_continue_x();
    let rail = ElementHandle::find_by_element_id(&app, "HomePage::rail_continue")
        .next()
        .unwrap();
    let start = LogicalPosition::new(3.0, rail.absolute_position().y + 32.0);
    touch(start, TouchPhase::Started);
    idle(16);
    let mut end = start;
    for step in 1..=8 {
        end = LogicalPosition::new(start.x + step as f32 * 12.0, start.y - step as f32 * 2.0);
        touch(end, TouchPhase::Moved);
        idle(16);
    }
    touch(end, TouchPhase::Ended);
    idle(200);
    assert!(
        app.get_home_continue_x() > before + 20.0,
        "edge touch must pan the row"
    );
    assert!(app.get_home_scroll_y().abs() < 1.0);
    assert!(picks.borrow().is_empty());
}

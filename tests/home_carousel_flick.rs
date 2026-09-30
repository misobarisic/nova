//! Home landing carousel flick inertia (headless).
//!
//! The rails have no Flickable momentum of their own (see `CarouselDrag`):
//! the drag sampler measures the rail's offset per 16 ms frame and the fling
//! decays that velocity after the release. Covers that the inertia a flick
//! gets matches the speed it was thrown at — a release that lands on the
//! enclosing Flickable's 100 ms press-delivery mark (i.e. every quick flick)
//! used to get its velocity wiped and halved — that a flick started while the
//! rail is still coasting keeps its own speed, and that a drag held still
//! before the release does not fling at all.
//!
//! Gestures are time-spread (`mock_elapsed_time`): one move per 16 ms frame,
//! which is the cadence the sampler itself runs on.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::rc::Rc;
use std::time::Duration;

/// One frame of movement in the tests below, in logical pixels per 16 ms.
const FRAME: f32 = -25.0;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn press(app: &nova::AppWindow, position: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn moved(app: &nova::AppWindow, position: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved { position });
}

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

/// The first landing carousel card on screen (the narrow ~2.5-across variant).
fn find_landing_card(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "ContinueCard")
        .collect::<Vec<_>>()
        .into_iter()
        .find(|c| {
            let p = c.absolute_position();
            let s = c.size();
            s.width > 80.0 && s.width < 160.0 && p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
        })
}

fn setup() -> nova::AppWindow {
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(0);
    app.set_touch_menus(true);
    let cont: Vec<nova::ContinueRow> = (0..10)
        .map(|i| nova::ContinueRow {
            id: SharedString::from(format!("s{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E1 · Pilot"),
            poster: Default::default(),
            is_loaded: false,
            progress: 0.3,
            badge: 0,
        })
        .collect();
    app.set_home_continue(Rc::new(VecModel::from(cont)).into());
    let up: Vec<nova::UpcomingRow> = (0..10)
        .map(|i| nova::UpcomingRow {
            id: SharedString::from(format!("u{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E2 · Next"),
            date: SharedString::from("in 3 days"),
            poster: Default::default(),
            is_loaded: false,
            index: i,
        })
        .collect();
    app.set_home_upcoming(Rc::new(VecModel::from(up)).into());
    app.window().show().unwrap();
    idle(400);
    app
}

/// Press, wait `gap`, drag `frames` frames of `FRAME` px, release. Returns
/// (the rail offset at the release, the first coast frame's movement).
fn flick(app: &nova::AppWindow, gap: u64, frames: usize) -> (f32, f32) {
    let from = center(&find_landing_card(app).expect("landing card"));
    let start = app.get_home_continue_x();
    press(app, from);
    idle(gap);
    let mut x = from.x;
    for _ in 1..=frames {
        x += FRAME;
        moved(app, LogicalPosition::new(x, from.y));
        idle(16);
    }
    release(app, LogicalPosition::new(x, from.y));
    let at_release = app.get_home_continue_x();
    assert!(
        at_release - start < FRAME * frames as f32 * 0.9,
        "the drag must follow the finger (moved {:.1})",
        at_release - start
    );
    idle(16);
    (at_release, app.get_home_continue_x() - at_release)
}

#[test]
fn carousel_flick_inertia_matches_the_throw() {
    i_slint_backend_testing::init_integration_test_with_mock_time();

    // A quick flick whose release lands right on the press-delivery mark, and
    // a slower drag well past it: both must coast at the speed they were
    // thrown at (`|FRAME|` px per frame, minus the fling's first decay step).
    for gap in [64u64, 120] {
        let app = setup();
        let (at_release, coast) = flick(&app, gap, 8);
        assert!(
            coast.abs() >= FRAME.abs() * 0.8 && coast.abs() <= FRAME.abs() * 0.92 + 0.5,
            "a flick thrown at {:.0}px/frame must coast at nearly that speed, \
             not {:+.1} (gap {gap}, release at {at_release:.1})",
            FRAME.abs(),
            coast
        );
        drop(app);
    }

    // A flick started while the rail is still coasting keeps its own speed:
    // the coast must not be folded into the new flick's velocity (nor fight
    // the finger while it drags).
    {
        let app = setup();
        let (at_release, coast) = flick(&app, 120, 8);
        assert!(
            coast < -18.0,
            "the first flick must have inertia (coast {coast:+.1})"
        );
        idle(48);
        let during_coast = app.get_home_continue_x();
        let (at_release2, coast2) = flick(&app, 120, 8);
        assert!(
            at_release2 < during_coast,
            "the second flick must keep dragging from where the rail is"
        );
        assert!(
            coast2.abs() >= FRAME.abs() * 0.8 && coast2.abs() <= FRAME.abs() * 0.92 + 0.5,
            "the second flick must coast at its own speed, not the coast's \
             ({coast2:+.1}, first flick {coast:+.1} at {at_release:.1})"
        );
    }

    // A drag held still before the release is a placement, not a throw.
    {
        let app = setup();
        let from = center(&find_landing_card(&app).expect("landing card"));
        press(&app, from);
        idle(120);
        let mut x = from.x;
        for _ in 1..=4 {
            x += FRAME;
            moved(&app, LogicalPosition::new(x, from.y));
            idle(16);
        }
        idle(64);
        release(&app, LogicalPosition::new(x, from.y));
        let at_release = app.get_home_continue_x();
        idle(16);
        let coast = app.get_home_continue_x() - at_release;
        assert!(
            coast.abs() < 3.0,
            "a paused drag must not fling (coast {coast:+.1})"
        );
    }

    fast_flicks_do_not_jump_and_upcoming_coasts_too();
    pointer_flick_is_not_undone_by_keyboard_focus();
    a_slow_drag_after_a_settled_flick_starts_where_the_rail_stopped();
    touch_drag_after_settled_flick_stays_at_the_settled_offset();
}

fn fast_flicks_do_not_jump_and_upcoming_coasts_too() {
    let app = setup();

    // A touch backend may batch moves into one event. The rail follows the
    // full finger travel, but that entire burst must not become one enormous
    // 16 ms coast step (which appears as a jump to a different card).
    let from = center(&find_landing_card(&app).expect("landing card"));
    press(&app, from);
    idle(120);
    moved(&app, LogicalPosition::new(from.x - 120.0, from.y));
    let at_drag = app.get_home_continue_x();
    assert!(at_drag < -100.0, "the rail must follow the full drag");
    release(&app, LogicalPosition::new(from.x - 120.0, from.y));
    idle(16);
    let step = app.get_home_continue_x() - at_drag;
    assert!(
        step < -3.0 && step > -55.0,
        "coast jumped {step:+.1}px in one frame"
    );

    // Both rails use CarouselDrag. Start Upcoming at its visible first card,
    // then verify a regular flick gets momentum without moving Continue.
    let upcoming = ElementHandle::find_by_element_type_name(&app, "UpcomingCard")
        .find(|c| {
            let p = c.absolute_position();
            p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
        })
        .expect("landing Upcoming card");
    let from = center(&upcoming);
    press(&app, from);
    idle(120);
    let mut x = from.x;
    for _ in 0..4 {
        x -= 25.0;
        moved(&app, LogicalPosition::new(x, from.y));
        idle(16);
    }
    release(&app, LogicalPosition::new(x, from.y));
    let at_release = app.get_home_upcoming_x();
    assert!(at_release < -75.0, "Upcoming must follow the drag");
    idle(16);
    assert!(
        app.get_home_upcoming_x() < at_release - 15.0,
        "Upcoming must coast"
    );
}

fn pointer_flick_is_not_undone_by_keyboard_focus() {
    let app = setup();
    for key in [
        slint::platform::Key::DownArrow,
        slint::platform::Key::RightArrow,
    ] {
        let _ = app
            .window()
            .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
                text: key.into(),
            });
    }
    let focused = app.get_home_continue_x();
    let from = center(&find_landing_card(&app).expect("landing card"));
    press(&app, from);
    idle(120);
    let mut x = from.x;
    for _ in 0..4 {
        x -= 25.0;
        moved(&app, LogicalPosition::new(x, from.y));
        idle(16);
    }
    release(&app, LogicalPosition::new(x, from.y));
    for _ in 0..40 {
        idle(16);
    }
    let flicked = app.get_home_continue_x();
    assert!(
        flicked < focused - 100.0,
        "pointer flick must move past keyboard focus"
    );

    // A view change and a model refresh both run kb_follow. Neither may pull
    // the rail back to the formerly focused card after pointer navigation.
    app.set_home_view(1);
    idle(300);
    app.set_home_view(0);
    idle(300);
    assert!(
        (app.get_home_continue_x() - flicked).abs() < 2.0,
        "returning to the landing snapped the rail to stale keyboard focus"
    );
}

fn a_slow_drag_after_a_settled_flick_starts_where_the_rail_stopped() {
    let app = setup();
    let from = center(&find_landing_card(&app).expect("landing card"));
    press(&app, from);
    idle(120);
    let mut x = from.x;
    for _ in 0..4 {
        x -= 25.0;
        moved(&app, LogicalPosition::new(x, from.y));
        idle(16);
    }
    release(&app, LogicalPosition::new(x, from.y));
    for _ in 0..80 {
        idle(16);
    }
    let settled = app.get_home_continue_x();
    assert!(
        settled < -200.0,
        "first flick must move the rail off its start"
    );
    idle(200);
    assert!(
        (app.get_home_continue_x() - settled).abs() < 1.0,
        "coast must be over"
    );

    let from = LogicalPosition::new(160.0, from.y);
    press(&app, from);
    idle(120);
    for i in 1..=10 {
        moved(&app, LogicalPosition::new(from.x + i as f32 * 5.0, from.y));
        let offset = app.get_home_continue_x();
        assert!(
            (offset - settled).abs() < 65.0,
            "new slow drag jumped at move {i}: {settled:.1} -> {offset:.1}"
        );
        idle(16);
    }
    release(&app, LogicalPosition::new(from.x + 50.0, from.y));
}

fn touch(app: &nova::AppWindow, position: LogicalPosition, phase: i_slint_core::input::TouchPhase) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::internal(
            slint::platform::InternalEvent::Touch {
                id: 1,
                position: i_slint_core::lengths::LogicalPoint::new(position.x, position.y),
                phase,
            },
        ));
}

fn touch_drag_after_settled_flick_stays_at_the_settled_offset() {
    use i_slint_core::input::TouchPhase;
    let app = setup();
    let from = center(&find_landing_card(&app).expect("landing card"));
    touch(&app, from, TouchPhase::Started);
    idle(120);
    let mut x = from.x;
    for _ in 0..4 {
        x -= 25.0;
        touch(&app, LogicalPosition::new(x, from.y), TouchPhase::Moved);
        idle(16);
    }
    touch(&app, LogicalPosition::new(x, from.y), TouchPhase::Ended);
    for _ in 0..80 {
        idle(16);
    }
    let settled = app.get_home_continue_x();
    assert!(settled < -200.0, "touch flick must move the rail");
    idle(200);
    assert!((app.get_home_continue_x() - settled).abs() < 1.0);

    // A touch can be interrupted before it becomes a horizontal drag (e.g.
    // the page or OS takes it). Its provisional anchor must not leak into the
    // next gesture at a different screen position.
    let interrupted = LogicalPosition::new(70.0, from.y);
    touch(&app, interrupted, TouchPhase::Started);
    idle(120);
    touch(&app, LogicalPosition::new(75.0, from.y), TouchPhase::Moved);
    touch(
        &app,
        LogicalPosition::new(75.0, from.y),
        TouchPhase::Cancelled,
    );
    idle(16);

    let from = LogicalPosition::new(160.0, from.y);
    touch(&app, from, TouchPhase::Started);
    idle(120);
    for i in 1..=10 {
        touch(
            &app,
            LogicalPosition::new(from.x + i as f32 * 5.0, from.y),
            TouchPhase::Moved,
        );
        let offset = app.get_home_continue_x();
        assert!(
            (offset - settled).abs() < 60.0,
            "new touch drag jumped at move {i}: {settled:.1} -> {offset:.1}"
        );
        if i == 1 {
            assert!(
                (offset - settled).abs() < 2.0,
                "the first 5px move must not pan from a previous touch anchor"
            );
        }
        idle(16);
    }
    assert!(
        app.get_home_continue_x() > settled + 35.0,
        "the new touch must follow its own slow movement"
    );
    touch(
        &app,
        LogicalPosition::new(from.x + 50.0, from.y),
        TouchPhase::Ended,
    );
}

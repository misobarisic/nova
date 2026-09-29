//! Continue Watching card hover (headless).
//!
//! The Continue / Upcoming cards lift by 6px under a real pointer hover. On
//! touch platforms that flourish is suppressed: `TouchArea.has-hover` goes true
//! under a finger, and a press inside the subpage's `ScrollView` is
//! *delay-forwarded* (the Flickable waits ~100ms to see whether the gesture is
//! a drag). While that delayed press is pending, Slint still hit-tests the
//! move — setting `has-hover` on the card under the finger — but then throws
//! the dispatch away in favour of the retained one, so the card never enters
//! the stored input stack and the matching `Exit` is never delivered. The flag
//! latches: the card stays lifted after the finger has moved away or lifted,
//! with no way to clear it from Slint. The fix is the `touch_menus` gate on the
//! lift binding (touch platforms have no hover at all).
//!
//! Guards both halves: the lift still works for a pointer hover (desktop), and
//! nothing lifts on a touch platform — neither during the press, nor after it
//! is released elsewhere. One test function: the testing backend initializes
//! once per process.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
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
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(position.x + size.width / 2.0, position.y + size.height / 2.0)
}

/// Every card whose card background currently sits above its own box, i.e. is
/// lifted. The lift lives on a child (`lift`), so the card's own geometry does
/// not move — compare the child's absolute y against the card's.
fn lifted_cards(app: &nova::AppWindow, kind: &str) -> Vec<String> {
    ElementHandle::find_by_element_type_name(app, kind)
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
        .filter_map(|(i, card)| {
            let lift = card.query_descendants().match_id(&format!("{kind}::lift")).find_first()?;
            let dy = lift.absolute_position().y - card.absolute_position().y;
            (dy < -1.0).then(|| {
                let p = card.absolute_position();
                format!("{kind}[{i}] x={:.0} y={:.0} dy={dy:.1}", p.x, p.y)
            })
        })
        .collect()
}

/// The visible subpage grid cards, in tree order. The landing carousel variant
/// is narrower (and `home_view` keeps it out of the grid's own instances
/// anyway), so the width filter picks the grid cards.
fn visible_grid_cards(app: &nova::AppWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "ContinueCard")
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|card| {
            let p = card.absolute_position();
            card.size().width > 140.0 && p.x >= 10.0 && p.y > 60.0 && p.y < 700.0
        })
        .collect()
}

#[test]
fn continue_card_hover_lift_is_pointer_only() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    // Assert the lift *binding*, not the 150ms ease: with the hover animations
    // off the lift is applied in the same tick as the pointer event, so the
    // geometry can be read right away (the app pushes this switch into the
    // global from Settings → Look and feel; the test drives AppWindow directly).
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    // The Continue Watching "see all" subpage: the only place these cards are
    // tappable (the landing rail's own cards are disabled for `CarouselDrag`).
    app.set_home_view(1);
    let rows: Vec<nova::ContinueRow> = (0..6)
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
    app.set_home_continue(Rc::new(VecModel::from(rows)).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();

        // ---- Desktop: a pointer hover still lifts the card ----
        app.set_touch_menus(false);
        let cards = visible_grid_cards(&app);
        let Some(hovered) = cards.first() else {
            fail(&failures1, false, "DIAG: no visible subpage grid card");
            slint::quit_event_loop().unwrap();
            return;
        };
        let a = center(hovered);
        moved(&app, a);
        let lifted = lifted_cards(&app, "ContinueCard");
        fail(
            &failures1,
            lifted.len() == 1,
            &format!("a pointer hover must lift exactly the hovered card, got {lifted:?}"),
        );
        // Leaving the card drops it again.
        moved(&app, LogicalPosition::new(a.x, 20.0));
        let lifted = lifted_cards(&app, "ContinueCard");
        fail(
            &failures1,
            lifted.is_empty(),
            &format!("leaving a card must drop its lift, got {lifted:?}"),
        );

        // ---- Touch platform: nothing lifts, ever ----
        app.set_touch_menus(true);
        let app2 = app1.clone();
        let failures2 = failures1.clone();
        after(200, move || {
            let app = app2.upgrade().unwrap();
            let cards = visible_grid_cards(&app);
            if cards.len() < 2 {
                fail(&failures2, false, "DIAG: not enough visible subpage grid cards");
                slint::quit_event_loop().unwrap();
                return;
            }
            let a = center(&cards[0]);
            let b = center(&cards[1]);

            // Touch down on a card, a small wiggle (still inside the
            // ScrollView's delay window), then release where the finger is.
            press(&app, a);
            moved(&app, LogicalPosition::new(a.x + 3.0, a.y));
            release(&app, LogicalPosition::new(a.x + 3.0, a.y));
            let lifted = lifted_cards(&app, "ContinueCard");
            fail(
                &failures2,
                lifted.is_empty(),
                &format!("a touch on a card must not leave it lifted, got {lifted:?}"),
            );

            // Touch down on a card, drag onto the next one, and release there:
            // neither the pressed nor the passed-over card may stay lifted.
            press(&app, a);
            moved(&app, LogicalPosition::new(a.x + 40.0, a.y + 20.0));
            moved(&app, b);
            release(&app, b);
            let lifted = lifted_cards(&app, "ContinueCard");
            fail(
                &failures2,
                lifted.is_empty(),
                &format!("a touch drag must not leave cards lifted, got {lifted:?}"),
            );

            // Re-check on a later tick: the stranded lift used to show only
            // once the trailing release had cleared the press state, so
            // asserting on the same tick alone would be too weak.
            let app3 = app2.clone();
            let failures3 = failures2.clone();
            after(250, move || {
                let app = app3.upgrade().unwrap();
                let lifted = lifted_cards(&app, "ContinueCard");
                fail(
                    &failures3,
                    lifted.is_empty(),
                    &format!("no card may stay lifted after a touch, got {lifted:?}"),
                );
                slint::quit_event_loop().unwrap();
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(failures.is_empty(), "continue card hover failures:\n  {}", failures.join("\n  "));
}

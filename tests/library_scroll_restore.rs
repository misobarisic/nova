//! My Library scroll restore (headless).
//!
//! Leaving the library for an entry and coming back must land on the exact
//! same offset. The page is recreated on return and restores its absolute
//! `scroll_y`, but the one-shot restore used to also *follow* the focused card
//! — which nudged a partially visible row into view and shifted the listing.
//! A row that is fully hidden (the page came back focused elsewhere) must
//! still be revealed: that is the recovery the follow was there for.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Synchronous upward drag over `from`: press, four small moves, release.
fn drag_up(app: &nova::AppWindow, from: LogicalPosition, dy: f32) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: from,
            button: slint::platform::PointerEventButton::Left,
        });
    for step in 1..=4 {
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(from.x, from.y - dy * step as f32 / 4.0),
                });
    }
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: LogicalPosition::new(from.x, from.y - dy),
                button: slint::platform::PointerEventButton::Left,
            });
}

fn card(id: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: s(format!("id{id}").as_str()),
        title: s(format!("Title {id}").as_str()),
        year: s("2024"),
        poster_path: SharedString::default(),
        poster: Default::default(),
        is_loaded: false,
        badge: SharedString::default(),
        watched: false,
    }
}

#[test]
fn returning_to_the_library_keeps_the_scroll_offset() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_library(
        Rc::new(VecModel::from(
            (0..60).map(card).collect::<Vec<nova::MediaCard>>(),
        ))
        .into(),
    );
    // Focus on the first card: after a small scroll its row is only partly
    // visible — exactly the case the restore used to nudge up.
    app.set_library_kb_zone(2);
    app.set_library_kb_idx(0);

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
        let Some(scroll) = ElementHandle::find_by_element_type_name(&app, "ScrollView").next()
        else {
            fail(&failures1, false, "DIAG: no library grid");
            slint::quit_event_loop().unwrap();
            return;
        };
        let pos = scroll.absolute_position();
        let size = scroll.size();
        drag_up(
            &app,
            LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0),
            60.0,
        );

        let failures2 = failures1.clone();
        let app2 = app.as_weak();
        // Long enough for any flick momentum to die out.
        after(900, move || {
            let app = app2.upgrade().unwrap();
            let before = app.get_library_scroll_y();
            fail(
                &failures2,
                (before + 60.0).abs() <= 45.0 && before != 0.0,
                &format!("DIAG: the grid did not scroll (offset {before})"),
            );

            // Open an entry, then come back.
            app.set_modal_visible(true);
            let failures3 = failures2.clone();
            let app3 = app.as_weak();
            after(400, move || {
                let app = app3.upgrade().unwrap();
                app.set_modal_visible(false);
                let failures4 = failures3.clone();
                let app4 = app.as_weak();
                after(800, move || {
                    let app = app4.upgrade().unwrap();
                    let after_return = app.get_library_scroll_y();
                    fail(
                        &failures4,
                        (after_return - before).abs() <= 0.5,
                        &format!(
                            "returning from an entry must keep the offset (was {before}, now {after_return})"
                        ),
                    );

                    // A card far outside the viewport is still revealed when
                    // the page comes back focused on it.
                    app.set_modal_visible(true);
                    app.set_library_kb_idx(59);
                    let failures5 = failures4.clone();
                    let app5 = app.as_weak();
                    after(400, move || {
                        let app = app5.upgrade().unwrap();
                        app.set_modal_visible(false);
                        let failures6 = failures5.clone();
                        let app6 = app.as_weak();
                        after(800, move || {
                            let app = app6.upgrade().unwrap();
                            let revealed = app.get_library_scroll_y();
                            fail(
                                &failures6,
                                revealed < before - 100.0,
                                &format!(
                                    "a fully hidden focused card must be revealed (was {before}, now {revealed})"
                                ),
                            );
                            slint::quit_event_loop().unwrap();
                        });
                    });
                });
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "library scroll failures:\n  {}",
        failures.join("\n  ")
    );
}

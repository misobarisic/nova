//! The Home featured showcase restarts its rotation delay on selection changes.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn tap(app: &nova::AppWindow, position: LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

#[test]
fn featured_rotation_restarts_on_selection_change_and_manual_step() {
    i_slint_backend_testing::init_integration_test_with_mock_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_featured_count(3);

    let steps = Rc::new(RefCell::new(Vec::new()));
    let captured_steps = steps.clone();
    let app_weak = app.as_weak();
    app.on_home_featured_step(move |delta| {
        captured_steps.borrow_mut().push(delta);
        if let Some(app) = app_weak.upgrade() {
            let next = (app.get_home_featured_index() + delta).rem_euclid(3);
            app.set_home_featured_index(next);
        }
    });

    // A committed selection at eight seconds starts a fresh interval.
    idle(0);
    idle(8_000);
    app.set_home_featured_index(1);
    idle(0);
    idle(8_000);
    assert!(
        steps.borrow().is_empty(),
        "selection change must reset the timer"
    );
    idle(1_000);
    assert_eq!(*steps.borrow(), vec![1], "automatic rotation must continue");

    // The automatic step occurred at seventeen seconds. A manual step at
    // twenty-five seconds must postpone the next automatic step until thirty-four.
    idle(8_000);
    let next = ElementHandle::find_by_accessible_label(&app, "Next featured title")
        .next()
        .expect("featured next button");
    let origin = next.absolute_position();
    let size = next.size();
    tap(
        &app,
        LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0),
    );
    idle(0);
    assert_eq!(
        *steps.borrow(),
        vec![1, 1],
        "manual button must step forward"
    );
    idle(8_000);
    assert_eq!(
        *steps.borrow(),
        vec![1, 1],
        "manual step must reset the timer"
    );
    idle(1_000);
    assert_eq!(
        *steps.borrow(),
        vec![1, 1, 1],
        "rotation must resume after a full interval"
    );
}

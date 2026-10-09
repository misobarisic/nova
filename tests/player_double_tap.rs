//! Double-tap seeking preserves OSD and pause state throughout the gesture.

use slint::{ComponentHandle, LogicalPosition};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

fn tap(app: &nova::AppWindow, x: f32, y: f32) {
    let c = LogicalPosition::new(x, y);
    press(app, c);
    release(app, c);
}

#[test]
fn double_tap_seeks_without_revealing_controls_even_between_taps() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let advance = |ms| {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(ms));
    };
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.window().show().unwrap();
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_position(100.0);
    app.set_duration(600.0);

    let wakes = Rc::new(Cell::new(0));
    let toggles = Rc::new(Cell::new(0));
    let seeks = Rc::new(RefCell::new(Vec::new()));
    {
        let wakes = wakes.clone();
        let weak = app.as_weak();
        app.on_osd_mouse_moved(move || {
            wakes.set(wakes.get() + 1);
            weak.upgrade().unwrap().set_osd_visible(true);
        });
    }
    {
        let toggles = toggles.clone();
        app.on_toggle_pause(move || toggles.set(toggles.get() + 1));
    }
    {
        let seeks = seeks.clone();
        app.on_seek(move |pos| seeks.borrow_mut().push(pos));
    }
    advance(500);

    for android in [true, false] {
        app.set_is_android(android);
        for paused in [false, true] {
            app.set_is_paused(paused);
            for visible in [false, true] {
                for (x, delta) in [(100.0, -10.0), (500.0, 10.0)] {
                    app.set_osd_visible(visible);
                    wakes.set(0);
                    toggles.set(0);
                    seeks.borrow_mut().clear();
                    let start = app.get_position();
                    let p = LogicalPosition::new(x, 300.0);
                    press(&app, p);
                    assert_eq!(app.get_osd_visible(), visible, "first press");
                    // Small finger jitter must remain a tap, without waking
                    // controls through the movement callbacks or release.
                    moved(&app, LogicalPosition::new(x + 3.0, 302.0));
                    release(&app, LogicalPosition::new(x + 3.0, 302.0));
                    advance(100);
                    assert_eq!(app.get_osd_visible(), visible, "between taps");
                    assert_eq!(wakes.get(), 0, "no transient OSD flash");
                    assert_eq!(toggles.get(), 0, "no first-tap pause toggle");
                    press(&app, p);
                    assert_eq!(app.get_osd_visible(), visible, "second press");
                    release(&app, p);
                    assert_eq!(seeks.borrow().as_slice(), [start + delta]);
                    assert_eq!(app.get_osd_visible(), visible, "after seeking");
                    assert_eq!(app.get_is_paused(), paused);
                    advance(400);
                    assert_eq!(wakes.get(), 0, "no trailing single-tap wake");
                    assert_eq!(toggles.get(), 0);
                    assert_eq!(app.get_osd_visible(), visible);
                }
            }
        }
    }

    // A genuine single side tap still toggles controls after pairing ends.
    app.set_is_android(true);
    app.set_osd_visible(false);
    seeks.borrow_mut().clear();
    tap(&app, 500.0, 300.0);
    advance(279);
    assert!(!app.get_osd_visible());
    advance(2);
    assert!(app.get_osd_visible());
    assert!(seeks.borrow().is_empty());
    tap(&app, 100.0, 300.0);
    advance(281);
    assert!(!app.get_osd_visible());

    // Centre taps have no seek ambiguity and remain immediate.
    tap(&app, 300.0, 300.0);
    assert!(app.get_osd_visible());
    tap(&app, 300.0, 300.0);
    assert!(!app.get_osd_visible());

    // A drag cancels the pending single rather than hiding the controls later.
    tap(&app, 500.0, 300.0);
    press(&app, LogicalPosition::new(500.0, 300.0));
    moved(&app, LogicalPosition::new(530.0, 300.0));
    release(&app, LogicalPosition::new(530.0, 300.0));
    assert!(app.get_osd_visible());
    advance(400);
    assert!(app.get_osd_visible());
    assert!(seeks.borrow().is_empty());

    // The pairing deadline applies to the second press, not its release.
    app.set_osd_visible(false);
    tap(&app, 500.0, 300.0);
    advance(270);
    press(&app, LogicalPosition::new(500.0, 300.0));
    advance(100);
    assert!(!app.get_osd_visible(), "second tap held past deadline");
    release(&app, LogicalPosition::new(500.0, 300.0));
    assert_eq!(seeks.borrow().len(), 1);
    advance(400);
    assert!(!app.get_osd_visible());

    // Different sides remain two singles; they cannot become a seek.
    seeks.borrow_mut().clear();
    tap(&app, 500.0, 300.0);
    advance(100);
    tap(&app, 100.0, 300.0);
    assert!(app.get_osd_visible(), "first independent single");
    advance(281);
    assert!(!app.get_osd_visible(), "second independent single");
    assert!(seeks.borrow().is_empty());
}

//! The Home showcase responds to horizontal swipes without changing the
//! Continue Watching / Upcoming rails.

use slint::{ComponentHandle, LogicalPosition};
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
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
}

#[test]
fn home_showcase_swipes_between_titles() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_featured_title("Showcase title".into());
    app.set_home_featured_count(3);

    let steps = Rc::new(RefCell::new(Vec::new()));
    let captured = steps.clone();
    app.on_home_featured_step(move |delta| captured.borrow_mut().push(delta));

    let weak = app.as_weak();
    after(80, move || {
        let app = weak.upgrade().unwrap();
        let center = LogicalPosition::new(180.0, 170.0);
        press(&app, center);
        for x in [160.0, 135.0, 110.0, 80.0] {
            moved(&app, LogicalPosition::new(x, center.y));
        }
        release(&app, LogicalPosition::new(80.0, center.y));

        let center = LogicalPosition::new(180.0, 170.0);
        press(&app, center);
        for x in [200.0, 225.0, 250.0, 280.0] {
            moved(&app, LogicalPosition::new(x, center.y));
        }
        release(&app, LogicalPosition::new(280.0, center.y));

        assert_eq!(*steps.borrow(), vec![1, -1]);
        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();
}

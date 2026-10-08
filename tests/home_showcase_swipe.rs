//! The Home showcase responds to horizontal swipes without changing the
//! Continue Watching / Upcoming rails.

use i_slint_backend_testing::ElementHandle;
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

    let picks = Rc::new(RefCell::new(0));
    let captured = picks.clone();
    app.on_home_featured_picked(move || *captured.borrow_mut() += 1);
    let watches = Rc::new(RefCell::new(0));
    let captured = watches.clone();
    app.on_home_featured_watch_now(move || *captured.borrow_mut() += 1);

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
        assert_eq!(*picks.borrow(), 0, "swipes must not open banner details");

        // A vertical drag also must not become a stationary banner tap.
        press(&app, center);
        moved(&app, LogicalPosition::new(center.x, center.y - 50.0));
        release(&app, LogicalPosition::new(center.x, center.y - 50.0));
        assert_eq!(*picks.borrow(), 0);
        press(&app, center);
        release(&app, center);
        assert_eq!(*picks.borrow(), 1, "a stationary banner tap opens details");

        let tap_control = |label: &str| {
            let e = ElementHandle::find_by_accessible_label(&app, label)
                .next()
                .unwrap();
            let p = e.absolute_position();
            let size = e.size();
            let p = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
            press(&app, p);
            release(&app, p);
        };
        tap_control("Start watching");
        assert_eq!(*watches.borrow(), 1);
        assert_eq!(*picks.borrow(), 1, "playback must not also open details");
        tap_control("Next featured title");
        tap_control("Previous featured title");
        assert_eq!(*steps.borrow(), vec![1, -1, 1, -1]);
        assert_eq!(*picks.borrow(), 1, "paging must not open details");
        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();
}

//! Player backdrop gestures (headless).
//!
//! The video backdrop owns taps (OSD toggle), double-taps on the outer
//! thirds (∓10 s seek), a 500 ms press-and-hold (transient 2× preview until
//! release) and, on Android, vertical swipes (left = brightness, right =
//! system volume). Covers that double-taps seek without waking controls or
//! toggling playback, side singles wait for pairing, centre taps act at once,
//! holds preview and restore the stored rate, and swipes step the system
//! bridges without seeking.

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

fn tap(app: &nova::AppWindow, x: f32, y: f32) {
    let c = LogicalPosition::new(x, y);
    press(app, c);
    release(app, c);
}

#[test]
fn backdrop_double_tap_hold_and_swipe() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.window().show().unwrap();
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_position(100.0);
    app.set_duration(600.0);
    app.set_is_paused(false);
    app.set_osd_visible(true);
    app.set_playback_speed(1.0);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let seeks: Rc<RefCell<Vec<f32>>> = Rc::new(RefCell::new(Vec::new()));
    let previews: Rc<RefCell<Vec<f32>>> = Rc::new(RefCell::new(Vec::new()));
    let toggles: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let brightness: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let volumes: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let seeks = seeks.clone();
        app.on_seek(move |pos| seeks.borrow_mut().push(pos));
    }
    {
        let previews = previews.clone();
        app.on_playback_speed_preview(move |v| previews.borrow_mut().push(v));
    }
    {
        let toggles = toggles.clone();
        app.on_toggle_pause(move || *toggles.borrow_mut() += 1);
    }
    // Mimic the backend OSD wake: re-showing the bar (the real countdown
    // that hides it again lives in run.rs).
    {
        let app_w = app.as_weak();
        app.on_osd_mouse_moved(move || {
            app_w.upgrade().unwrap().set_osd_visible(true);
        });
    }
    {
        let brightness = brightness.clone();
        let app_w = app.as_weak();
        app.on_android_brightness_step(move |d| {
            brightness.borrow_mut().push(d);
            // Mimic the backend mirroring the session level.
            app_w.upgrade().unwrap().set_player_brightness(0.6);
        });
    }
    {
        let volumes = volumes.clone();
        app.on_android_volume_step(move |d| volumes.borrow_mut().push(d));
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let (seeks1, previews1, toggles1) = (seeks.clone(), previews.clone(), toggles.clone());
    after(300, move || {
        let app = app1.upgrade().unwrap();

        // ---- Double-tap right seeks +10 s, OSD untouched ----
        tap(&app, 500.0, 300.0);
        tap(&app, 500.0, 300.0);
        fail(
            &failures1,
            seeks1.borrow().as_slice() == [110.0],
            &format!(
                "double-tap right must seek +10 s (got {:?})",
                seeks1.borrow()
            ),
        );
        fail(
            &failures1,
            app.get_osd_visible(),
            "double-tap must not toggle the OSD",
        );
        fail(
            &failures1,
            toggles1.borrow().eq(&0),
            "double-tap must not toggle pause",
        );
        fail(
            &failures1,
            previews1.borrow().is_empty(),
            "double-tap must not preview speed",
        );

        // ---- Double-tap left seeks −10 s ----
        tap(&app, 100.0, 300.0);
        tap(&app, 100.0, 300.0);
        fail(
            &failures1,
            seeks1.borrow().as_slice() == [110.0, 100.0],
            &format!(
                "double-tap left must seek −10 s (got {:?})",
                seeks1.borrow()
            ),
        );

        // ---- Lone side tap hides the OSD after the pairing window ----
        tap(&app, 500.0, 300.0);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(450, move || {
            let app = app2.upgrade().unwrap();
            fail(
                &failures2,
                !app.get_osd_visible(),
                "lone tap must hide the OSD after the window",
            );
            fail(
                &failures2,
                seeks1.borrow().len() == 2,
                "lone tap must not seek",
            );

            // ---- Middle tap wakes immediately (and toggles pause off Android) ----
            tap(&app, 300.0, 300.0);
            fail(
                &failures2,
                app.get_osd_visible(),
                "middle tap must wake the OSD at once",
            );
            fail(
                &failures2,
                toggles1.borrow().eq(&1),
                "desktop wake tap must toggle pause",
            );
            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(450, move || {
                let app = app3.upgrade().unwrap();
                fail(
                    &failures3,
                    toggles1.borrow().eq(&1),
                    "no trailing single may fire after an immediate tap",
                );

                // ---- Press-and-hold previews 2×, release restores ----
                press(&app, LogicalPosition::new(300.0, 300.0));
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                after(700, move || {
                    let app = app4.upgrade().unwrap();
                    fail(
                        &failures4,
                        previews1.borrow().as_slice() == [2.0],
                        &format!("hold must preview 2× (got {:?})", previews1.borrow()),
                    );
                    release(&app, LogicalPosition::new(300.0, 300.0));
                    fail(
                        &failures4,
                        previews1.borrow().as_slice() == [2.0, 1.0],
                        &format!(
                            "release must restore the stored rate (got {:?})",
                            previews1.borrow()
                        ),
                    );
                    fail(&failures4, seeks1.borrow().len() == 2, "hold must not seek");
                    fail(
                        &failures4,
                        app.get_osd_visible(),
                        "hold must leave the OSD up",
                    );

                    // ---- Android swipes: brightness left, volume right ----
                    app.set_is_android(true);
                    let p = LogicalPosition::new(100.0, 300.0);
                    press(&app, p);
                    for k in 1..=8 {
                        moved(&app, LogicalPosition::new(100.0, 300.0 + k as f32 * 15.0));
                    }
                    release(&app, LogicalPosition::new(100.0, 420.0));
                    // 120px down at 40px/notch (threshold travel counts
                    // towards the first notch): three down-steps.
                    let app5 = app.as_weak();
                    let failures5 = failures4.clone();
                    let brightness1 = brightness.clone();
                    let volumes1 = volumes.clone();
                    after(200, move || {
                        let app = app5.upgrade().unwrap();
                        fail(
                            &failures5,
                            brightness1.borrow().as_slice() == [-1, -1, -1],
                            &format!(
                                "left swipe must step brightness down (got {:?})",
                                brightness1.borrow()
                            ),
                        );
                        let p = LogicalPosition::new(500.0, 300.0);
                        press(&app, p);
                        for k in 1..=7 {
                            moved(&app, LogicalPosition::new(500.0, 300.0 + k as f32 * 15.0));
                        }
                        release(&app, LogicalPosition::new(500.0, 405.0));
                        let app6 = app.as_weak();
                        let failures6 = failures5.clone();
                        after(200, move || {
                            let app = app6.upgrade().unwrap();
                            fail(
                                &failures6,
                                volumes1.borrow().as_slice() == [-1, -1],
                                &format!(
                                    "right swipe must step volume down (got {:?})",
                                    volumes1.borrow()
                                ),
                            );
                            // A notification-shade pull can reach Slint
                            // before Android takes over. Neither half may
                            // become a player swipe later in the same drag.
                            for x in [100.0, 500.0] {
                                press(&app, LogicalPosition::new(x, 8.0));
                                for k in 1..=12 {
                                    moved(&app, LogicalPosition::new(x, 8.0 + k as f32 * 20.0));
                                }
                                release(&app, LogicalPosition::new(x, 248.0));
                            }
                            fail(
                                &failures6,
                                brightness1.borrow().as_slice() == [-1, -1, -1]
                                    && volumes1.borrow().as_slice() == [-1, -1],
                                "top-edge shade pulls must not change brightness or volume",
                            );
                            fail(
                                &failures6,
                                seeks1.borrow().len() == 2,
                                "swipes must not seek",
                            );
                            fail(
                                &failures6,
                                app.get_osd_visible(),
                                "swipes must leave the OSD up",
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
        "player gesture failures:\n  {}",
        failures.join("\n  ")
    );
}

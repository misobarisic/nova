//! Hidden player OSD touch routing (headless).
//!
//! The player fades unavailable controls with opacity. Opacity alone does not
//! remove Slint hit-testing, so hidden-control handlers must suppress their
//! actions and wake the OSD instead. A hidden tap must not activate pause,
//! close, seek, or other bar targets, while an empty-bar tap must wake the OSD
//! without a hover-only flash. With the OSD already up, tapping empty video or
//! bar space (not a button or the seekbar) must dismiss it.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition};
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn press(
    app: &nova::AppWindow,
    position: LogicalPosition,
) -> Result<slint::platform::WindowEventDispatchResult, slint::PlatformError> {
    // No synthetic hover/move: a touchscreen tap is press followed by release.
    app.window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        })
}

fn release(
    app: &nova::AppWindow,
    position: LogicalPosition,
) -> Result<slint::platform::WindowEventDispatchResult, slint::PlatformError> {
    app.window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        })
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn is_size(element: &ElementHandle, width: f32, height: f32) -> bool {
    let size = element.size();
    (size.width - width).abs() < 1.5 && (size.height - height).abs() < 1.5
}

#[test]
fn hidden_osd_taps_wake_without_activating_controls() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_position(123.0);
    app.set_duration(600.0);
    app.set_osd_visible(false);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let wakes: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let toggles: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let closes: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let seeks: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));

    // Mirror the backend wake: the first tap makes the OSD visible again.
    app.on_osd_mouse_moved({
        let wakes = wakes.clone();
        let app = app.as_weak();
        move || {
            *wakes.borrow_mut() += 1;
            app.upgrade().unwrap().set_osd_visible(true);
        }
    });
    app.on_toggle_pause({
        let toggles = toggles.clone();
        move || *toggles.borrow_mut() += 1
    });
    app.on_close_player({
        let closes = closes.clone();
        move || *closes.borrow_mut() += 1
    });
    app.on_seek({
        let seeks = seeks.clone();
        move |_| *seeks.borrow_mut() += 1
    });

    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };
    let reset = |app: &nova::AppWindow,
                 wakes: &Rc<RefCell<u32>>,
                 toggles: &Rc<RefCell<u32>>,
                 closes: &Rc<RefCell<u32>>,
                 seeks: &Rc<RefCell<u32>>| {
        *wakes.borrow_mut() = 0;
        *toggles.borrow_mut() = 0;
        *closes.borrow_mut() = 0;
        *seeks.borrow_mut() = 0;
        app.set_osd_visible(false);
    };
    let check_wake_only = move |app: &nova::AppWindow,
                                failures: &Rc<RefCell<Vec<String>>>,
                                wakes: &Rc<RefCell<u32>>,
                                toggles: &Rc<RefCell<u32>>,
                                closes: &Rc<RefCell<u32>>,
                                seeks: &Rc<RefCell<u32>>,
                                what: &str| {
        fail(
            failures,
            *wakes.borrow() >= 1,
            &format!("hidden {what} tap must wake the OSD"),
        );
        fail(
            failures,
            *toggles.borrow() == 0,
            &format!("hidden {what} tap must not toggle pause"),
        );
        fail(
            failures,
            *closes.borrow() == 0,
            &format!("hidden {what} tap must not close the player"),
        );
        fail(
            failures,
            *seeks.borrow() == 0,
            &format!("hidden {what} tap must not seek"),
        );
        fail(
            failures,
            app.get_osd_visible(),
            &format!("hidden {what} tap must leave the OSD visible"),
        );
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let wakes1 = wakes.clone();
    let toggles1 = toggles.clone();
    let closes1 = closes.clone();
    let seeks1 = seeks.clone();
    after(500, move || {
        let app = app1.upgrade().unwrap();
        let height = app.window().size().height as f32;

        // Android centered transport play/pause: the 72×72 button in the
        // middle of the screen. Its hidden hit area must not toggle playback.
        let play_pause = ElementHandle::find_by_element_type_name(&app, "IconBtn")
            .find(|element| is_size(element, 72.0, 72.0));
        // Top-left close hit area. It must only wake the OSD while hidden.
        let close = ElementHandle::find_by_element_type_name(&app, "TouchArea").find(|element| {
            let position = element.absolute_position();
            is_size(element, 40.0, 40.0)
                && (12.0..24.0).contains(&position.x)
                && (12.0..24.0).contains(&position.y)
        });
        // Bottom OSD hover strip. While hidden, tapping it must wake the OSD
        // instead of painting a hover-only flash.
        let bar = ElementHandle::find_by_element_type_name(&app, "TouchArea").find(|element| {
            let position = element.absolute_position();
            let size = element.size();
            size.width > 300.0
                && (size.height - 96.0).abs() < 1.5
                && position.x.abs() < 1.5
                && position.y > height - 130.0
        });
        let backdrop =
            ElementHandle::find_by_element_type_name(&app, "TouchArea").find(|element| {
                let position = element.absolute_position();
                let size = element.size();
                position.x.abs() < 1.5
                    && position.y.abs() < 1.5
                    && size.width > 350.0
                    && size.height > 700.0
            });

        let (Some(play_pause), Some(close), Some(bar), Some(_backdrop)) =
            (play_pause, close, bar, backdrop)
        else {
            fail(
                &failures1,
                false,
                "DIAG: expected hidden controls, OSD bar, and video backdrop hit areas",
            );
            slint::quit_event_loop().unwrap();
            return;
        };

        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let position = center(&play_pause);
        let _ = press(&app, position);
        let _ = release(&app, position);
        check_wake_only(
            &app,
            &failures1,
            &wakes1,
            &toggles1,
            &closes1,
            &seeks1,
            "play/pause",
        );

        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let position = center(&close);
        let _ = press(&app, position);
        let _ = release(&app, position);
        check_wake_only(
            &app, &failures1, &wakes1, &toggles1, &closes1, &seeks1, "close",
        );

        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let position = center(&bar);
        let _ = press(&app, position);
        let _ = release(&app, position);
        // A hover-tracking press can legitimately wake before release; the
        // regression is that the tap must wake and stay awake.
        fail(
            &failures1,
            *wakes1.borrow() >= 1,
            "hidden OSD bar tap must wake the OSD",
        );
        fail(
            &failures1,
            *toggles1.borrow() == 0,
            "hidden OSD bar tap must not toggle pause",
        );
        fail(
            &failures1,
            *closes1.borrow() == 0,
            "hidden OSD bar tap must not close the player",
        );
        fail(
            &failures1,
            *seeks1.borrow() == 0,
            "hidden OSD bar tap must not seek",
        );
        fail(
            &failures1,
            app.get_osd_visible(),
            "hidden OSD bar tap must leave the OSD visible",
        );

        // With the OSD already up, tapping an empty part of the bar (above
        // the control row, clear of any button or the seekbar) must dismiss
        // it, like tapping the video, and must not run a control action.
        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        app.set_osd_visible(true);
        let bar_empty = LogicalPosition::new(180.0, height - 90.0);
        let _ = press(&app, bar_empty);
        let _ = release(&app, bar_empty);
        fail(
            &failures1,
            !app.get_osd_visible(),
            "tapping empty OSD bar space must dismiss the OSD",
        );
        fail(
            &failures1,
            *toggles1.borrow() == 0 && *closes1.borrow() == 0 && *seeks1.borrow() == 0,
            "empty OSD bar tap must not act on a control",
        );

        // The centre video area has no seek ambiguity and dismisses at once.
        // Side singles and double taps are covered in player_double_tap.rs.
        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        app.set_osd_visible(true);
        let video = LogicalPosition::new(180.0, 200.0);
        let _ = press(&app, video);
        let _ = release(&app, video);
        fail(
            &failures1,
            !app.get_osd_visible(),
            "tapping the video while the OSD is up must dismiss it",
        );

        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let start = LogicalPosition::new(180.0, 200.0);
        let _ = press(&app, start);
        fail(
            &failures1,
            !app.get_osd_visible(),
            "empty-space press must wait for a recognized tap or gesture",
        );
        let _ = release(&app, start);
        check_wake_only(
            &app,
            &failures1,
            &wakes1,
            &toggles1,
            &closes1,
            &seeks1,
            "empty-space",
        );

        // Backdrop swipe while hidden: `moved` wakes the OSD, so the release
        // must not be treated as a tap-to-hide (the bar used to flash and
        // vanish). The swipe must leave it up.
        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let _ = press(&app, start);
        for dy in [10.0, 30.0, 60.0] {
            let _ = app.window().dispatch_event_with_result(
                slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(180.0, 200.0 - dy),
                },
            );
        }
        fail(&failures1, *wakes1.borrow() >= 1, "swipe must wake the OSD");
        let _ = release(&app, LogicalPosition::new(180.0, 140.0));
        fail(
            &failures1,
            app.get_osd_visible(),
            "backdrop swipe must leave the OSD visible",
        );

        app.set_is_android(false);
        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(181.0, 200.0),
                });
        fail(
            &failures1,
            app.get_osd_visible(),
            "desktop backdrop movement must wake the OSD",
        );

        reset(&app, &wakes1, &toggles1, &closes1, &seeks1);
        let _ = press(&app, start);
        let _ = release(&app, start);
        fail(
            &failures1,
            app.get_osd_visible(),
            "desktop hidden empty-space tap must leave the OSD visible",
        );
        fail(
            &failures1,
            *toggles1.borrow() == 1,
            "desktop hidden empty-space tap must toggle pause once",
        );

        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "player OSD touch failures:\n  {}",
        failures.join("\n  ")
    );
}

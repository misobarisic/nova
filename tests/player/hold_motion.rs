//! An established speed hold owns pointer motion until release.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn advance(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
    slint::platform::update_timers_and_animations();
}

fn press(app: &nova::AppWindow, position: LogicalPosition) {
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
}

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
}

#[test]
fn held_speed_ignores_motion_and_restores_without_waking_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "held_speed_ignores_motion_and_restores_without_waking_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_position(100.0);
    app.set_is_paused(false);
    let wakes = Rc::new(Cell::new(0));
    let counter = wakes.clone();
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || {
        counter.set(counter.get() + 1);
        weak.upgrade().unwrap().set_osd_visible(true);
    });
    let previews = Rc::new(RefCell::new(Vec::new()));
    let values = previews.clone();
    app.on_playback_speed_preview(move |speed| values.borrow_mut().push(speed));
    let actions = Rc::new(Cell::new(0));
    let counter = actions.clone();
    app.on_toggle_pause(move || counter.set(counter.get() + 1));
    let counter = actions.clone();
    app.on_seek(move |_| counter.set(counter.get() + 1));
    let counter = actions.clone();
    app.on_android_brightness_step(move |_| counter.set(counter.get() + 1));
    let counter = actions.clone();
    app.on_android_volume_step(move |_| counter.set(counter.get() + 1));
    app.show().unwrap();
    advance(500);

    for android in [true, false] {
        app.set_is_android(android);
        for visible in [false, true] {
            for end in [
                LogicalPosition::new(580.0, 320.0),
                LogicalPosition::new(120.0, 600.0),
                LogicalPosition::new(300.0, 850.0),
            ] {
                app.set_osd_visible(visible);
                advance(250);
                // Force the deferred factory's geometry before publishing
                // the session rate, just as the rendered player is mounted
                // before the backend mirrors its effective playback speed.
                let backdrop =
                    ElementHandle::find_by_element_id(&app, "PlayerOverlay::player_backdrop")
                        .next()
                        .unwrap();
                let _ = (backdrop.size(), backdrop.absolute_position());
                app.set_effective_playback_speed(1.5);
                assert_eq!(
                    app.global::<nova::PlayerOsdBridge>().get_playback_speed(),
                    1.5
                );
                wakes.set(0);
                previews.borrow_mut().clear();
                let start = LogicalPosition::new(300.0, 300.0);
                press(&app, start);
                advance(600);
                assert_eq!(previews.borrow().as_slice(), [2.0]);
                for i in 1..=10 {
                    let fraction = i as f32 / 10.0;
                    app.window().dispatch_event(WindowEvent::PointerMoved {
                        position: LogicalPosition::new(
                            start.x + (end.x - start.x) * fraction,
                            start.y + (end.y - start.y) * fraction,
                        ),
                    });
                    advance(16);
                    assert_eq!(app.get_osd_visible(), visible);
                    assert_eq!(wakes.get(), 0, "a speed hold cannot become an OSD drag");
                }
                advance(900);
                let pill = ElementHandle::find_by_element_id(&app, "PlayerOverlay::flash_pill")
                    .next()
                    .unwrap();
                assert_eq!(pill.computed_opacity(), 1.0, "2× feedback stays while held");
                assert_eq!(
                    previews.borrow().as_slice(),
                    [2.0],
                    "motion retains the preview"
                );
                release(&app, end);
                advance(400);
                assert_eq!(previews.borrow().as_slice(), [2.0, 1.5]);
                assert_eq!(
                    app.get_effective_playback_speed(),
                    1.5,
                    "preview does not persist"
                );
                assert_eq!(
                    app.get_playback_speed(),
                    1.0,
                    "the global default is untouched"
                );
                assert_eq!(app.get_osd_visible(), visible);
                assert_eq!(wakes.get(), 0, "release cannot become a delayed single tap");
                assert_eq!(
                    actions.get(),
                    0,
                    "hold motion cannot seek, pause or adjust levels"
                );
            }
        }
    }
    // Normal hover still wakes desktop controls once the hold has ended.
    app.set_osd_visible(false);
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(400.0, 300.0),
    });
    assert!(app.get_osd_visible());
}

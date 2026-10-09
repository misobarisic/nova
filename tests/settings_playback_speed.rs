//! Playback-speed control (headless).
//!
//! Settings → Player and the player's own settings panel edit one per-device
//! rate through the shared `SpeedControl`. Guards that the control renders in
//! both hosts, that the ± buttons report a step to the backend (which owns the
//! 0.05 quantization — see the `nova-config` unit tests), that a press on the
//! slider reports a value that tracks the pointer, and that the readout shows
//! the shared value with two decimals. One test function: the testing backend
//! initializes once per process.

#[path = "support/settings.rs"]
mod settings_support;

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition};
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Tap: press + release at the same point (a touchscreen click).
fn tap(app: &nova::AppWindow, position: LogicalPosition) {
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        let _ = app.window().dispatch_event_with_result(event);
    }
}

/// Click `element`, then run `body` on the UI thread once the page has had a
/// layout/reveal tick to build whatever the click opened.
fn click_then(
    app: &nova::AppWindow,
    element: ElementHandle,
    ms: u64,
    body: impl FnOnce(nova::AppWindow) + 'static,
) {
    let weak = app.as_weak();
    slint::spawn_local(async move {
        if let Some(app) = weak.upgrade() {
            settings_support::click(&app, &element).await;
        }
        after(ms, move || {
            if let Some(app) = weak.upgrade() {
                body(app);
            }
        });
    })
    .unwrap();
}

fn open_player_settings(
    app: &nova::AppWindow,
    link: ElementHandle,
    body: impl FnOnce(nova::AppWindow) + 'static,
) {
    click_then(app, link, 400, move |app| {
        slint::spawn_local(async move {
            assert_eq!(app.get_settings_selected_id(), 5);
            assert!(app.get_settings_detail_open());
            // Element queries omit clipped descendants. The rate control now
            // follows the backend and episode-start rows on a phone, so reveal
            // it before checking its presets, readout and pointer actions.
            for _ in 0..20 {
                let control = ElementHandle::find_by_element_type_name(&app, "SpeedControl").next();
                if let Some(control) = control {
                    let center = control.absolute_position().y + control.size().height / 2.0;
                    app.set_settings_scroll_y(
                        app.get_settings_scroll_y() + app.window().size().height as f32 / 2.0
                            - center,
                    );
                    settings_support::settle().await;
                    body(app);
                    return;
                }
                app.set_settings_scroll_y(app.get_settings_scroll_y() - 100.0);
                settings_support::settle().await;
            }
            panic!("missing playback-speed control after scrolling Settings → Player");
        })
        .unwrap();
    });
}

#[test]
fn playback_speed_control_renders_and_reports_steps() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_playback_speed(1.0);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let steps: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let changes: Rc<RefCell<Vec<f32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let steps = steps.clone();
        app.on_playback_speed_stepped(move |d| steps.borrow_mut().push(d));
    }
    {
        let changes = changes.clone();
        app.on_playback_speed_changed(move |v| changes.borrow_mut().push(v));
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let steps1 = steps.clone();
    let changes1 = changes.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
            if !cond {
                failures.borrow_mut().push(msg.to_string());
            }
        };

        // ---- Settings → Player ----
        let Some(player_link) = ({
            app.set_settings_search_query("Player".into());
            destinations::find(&app, "settings:player").next()
        }) else {
            fail(&failures1, false, "DIAG: no Player landing entry");
            slint::quit_event_loop().unwrap();
            return;
        };
        let failures2 = failures1.clone();
        let steps2 = steps1.clone();
        let changes2 = changes1.clone();
        open_player_settings(&app, player_link, move |app| {
            let controls = ElementHandle::find_by_element_type_name(&app, "SpeedControl").count();
            fail(
                &failures2,
                controls == 1,
                "Settings → Player must show exactly one playback-speed control",
            );
            for preset in ["1×", "1.25×", "1.5×", "2×"] {
                fail(
                    &failures2,
                    ElementHandle::find_by_accessible_label(&app, preset)
                        .next()
                        .is_some(),
                    "the speed control must offer the 1× / 1.25× / 1.5× / 2× presets",
                );
            }
            // The readout reflects the shared value, two decimals.
            fail(
                &failures2,
                ElementHandle::find_by_accessible_label(&app, "1.00×")
                    .next()
                    .is_some(),
                "the speed readout must show the current rate (1.00×)",
            );
            let Some(faster) = ElementHandle::find_by_accessible_label(&app, "Faster").next()
            else {
                fail(
                    &failures2,
                    false,
                    "DIAG: no Faster button in Settings → Player",
                );
                slint::quit_event_loop().unwrap();
                return;
            };
            let failures3 = failures2.clone();
            let steps3 = steps2.clone();
            let changes3 = changes2.clone();
            click_then(&app, faster, 250, move |app| {
                fail(
                    &failures3,
                    steps3.borrow().as_slice() == [1],
                    "the + button must report a +1 step to the backend",
                );

                // ---- The player's own settings panel ----
                app.set_show_settings(false);
                app.set_player_open(true);
                app.set_playback_started(false);
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                let steps4 = steps3.clone();
                let changes4 = changes3.clone();
                after(300, move || {
                    let app = app4.upgrade().unwrap();
                    let Some(gear) =
                        ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
                            .next()
                    else {
                        fail(
                            &failures4,
                            false,
                            "DIAG: no settings gear in the player OSD",
                        );
                        slint::quit_event_loop().unwrap();
                        return;
                    };
                    let failures5 = failures4.clone();
                    let steps5 = steps4.clone();
                    let changes5 = changes4.clone();
                    click_then(&app, gear, 300, move |app| {
                        // Root page of the panel: the rate row opens its own
                        // submenu.
                        let Some(row) =
                            ElementHandle::find_by_accessible_label(&app, "Playback speed").next()
                        else {
                            fail(
                                &failures5,
                                false,
                                "the player settings panel must list Playback speed",
                            );
                            slint::quit_event_loop().unwrap();
                            return;
                        };
                        let failures6 = failures5.clone();
                        let steps6 = steps5.clone();
                        let changes6 = changes5.clone();
                        click_then(&app, row, 300, move |app| {
                            let controls =
                                ElementHandle::find_by_element_type_name(&app, "SpeedControl")
                                    .count();
                            fail(
                                &failures6,
                                controls == 1,
                                "the Playback speed submenu must show the shared control",
                            );
                            let Some(control) =
                                ElementHandle::find_by_element_type_name(&app, "SpeedControl")
                                    .next()
                            else {
                                slint::quit_event_loop().unwrap();
                                return;
                            };
                            // Tap two points inside the slider: the reported
                            // value must follow the pointer (and stay in
                            // range). The track starts after the − button and
                            // its gap (32 + 8) and ends before the + button,
                            // its gap and the readout (32 + 8 + 52 + gaps), so
                            // the two taps are placed a quarter and three
                            // quarters along that span — comfortably inside it
                            // even if the layout shifts by a few pixels.
                            let origin = control.absolute_position();
                            let size = control.size();
                            let track_left = origin.x + 40.0;
                            let track_width = (size.width - 140.0).max(1.0);
                            // The slider row is the top 32px of the control
                            // (the preset row sits under it, with a gap).
                            let y = origin.y + 16.0;
                            tap(
                                &app,
                                LogicalPosition::new(track_left + track_width * 0.25, y),
                            );
                            let low = changes6.borrow().last().copied();
                            tap(
                                &app,
                                LogicalPosition::new(track_left + track_width * 0.75, y),
                            );
                            let high = changes6.borrow().last().copied();
                            let in_range =
                                |v: Option<f32>| v.is_some_and(|v| (0.5..=2.0).contains(&v));
                            fail(
                                &failures6,
                                in_range(low) && in_range(high),
                                &format!(
                                    "slider taps must report values inside 0.5–2.0 (low {low:?}, high {high:?})"
                                ),
                            );
                            fail(
                                &failures6,
                                matches!((low, high), (Some(lo), Some(hi)) if hi > lo),
                                &format!(
                                    "a tap further along the slider must report a higher rate (low {low:?}, high {high:?})"
                                ),
                            );
                            let Some(preset) =
                                ElementHandle::find_by_accessible_label(&app, "2×").next()
                            else {
                                fail(&failures6, false, "DIAG: no 2× preset in the player panel");
                                slint::quit_event_loop().unwrap();
                                return;
                            };
                            let failures7 = failures6.clone();
                            let steps7 = steps6.clone();
                            let changes7 = changes6.clone();
                            click_then(&app, preset, 250, move |_app| {
                                fail(
                                    &failures7,
                                    changes7.borrow().last() == Some(&2.0),
                                    "a preset must apply its exact rate through the change path",
                                );
                                fail(
                                    &failures7,
                                    steps7.borrow().as_slice() == [1],
                                    "a preset must not be reported as a ± step",
                                );
                                slint::quit_event_loop().unwrap();
                            });
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
        "playback speed failures:\n  {}",
        failures.join("\n  ")
    );
}

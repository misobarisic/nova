//! The real UI-thread OSD timer follows the live paused-controls preference.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..4 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}
fn wait(app: &nova::AppWindow, ms: u64) {
    // Evaluate live binding changes before advancing the mock deadline.
    settle(app);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
    settle(app);
}
fn wake(app: &nova::AppWindow) {
    app.invoke_osd_mouse_moved();
    settle(app);
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}
fn setup() -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_position(120.0);
    app.set_duration(600.0);
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || {
        let app = weak.upgrade().unwrap();
        app.set_osd_visible(true);
        app.invoke_restart_osd_hide();
    });
    app.show().unwrap();
    settle(&app);
    app
}

#[test]
fn paused_controls_live_changes_and_input_use_fresh_hide_delays() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::paused_controls_live_changes_and_input_use_fresh_hide_delays"
    )) {
        return;
    }
    let app = setup();
    for android in [false, true] {
        app.set_is_android(android);
        app.set_keep_controls_visible_while_paused(false);
        app.set_is_paused(true);
        wake(&app);
        wait(&app, 4000);
        assert!(app.get_osd_visible());
        // Enabling during a countdown cancels it, even after its old deadline.
        app.set_keep_controls_visible_while_paused(true);
        wait(&app, 6000);
        assert!(app.get_osd_visible());
        app.set_keep_controls_visible_while_paused(false);
        settle(&app);
        wait(&app, 4000);
        assert!(app.get_osd_visible(), "disabling gets a fresh five seconds");
        wait(&app, 1100);
        assert!(!app.get_osd_visible());
        // Input reveals controls and rearms the same normal paused delay.
        key(&app, slint::platform::Key::Tab);
        assert!(app.get_osd_visible());
        wait(&app, 5100);
        assert!(
            !app.get_osd_visible(),
            "idle keyboard focus must not pin controls"
        );
        for enabled in [false, true] {
            app.set_keep_controls_visible_while_paused(enabled);
            app.set_is_paused(false);
            wake(&app);
            wait(&app, 3100);
            assert!(!app.get_osd_visible(), "playing always uses three seconds");
            // An expired timer and pause changes cannot wake hidden controls.
            app.set_is_paused(true);
            wait(&app, 6000);
            assert!(!app.get_osd_visible());
        }
        wake(&app);
        app.set_osd_visible(false); // explicit Back/tap dismissal
        app.set_keep_controls_visible_while_paused(false);
        app.set_keep_controls_visible_while_paused(true);
        wait(&app, 6000);
        assert!(
            !app.get_osd_visible(),
            "preference edits respect explicit dismissal"
        );
        wake(&app);
        app.set_is_paused(false);
        wait(&app, 3100);
        assert!(!app.get_osd_visible(), "resume restarts a held paused OSD");
    }
}

#[test]
fn paused_controls_protect_popups_scrubbing_and_loading_without_pinning_tv_focus() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::paused_controls_protect_popups_scrubbing_and_loading_without_pinning_tv_focus"
    )) {
        return;
    }
    let app = setup();
    app.set_keep_controls_visible_while_paused(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    wake(&app);
    key(&app, slint::platform::Key::RightArrow);
    wait(&app, 5100);
    assert!(
        !app.get_osd_visible(),
        "idle TV focus must time out while paused"
    );
    key(&app, slint::platform::Key::UpArrow);
    assert!(app.get_osd_visible(), "D-pad input reveals hidden controls");
    key(&app, slint::platform::Key::UpArrow);
    let played = ElementHandle::find_by_element_id(&app, "PlayerOverlay::played_track")
        .next()
        .unwrap();
    assert!(
        played.size().height >= 5.9,
        "TV Up selects the timeline before held seeking"
    );
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::RightArrow.into(),
    });
    settle(&app);
    assert!(
        app.global::<nova::PlayerOsdBridge>()
            .get_osd_auto_hide_blocked(),
        "held TV seek publishes its auto-hide protection"
    );
    wait(&app, 6100);
    assert!(app.get_osd_visible(), "held TV scrub owns the controls");
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::RightArrow.into(),
    });
    settle(&app);
    wait(&app, 5100);
    assert!(!app.get_osd_visible(), "release starts a fresh idle delay");

    app.global::<nova::InterfaceStyle>().set_tv(false);
    wake(&app);
    let seek = ElementHandle::find_by_element_id(&app, "PlayerOverlay::seek_touch")
        .next()
        .unwrap();
    let pos = seek.absolute_position();
    let size = seek.size();
    let position = slint::LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    wait(&app, 6100);
    assert!(app.get_osd_visible(), "held pointer scrub retains controls");
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    settle(&app);
    wait(&app, 5100);
    assert!(!app.get_osd_visible(), "pointer release resumes idle hide");
    wake(&app);
    ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    wait(&app, 6100);
    assert!(app.get_osd_visible(), "an open menu retains its controls");
    key(&app, slint::platform::Key::Escape);
    wait(&app, 4000);
    assert!(app.get_osd_visible());
    wait(&app, 1100);
    assert!(!app.get_osd_visible());

    app.set_playback_started(false);
    app.set_player_status("Loading…".into());
    wake(&app);
    wait(&app, 6100);
    assert!(
        app.get_osd_visible(),
        "loading must retain close and status feedback"
    );
    app.set_player_status("Playback failed".into());
    wait(&app, 6100);
    assert!(
        app.get_osd_visible(),
        "source errors retain close and status feedback"
    );
    app.set_playback_started(true);
    wait(&app, 5100);
    assert!(!app.get_osd_visible());

    // Destroy an open menu: its old hold must not pin the next player tree.
    wake(&app);
    ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    app.set_player_open(false);
    settle(&app);
    app.set_player_open(true);
    settle(&app);
    wake(&app);
    wait(&app, 5100);
    assert!(
        !app.get_osd_visible(),
        "a previous overlay cannot retain a menu hold"
    );
}

#[test]
fn paused_hidden_controls_recover_from_desktop_mouse_and_android_tap_without_resuming() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::paused_hidden_controls_recover_from_desktop_mouse_and_android_tap_without_resuming"
    )) {
        return;
    }
    let app = setup();
    app.set_keep_controls_visible_while_paused(false);
    let actions = Rc::new(Cell::new(0));
    let recorded = actions.clone();
    app.on_toggle_pause(move || recorded.set(recorded.get() + 1));
    let recorded = actions.clone();
    app.on_seek_relative(move |_| recorded.set(recorded.get() + 1));
    for android in [false, true] {
        app.set_is_android(android);
        wake(&app);
        wait(&app, 5100);
        assert!(
            !app.get_osd_visible(),
            "real paused delay must first expire"
        );
        if android {
            // Use bare press/release at the center, with no synthetic hover.
            // This is over the faded center Play button and must only reveal.
            let item = ElementHandle::find_by_element_id(&app, "PlayerOverlay::center_transport")
                .next()
                .unwrap();
            let pos = item.absolute_position();
            let size = item.size();
            let position =
                slint::LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        } else {
            app.window().dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(640.0, 220.0),
            });
        }
        settle(&app);
        assert!(
            app.get_osd_visible(),
            "normal input reveals paused controls"
        );
        assert!(app.get_is_paused());
        assert_eq!(
            actions.get(),
            0,
            "revealing must not activate hidden transport"
        );
        wait(&app, 5100);
        assert!(
            !app.get_osd_visible(),
            "revealed controls hide again after inactivity"
        );
        if android {
            let position = slint::LogicalPosition::new(640.0, 180.0);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            settle(&app);
            assert!(
                app.get_osd_visible(),
                "Android video tap reveals hidden controls"
            );
            assert!(app.get_is_paused());
            assert_eq!(
                actions.get(),
                0,
                "Android video tap must not resume or seek"
            );
        }
        if !android {
            let item = ElementHandle::find_by_element_id(&app, "PlayerOverlay::desktop_pause")
                .next()
                .unwrap();
            let pos = item.absolute_position();
            let size = item.size();
            let position =
                slint::LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            settle(&app);
            assert!(app.get_osd_visible(), "faded button click reveals controls");
            assert!(app.get_is_paused());
            assert_eq!(actions.get(), 0, "faded button click must not resume");
        }
    }
}

#[test]
fn visible_controls_activity_restarts_existing_playing_and_paused_deadlines() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::visible_controls_activity_restarts_existing_playing_and_paused_deadlines"
    )) {
        return;
    }
    let app = setup();
    app.set_is_android(false);
    app.set_keep_controls_visible_while_paused(false);
    for (paused, activity_interval) in [(false, 2000), (true, 4000)] {
        app.set_is_paused(paused);
        wake(&app);
        for x in [400.0, 500.0] {
            wait(&app, activity_interval);
            assert!(app.get_osd_visible());
            app.window().dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(x, 180.0),
            });
            settle(&app);
        }
        wait(&app, activity_interval);
        assert!(
            app.get_osd_visible(),
            "activity must renew an already-running hide timer past its old deadline"
        );
        wait(&app, 1100);
        assert!(
            !app.get_osd_visible(),
            "controls still hide after the last activity's full delay"
        );
        assert_eq!(
            app.get_is_paused(),
            paused,
            "pointer activity only controls the OSD"
        );
    }
}

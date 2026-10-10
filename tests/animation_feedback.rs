//! Bottom-sheet and player gesture-readout motion, including immediate
//! interaction, interrupted exits and independent animation switches.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn idle(app: &nova::AppWindow, ms: u64) {
    // Read geometry each frame: the headless backend has no renderer to
    // evaluate lazy animation bindings for us.
    for _ in 0..ms {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        for panel in ElementHandle::find_by_element_id(app, "MenuSheet::panel") {
            let _ = panel.absolute_position();
        }
        for pill in ElementHandle::find_by_element_id(app, "PlayerOverlay::flash_pill") {
            let _ = pill.absolute_position();
        }
    }
}

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
        app.window().dispatch_event(event);
    }
}

fn panel(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "MenuSheet::panel")
        .next()
        .expect("visible sheet panel")
}

fn sheet_count(app: &nova::AppWindow) -> usize {
    ElementHandle::find_by_element_type_name(app, "MenuSheet").count()
}

#[test]
fn sheet_and_player_feedback_respect_animation_switches() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_touch_menus(true);
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    app.set_stream_selector_open(true);
    app.set_stream_action_title("Stream options".into());
    app.set_stream_action_items(
        Rc::new(VecModel::from(vec![nova::SheetItem {
            label: "Download".into(),
            enabled: true,
        }]))
        .into(),
    );
    let actions = Rc::new(Cell::new(0));
    let counter = actions.clone();
    app.on_stream_action_selected(move |_, _| counter.set(counter.get() + 1));
    idle(&app, 300);
    assert_eq!(sheet_count(&app), 0, "closed sheets must be hidden");

    app.set_stream_action_open(true);
    idle(&app, 1);
    let start_y = panel(&app).absolute_position().y;
    idle(&app, 100);
    let midway_y = panel(&app).absolute_position().y;
    idle(&app, 200);
    let settled_y = panel(&app).absolute_position().y;
    assert!(
        start_y > midway_y && midway_y > settled_y,
        "sheet slides up: {start_y} -> {midway_y} -> {settled_y}"
    );
    assert!(settled_y + panel(&app).size().height <= 800.5);

    // Outside dismissal starts the exit but cannot trigger an action, even
    // when another tap lands on a row while the sheet is sliding away.
    tap(&app, LogicalPosition::new(180.0, 100.0));
    idle(&app, 1);
    assert!(!app.get_stream_action_open());
    assert_eq!(sheet_count(&app), 1, "exit layer must stay mounted");
    let row = ElementHandle::find_by_accessible_label(&app, "Download")
        .next()
        .unwrap();
    let pos = row.absolute_position();
    let size = row.size();
    tap(
        &app,
        LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0),
    );
    assert_eq!(actions.get(), 0, "closing rows must be inert");
    idle(&app, 100);
    assert!(
        panel(&app).absolute_position().y > settled_y,
        "sheet slides down"
    );

    // A new open cancels the pending exit; its old timer cannot hide it.
    app.set_stream_action_open(true);
    idle(&app, 1);
    idle(&app, 300);
    assert_eq!(sheet_count(&app), 1);
    assert!((panel(&app).absolute_position().y - settled_y).abs() < 0.5);
    app.set_stream_action_open(false);
    idle(&app, 1);
    idle(&app, 300);
    assert_eq!(sheet_count(&app), 0);

    for (master, transitions) in [(true, false), (false, true)] {
        let anim = app.global::<nova::Anim>();
        anim.set_enabled(master);
        anim.set_transitions(transitions);
        app.set_stream_action_open(true);
        idle(&app, 1);
        assert!((panel(&app).absolute_position().y - settled_y).abs() < 0.5);
        app.set_stream_action_open(false);
        idle(&app, 1);
        assert_eq!(
            sheet_count(&app),
            0,
            "disabled motion must not retain an exit"
        );
    }

    let anim = app.global::<nova::Anim>();
    anim.set_enabled(true);
    anim.set_transitions(true);
    app.set_stream_action_open(true);
    idle(&app, 1);
    idle(&app, 300);
    app.set_stream_action_open(false);
    idle(&app, 1);
    anim.set_enabled(false);
    idle(&app, 1);
    assert_eq!(
        sheet_count(&app),
        0,
        "disabling motion during exit hides at once"
    );

    // The gesture readout uses the player category, independently of page
    // transitions. Seeking still happens immediately, before its reveal.
    anim.set_enabled(true);
    anim.set_player(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_position(100.0);
    idle(&app, 300);
    let flash_y = || {
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::flash_pill")
            .next()
            .unwrap()
            .absolute_position()
            .y
    };
    assert!((flash_y() - 10.0).abs() < 0.5);
    let p = LogicalPosition::new(300.0, 300.0);
    tap(&app, p);
    tap(&app, p);
    assert_eq!(app.get_position(), 110.0);
    idle(&app, 1);
    idle(&app, 75);
    assert!(flash_y() > 10.0 && flash_y() < 16.0);
    idle(&app, 200);
    assert!((flash_y() - 16.0).abs() < 0.5);
    idle(&app, 800);
    assert!((flash_y() - 10.0).abs() < 0.5, "readout animates out");

    for (master, player) in [(true, false), (false, true)] {
        anim.set_enabled(master);
        anim.set_player(player);
        tap(&app, p);
        tap(&app, p);
        idle(&app, 1);
        assert!(
            (flash_y() - 16.0).abs() < 0.5,
            "disabled player motion snaps in"
        );
        idle(&app, 810);
        assert!(
            (flash_y() - 10.0).abs() < 0.5,
            "disabled player motion snaps out"
        );
    }
}

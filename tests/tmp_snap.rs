//! TEMPORARY probe (delete before finishing): hunt for rail position snaps.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::rc::Rc;
use std::time::Duration;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

/// Advance `n` frames of 16ms, so repeating timers (the fling) really tick.
fn frames(n: usize) {
    for _ in 0..n {
        idle(16);
    }
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
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn find_landing_card(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "ContinueCard")
        .collect::<Vec<_>>()
        .into_iter()
        .find(|c| {
            let p = c.absolute_position();
            let s = c.size();
            s.width > 80.0 && s.width < 160.0 && p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
        })
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(position.x + size.width / 2.0, position.y + size.height / 2.0)
}

fn setup() -> nova::AppWindow {
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(0);
    app.set_touch_menus(true);
    let cont: Vec<nova::ContinueRow> = (0..10)
        .map(|i| nova::ContinueRow {
            id: SharedString::from(format!("s{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E1 · Pilot"),
            poster: Default::default(),
            is_loaded: false,
            progress: 0.3,
            badge: 0,
        })
        .collect();
    app.set_home_continue(Rc::new(VecModel::from(cont)).into());
    let up: Vec<nova::UpcomingRow> = (0..10)
        .map(|i| nova::UpcomingRow {
            id: SharedString::from(format!("u{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E2 · Next"),
            date: SharedString::from("in 3 days"),
            poster: Default::default(),
            is_loaded: false,
            index: i,
        })
        .collect();
    app.set_home_upcoming(Rc::new(VecModel::from(up)).into());
    app.window().show().unwrap();
    idle(400);
    app
}

/// A flick with fine-grained sampling: prints the rail's movement around every
/// move event (before and after the frame's timers) and flags anything that
/// does not match the finger.
fn probe_flick(app: &nova::AppWindow, tag: &str, gap: u64, frames: usize, step: f32) {
    let from = center(&find_landing_card(app).expect("card"));
    let mut x = from.x;
    let mut rail = app.get_home_continue_x();
    press(app, from);
    idle(gap);
    for frame in 1..=frames {
        x += step;
        moved(app, LogicalPosition::new(x, from.y));
        let after_move = app.get_home_continue_x();
        idle(16);
        let after_frame = app.get_home_continue_x();
        let d_move = after_move - rail;
        let d_frame = after_frame - after_move;
        let expected = step;
        let flag = if (d_move - expected).abs() > 2.0 || (d_frame - expected).abs() > 2.0 {
            "  <-- SNAP?"
        } else {
            ""
        };
        eprintln!(
            "DIAG {tag} frame {frame}: move {d_move:+.1} then {d_frame:+.1} (finger {expected:+.1}) rail {after_frame:.1}{flag}"
        );
        rail = after_frame;
    }
    release(app, LogicalPosition::new(x, from.y));
    let at_release = app.get_home_continue_x();
    eprintln!("DIAG {tag}: release at {at_release:.1} (release delta {:.1})", at_release - rail);
    for frame in 0..4 {
        idle(16);
        let now = app.get_home_continue_x();
        eprintln!("DIAG {tag} coast {frame}: {:+.1}", now - at_release);
        rail = now;
    }
}

fn key(app: &nova::AppWindow, k: slint::platform::Key) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
            text: k.into(),
        });
}

#[test]
fn probe_rail_snaps() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = setup();

    // Keyboard use: the rail must still follow the focused card.
    eprintln!(
        "DIAG before keys: zone {} idx {} rail {:.1}",
        app.get_home_kb_zone(),
        app.get_home_kb_idx(),
        app.get_home_continue_x()
    );
    key(&app, slint::platform::Key::DownArrow);
    frames(2);
    eprintln!(
        "DIAG after Down: zone {} idx {} rail {:.1}",
        app.get_home_kb_zone(),
        app.get_home_kb_idx(),
        app.get_home_continue_x()
    );
    for _ in 0..6 {
        key(&app, slint::platform::Key::RightArrow);
        frames(2);
    }
    let followed = app.get_home_continue_x();
    eprintln!(
        "DIAG after 6 RightArrow: zone {} idx {} rail {followed:.1}",
        app.get_home_kb_zone(),
        app.get_home_kb_idx()
    );

    // A pointer drag ends the modality: a flick afterwards must not be undone
    // by later focus follows.
    let from = center(&find_landing_card(&app).expect("card"));
    press(&app, from);
    idle(120);
    let mut x = from.x;
    for _ in 1..=6 {
        x += -30.0;
        moved(&app, LogicalPosition::new(x, from.y));
        idle(16);
    }
    release(&app, LogicalPosition::new(x, from.y));
    frames(60);
    let flicked = app.get_home_continue_x();
    eprintln!("DIAG settled after the flick: rail {flicked:.1}");
    app.set_home_view(1);
    frames(10);
    app.set_home_view(0);
    frames(10);
    eprintln!("DIAG after a subpage round trip: rail {:.1} (was {flicked:.1})", app.get_home_continue_x());
}

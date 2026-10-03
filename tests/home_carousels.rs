//! Home carousels + "see all" subpage (headless).
//!
//! The landing renders Continue Watching / Upcoming as horizontal
//! carousels; tapping a section header slides in the full vertical subpage.
//! Covers that the carousels render with real rows, that the header tap
//! flips `home_view`, and that the subpage grid is actually scrollable (the
//! ScrollView needs a direct layout child or its content-height never
//! derives and it cannot scroll).

use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::Cell;
use std::rc::Rc;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(ms));
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

fn center(element: &i_slint_backend_testing::ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

/// The first landing carousel card currently on screen. Landing cards are the
/// narrow (~2.5-across) variant; subpage grid cards are wider and are not
/// instantiated while `home_view == 0` anyway.
fn find_landing_card(app: &nova::AppWindow) -> Option<i_slint_backend_testing::ElementHandle> {
    let cards: Vec<_> =
        i_slint_backend_testing::ElementHandle::find_by_element_type_name(app, "ContinueCard")
            .collect();
    cards.into_iter().find(|c| {
        let p = c.absolute_position();
        let s = c.size();
        s.width > 80.0 && s.width < 160.0 && p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
    })
}

#[test]
fn carousels_render_headers_open_subpages_and_subpage_scrolls() {
    i_slint_backend_testing::init_integration_test_with_mock_time();

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
            ..Default::default()
        })
        .collect();
    let up: Vec<nova::UpcomingRow> = (0..10)
        .map(|i| nova::UpcomingRow {
            id: SharedString::from(format!("u{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E2 · Next"),
            date: SharedString::from("in 3 days"),
            poster: Default::default(),
            is_loaded: false,
            index: i,
            ..Default::default()
        })
        .collect();
    app.set_home_continue(Rc::new(VecModel::from(cont)).into());
    app.set_home_upcoming(Rc::new(VecModel::from(up)).into());

    // Count card picks. The test drives AppWindow directly, so wire the
    // generated callbacks instead of the real app's detail-opening path.
    let picks = Rc::new(Cell::new(0));
    {
        let picks = picks.clone();
        app.on_continue_picked(move |_| picks.set(picks.get() + 1));
    }
    {
        let picks = picks.clone();
        app.on_upcoming_picked(move |_| picks.set(picks.get() + 1));
    }

    idle(400);
    let card = find_landing_card(&app).expect("visible landing carousel card");
    let c = center(&card);
    press(&app, c);
    idle(120);
    // Native inertia samples elapsed time; a burst of moves in one tick
    // is a position change, not a measurable flick.
    for dx in [10.0, 30.0, 60.0, 90.0, 120.0] {
        moved(&app, LogicalPosition::new(c.x - dx, c.y));
        idle(20);
    }
    release(&app, LogicalPosition::new(c.x - 120.0, c.y));
    let at_release = app.get_home_continue_x();
    assert!(at_release < 0.0, "dragging must scroll the carousel");
    assert_eq!(picks.get(), 0, "a horizontal drag must not open a card");
    idle(150);
    assert!(
        app.get_home_continue_x() < at_release,
        "a released flick must coast on"
    );

    let c = center(&find_landing_card(&app).expect("card for tap phase"));
    press(&app, c);
    release(&app, c);
    assert_eq!(picks.get(), 1, "a tap must open the card exactly once");

    let header =
        i_slint_backend_testing::ElementHandle::find_by_element_type_name(&app, "SectionHeader")
            .next()
            .expect("Continue header");
    let c = center(&header);
    press(&app, c);
    release(&app, c);
    idle(400);
    assert_eq!(app.get_home_view(), 1, "header must open its subpage");

    let sub =
        i_slint_backend_testing::ElementHandle::find_by_element_type_name(&app, "ContinueCard")
            .find(|c| {
                let p = c.absolute_position();
                c.size().width > 140.0 && p.x >= 18.0 && p.y > 60.0 && p.y < 600.0
            })
            .expect("visible subpage card");
    let c = center(&sub);
    press(&app, c);
    idle(120);
    for dy in [10.0, 30.0, 60.0, 90.0, 120.0] {
        moved(&app, LogicalPosition::new(c.x, c.y - dy));
        idle(20);
    }
    release(&app, LogicalPosition::new(c.x, c.y - 120.0));
    assert!(
        app.get_home_all_scroll_y() < 0.0,
        "subpage grid must scroll vertically"
    );
}

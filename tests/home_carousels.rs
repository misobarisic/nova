//! Home carousels + "see all" subpage (headless).
//!
//! The landing renders Continue Watching / Upcoming as horizontal
//! carousels; tapping a section header slides in the full vertical subpage.
//! Covers that the carousels render with real rows, that the header tap
//! flips `home_view`, and that the subpage grid is actually scrollable (the
//! ScrollView needs a direct layout child or its content-height never
//! derives and it cannot scroll).

use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
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
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(0);

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
    app.set_home_continue(Rc::new(VecModel::from(cont)).into());
    app.set_home_upcoming(Rc::new(VecModel::from(up)).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    // Count card picks. The test drives AppWindow directly, so wire the
    // generated callbacks instead of the real app's detail-opening path.
    let picks: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let picks = picks.clone();
        app.on_continue_picked(move |i| picks.borrow_mut().push(format!("c{i}")));
    }
    {
        let picks = picks.clone();
        app.on_upcoming_picked(move |i| picks.borrow_mut().push(format!("u{i}")));
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let picks1 = picks.clone();
    after(300, move || {
        let app = app1.upgrade().unwrap();

        // ---- Landing: a horizontal drag scrolls, does not open ----
        let Some(card) = find_landing_card(&app) else {
            fail(&failures1, false, "DIAG: no visible landing carousel card");
            slint::quit_event_loop().unwrap();
            return;
        };
        let c = center(&card);
        press(&app, c);
        for dx in [10.0, 30.0, 60.0, 90.0, 120.0] {
            moved(&app, LogicalPosition::new(c.x - dx, c.y));
        }
        release(&app, LogicalPosition::new(c.x - 120.0, c.y));
        fail(
            &failures1,
            app.get_home_continue_x() < 0.0,
            "dragging a landing card must scroll the carousel",
        );
        fail(
            &failures1,
            picks1.borrow().is_empty(),
            "a horizontal drag must not open a card",
        );
        // The rail is driven by hand (no Flickable momentum), so a flick must
        // coast on by itself after the release.
        let at_release = app.get_home_continue_x();

        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let picks2 = picks1.clone();
        after(150, move || {
            let app = app2.upgrade().unwrap();
            fail(
                &failures2,
                app.get_home_continue_x() < at_release,
                "a released flick must coast on (touch inertia)",
            );

            // ---- Landing: a stationary tap opens the card ----
            match find_landing_card(&app) {
                Some(card) => {
                    let c = center(&card);
                    press(&app, c);
                    release(&app, c);
                }
                None => fail(&failures2, false, "DIAG: no card for the tap phase"),
            }
            fail(
                &failures2,
                picks2.borrow().len() == 1,
                "a tap on a landing card must open it exactly once",
            );

            // ---- Header opens the subpage (existing behaviour) ----
            let failures3 = failures2.clone();
            slint::spawn_local(async move {
                let Some(header) =
                    i_slint_backend_testing::ElementHandle::find_by_element_type_name(
                        &app,
                        "SectionHeader",
                    )
                    .next()
                else {
                    fail(&failures3, false, "DIAG: no SectionHeader found");
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                slint::spawn_local(async move {
                    header
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        fail(
                            &failures4,
                            app.get_home_view() == 1,
                            "tapping the Continue header must open its subpage",
                        );

                        // The subpage grid must scroll: drag a visible grid card up.
                        let cards: Vec<_> =
                            i_slint_backend_testing::ElementHandle::find_by_element_type_name(
                                &app,
                                "ContinueCard",
                            )
                            .collect();
                        let sub = cards.iter().find(|c| {
                            let p = c.absolute_position();
                            // Grid cards are wider than carousel cards; on-screen.
                            c.size().width > 140.0 && p.x >= 20.0 && p.y > 60.0 && p.y < 600.0
                        });
                        match sub {
                            Some(card) => {
                                let c = center(card);
                                press(&app, c);
                                for dy in [10.0, 30.0, 60.0, 90.0, 120.0] {
                                    moved(&app, LogicalPosition::new(c.x, c.y - dy));
                                }
                                release(&app, LogicalPosition::new(c.x, c.y - 120.0));
                                fail(
                                    &failures4,
                                    app.get_home_all_scroll_y() != 0.0,
                                    "subpage grid must scroll vertically",
                                );
                            }
                            None => fail(&failures4, false, "DIAG: no visible subpage card"),
                        }
                        slint::quit_event_loop().unwrap();
                    });
                })
                .unwrap();
            })
            .unwrap();
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "home carousel failures:\n  {}",
        failures.join("\n  ")
    );
}

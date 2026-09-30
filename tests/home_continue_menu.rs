//! Home → Continue Watching card menu (headless).
//!
//! Guards the Continue Watching card menu, which mirrors a My Library series
//! card's popup: with touch menus enabled a hold — or a right-click, as on the
//! Library cards — opens the page-level sheet, offering the Library actions
//! (mark watched/unwatched, On Hold, Dropped, back to automatic, remove from
//! library) next to Play and "Remove from Continue Watching", each dispatching
//! the matching callback for the card index. The same sheet opens from a hold
//! on the "see all" subpage grid. On pointer platforms (touch menus off, i.e.
//! desktop) the hold timer stays unarmed and a right-click opens the native
//! popup instead — the headless backend can't host that popup, so the guard
//! there is that the mobile sheet is *not* used. One test function: the testing
//! backend initializes once per process.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

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

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
}

/// A full right-click (press + release): the desktop card-menu trigger.
fn right_click(app: &nova::AppWindow, position: LogicalPosition) {
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Right,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Right,
        },
    ] {
        let _ = app.window().dispatch_event_with_result(event);
    }
}

/// Synthetic system-back: Home pops the open card sheet with it.
fn back(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

/// The first landing carousel card currently on screen (the narrow
/// ~2.5-across variant; the subpage grid cards are wider and are not
/// instantiated while `home_view == 0` anyway).
fn landing_card(app: &nova::AppWindow) -> Option<ElementHandle> {
    let cards: Vec<_> = ElementHandle::find_by_element_type_name(app, "ContinueCard").collect();
    cards.into_iter().find(|c| {
        let p = c.absolute_position();
        let sz = c.size();
        sz.width > 80.0 && sz.width < 160.0 && p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
    })
}

/// The first subpage grid card currently on screen (wider than the carousel
/// variant).
fn subpage_card(app: &nova::AppWindow) -> Option<ElementHandle> {
    let cards: Vec<_> = ElementHandle::find_by_element_type_name(app, "ContinueCard").collect();
    cards.into_iter().find(|c| {
        let p = c.absolute_position();
        c.size().width > 140.0 && p.x >= 10.0 && p.y > 40.0 && p.y < 700.0
    })
}

#[test]
fn continue_menu_matches_the_library_card_menu() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(0);
    // Touch surfaces open the page-level sheet on hold; desktop takes the
    // card-level native menus, which the headless backend cannot host.
    app.set_touch_menus(true);
    let rows: Vec<nova::ContinueRow> = (0..6)
        .map(|i| nova::ContinueRow {
            id: s(format!("s{i}").as_str()),
            title: s(format!("Show {i}").as_str()),
            subtitle: s("S1 E1 · Pilot"),
            poster: Default::default(),
            is_loaded: false,
            progress: 0.3,
            badge: 0,
        })
        .collect();
    app.set_home_continue(Rc::new(VecModel::from(rows)).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };
    let picks: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let removes: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let enters: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let picks = picks.clone();
        app.on_continue_picked(move |i| picks.borrow_mut().push(i));
    }
    {
        let removes = removes.clone();
        app.on_continue_remove(move |i| removes.borrow_mut().push(i));
    }
    {
        let enters = enters.clone();
        app.on_continue_enter(move |i| enters.borrow_mut().push(i));
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let picks1 = picks.clone();
    let removes1 = removes.clone();
    // The window-level "Enter series" callback forwards to the handler (the
    // sheet's own row dispatch is covered by the Remove click below).
    app.invoke_continue_enter(0);
    assert_eq!(enters.borrow().as_slice(), [0]);
    after(300, move || {
        let app = app1.upgrade().unwrap();
        // ---- Landing: hold the first card to open the sheet ----
        let Some(card) = landing_card(&app) else {
            fail(&failures1, false, "DIAG: no visible landing carousel card");
            slint::quit_event_loop().unwrap();
            return;
        };
        let c = center(&card);
        press(&app, c);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let picks2 = picks1.clone();
        let removes2 = removes1.clone();
        // The card-menu hold timer is 600ms; hold well past it, then release
        // and check the sheet (the release must not also open the card).
        after(800, move || {
            let app = app2.upgrade().unwrap();
            release(&app, c);

            let sheets: Vec<_> =
                ElementHandle::find_by_element_type_name(&app, "MenuSheet").collect();
            fail(
                &failures2,
                !sheets.is_empty(),
                "holding a landing card must open the menu sheet",
            );
            // The sheet is an overlay, not a layout child: it must cover the
            // whole page rather than push the landing content up below the nav.
            let sheet = &sheets[0];
            fail(
                &failures2,
                sheet.absolute_position().y <= 1.0,
                "the card menu sheet must overlay the page from the top",
            );
            fail(
                &failures2,
                sheet.size().height >= app.window().size().height as f32 - 1.0,
                "the card menu sheet must cover the whole window",
            );
            fail(
                &failures2,
                picks2.borrow().is_empty(),
                "a hold must not also open the card",
            );
            let Some(remove_row) =
                ElementHandle::find_by_accessible_label(&app, "Remove from Continue Watching")
                    .next()
            else {
                fail(
                    &failures2,
                    false,
                    "menu sheet must offer a Remove from Continue Watching row",
                );
                slint::quit_event_loop().unwrap();
                return;
            };
            fail(
                &failures2,
                ElementHandle::find_by_accessible_label(&app, "Play")
                    .next()
                    .is_some(),
                "menu sheet must offer a Play row",
            );
            // Exactly the Continue Watching actions: Play, Enter series,
            // Remove from Continue Watching (+ the sheet's own Cancel) — and
            // none of the My Library card actions.
            for label in ["Play", "Enter series"] {
                fail(
                    &failures2,
                    ElementHandle::find_by_accessible_label(&app, label)
                        .next()
                        .is_some(),
                    "the Continue Watching menu must offer Play and Enter series",
                );
            }
            for label in [
                "Mark series as watched",
                "Mark as On Hold",
                "Mark as Dropped",
                "Back to automatic",
                "Remove from library",
            ] {
                fail(
                    &failures2,
                    ElementHandle::find_by_accessible_label(&app, label)
                        .next()
                        .is_none(),
                    "the Continue Watching menu must not offer My Library card actions",
                );
            }

            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            let picks3 = picks2.clone();
            let removes3 = removes2.clone();
            slint::spawn_local(async move {
                remove_row
                    .single_click(slint::platform::PointerEventButton::Left)
                    .await;
                after(300, move || {
                    let app = app3.upgrade().unwrap();
                    fail(
                        &failures3,
                        removes3.borrow().as_slice() == [0],
                        "sheet Remove must dispatch continue_remove(0)",
                    );
                    fail(
                        &failures3,
                        picks3.borrow().is_empty(),
                        "the sheet must not also play the item",
                    );

                    // ---- Subpage: hold a grid card to open the sheet ----
                    let Some(header) =
                        ElementHandle::find_by_element_type_name(&app, "SectionHeader").next()
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
                            let Some(card) = subpage_card(&app) else {
                                fail(&failures4, false, "DIAG: no visible subpage card");
                                slint::quit_event_loop().unwrap();
                                return;
                            };
                            let c = center(&card);
                            press(&app, c);
                            let app5 = app.as_weak();
                            let failures5 = failures4.clone();
                            let picks5 = picks3.clone();
                            let removes5 = removes3.clone();
                            after(800, move || {
                                let app = app5.upgrade().unwrap();
                                release(&app, c);
                                let sheets: Vec<_> = ElementHandle::find_by_element_type_name(
                                    &app,
                                    "MenuSheet",
                                )
                                .collect();
                                fail(
                                    &failures5,
                                    !sheets.is_empty(),
                                    "holding a subpage card must open the menu sheet",
                                );

                                // ---- Touch: right-click opens the same sheet ----
                                // Library cards open the sheet from both a hold
                                // and a mouse right-click on Android; the
                                // Continue cards must do the same.
                                back(&app); // close the held sheet
                                right_click(&app, c);
                                let app8 = app.as_weak();
                                let failures8 = failures5.clone();
                                after(200, move || {
                                    let app = app8.upgrade().unwrap();
                                    let sheets: Vec<_> =
                                        ElementHandle::find_by_element_type_name(&app, "MenuSheet")
                                            .collect();
                                    fail(
                                        &failures8,
                                        !sheets.is_empty(),
                                        "a touch right-click must open the same card menu sheet",
                                    );

                                    // ---- Desktop: the mobile sheet must not be used ----
                                    // Pointer platforms open the native popup at the
                                    // cursor, exactly like the Library/Detail card
                                    // menus. The headless backend can't host that
                                    // popup, so the guard is that the bottom sheet
                                    // — the mobile UI — stays closed, the rail is
                                    // not panned and nothing is played or removed.
                                    back(&app); // page-level Back closes the sheet
                                    app.set_touch_menus(false);
                                    app.set_home_view(0);
                                    let app6 = app.as_weak();
                                    let failures6 = failures8.clone();
                                    let picks6 = picks5.clone();
                                    let removes6 = removes5.clone();
                                    // Re-layout tick before locating a landing card.
                                    after(300, move || {
                                        let app = app6.upgrade().unwrap();
                                        let picks_before = picks6.borrow().len();
                                        let removes_before = removes6.borrow().len();
                                        let Some(card) = landing_card(&app) else {
                                            fail(
                                                &failures6,
                                                false,
                                                "DIAG: no visible landing card (desktop phase)",
                                            );
                                            slint::quit_event_loop().unwrap();
                                            return;
                                        };
                                        right_click(&app, center(&card));
                                        let app7 = app.as_weak();
                                        let failures7 = failures6.clone();
                                        let picks7 = picks6.clone();
                                        let removes7 = removes6.clone();
                                        after(300, move || {
                                            let app = app7.upgrade().unwrap();
                                            let sheets: Vec<_> = ElementHandle::find_by_element_type_name(
                                                &app,
                                                "MenuSheet",
                                            )
                                            .collect();
                                            fail(
                                                &failures7,
                                                sheets.is_empty(),
                                                "a desktop right-click must not open the mobile bottom sheet",
                                            );
                                            fail(
                                                &failures7,
                                                app.get_home_view() == 0,
                                                "a desktop right-click must not open the card",
                                            );
                                            fail(
                                                &failures7,
                                                picks7.borrow().len() == picks_before,
                                                "a desktop right-click must not play the card",
                                            );
                                            fail(
                                                &failures7,
                                                removes7.borrow().len() == removes_before,
                                                "a desktop right-click must not remove the card",
                                            );
                                            slint::quit_event_loop().unwrap();
                                        });
                                    });
                                });
                            });
                        });
                    })
                    .unwrap();
                });
            })
            .unwrap();
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "continue menu failures:\n  {}",
        failures.join("\n  ")
    );
}

//! Home → Upcoming calendar (headless).
//!
//! The Upcoming "see all" subpage toggles between the episode grid and a
//! month calendar: 42 day cells with episode counts plus the selected day's
//! episodes underneath. Covers that the toggle opens/closes the calendar,
//! that tapping a marked day reports its epoch (not the cell position), that
//! tapping a selected-day card resolves through the row's full-list index,
//! that the month step and keyboard paths reach the backend, and that system
//! back closes the calendar before leaving the subpage.

use i_slint_backend_testing::ElementHandle;
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

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
}

fn tap(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let s = element.size();
    let c = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    press(app, c);
    release(app, c);
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
}

#[test]
fn upcoming_calendar_toggles_picks_days_and_resolves_indices() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(2);

    // Two marked days: the selected one (two episodes) and another (one).
    const E: i32 = 20454;
    const E2: i32 = 20461;

    let up: Vec<nova::UpcomingRow> = (0..3)
        .map(|i| nova::UpcomingRow {
            id: SharedString::from(format!("s{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E4 · Soon"),
            date: SharedString::from("in 3 days"),
            poster: Default::default(),
            is_loaded: false,
            index: i,
        })
        .collect();
    app.set_home_upcoming(Rc::new(VecModel::from(up)).into());

    let cells: Vec<nova::CalCell> = (0..42)
        .map(|i| nova::CalCell {
            epoch: E - 16 + i,
            day: (i % 30) + 1,
            in_month: (2..40).contains(&i),
            count: if E - 16 + i == E {
                2
            } else if E - 16 + i == E2 {
                1
            } else {
                0
            },
            selected: E - 16 + i == E,
            today: false,
        })
        .collect();
    app.set_home_cal_cells(Rc::new(VecModel::from(cells)).into());
    app.set_home_cal_title(SharedString::from("September 2026"));

    // Selected-day rows carry their full-list indices (1, 2): tapping the
    // first one must resolve to index 1, not day-list position 0.
    let day: Vec<nova::UpcomingRow> = [1, 2]
        .iter()
        .map(|i| nova::UpcomingRow {
            id: SharedString::from(format!("s{i}")),
            title: SharedString::from(format!("Show {i}")),
            subtitle: SharedString::from("S1 E4 · Soon"),
            date: SharedString::from("in 3 days"),
            poster: Default::default(),
            is_loaded: false,
            index: *i,
        })
        .collect();
    app.set_home_cal_day(Rc::new(VecModel::from(day)).into());
    app.set_home_cal_epoch(E);
    app.set_home_cal_open(false);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    // Stub backend: mimic run.rs by flipping the mirrored props.
    let cal_open_calls: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let picks: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let months: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let activates: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    {
        let app_w = app.as_weak();
        let cal_open_calls = cal_open_calls.clone();
        app.on_upcoming_cal_open(move || {
            cal_open_calls.borrow_mut().push(true);
            app_w.upgrade().unwrap().set_home_cal_open(true);
        });
    }
    {
        let app_w = app.as_weak();
        let cal_open_calls = cal_open_calls.clone();
        app.on_upcoming_cal_close(move || {
            cal_open_calls.borrow_mut().push(false);
            app_w.upgrade().unwrap().set_home_cal_open(false);
        });
    }
    {
        let app_w = app.as_weak();
        let picks = picks.clone();
        app.on_upcoming_cal_pick(move |e| {
            picks.borrow_mut().push(e);
            app_w.upgrade().unwrap().set_home_cal_epoch(e);
        });
    }
    {
        let months = months.clone();
        app.on_upcoming_cal_month(move |d| months.borrow_mut().push(d));
    }
    {
        let opened = opened.clone();
        app.on_upcoming_picked(move |i| opened.borrow_mut().push(format!("u{i}")));
    }
    {
        let activates = activates.clone();
        app.on_upcoming_cal_activate(move || *activates.borrow_mut() += 1);
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let (cal_open_calls1, picks1, months1, opened1, activates1) = (
        cal_open_calls.clone(),
        picks.clone(),
        months.clone(),
        opened.clone(),
        activates.clone(),
    );
    after(300, move || {
        let app = app1.upgrade().unwrap();

        // ---- Grid mode: episode grid up, no calendar cells ----
        // (The landing carousel cards are still instantiated behind the
        // opaque subpage, so only the wide grid cards count.)
        let grid_cards = ElementHandle::find_by_element_type_name(&app, "UpcomingCard")
            .filter(|c| c.size().width > 140.0)
            .count();
        fail(
            &failures1,
            grid_cards == 3,
            &format!("grid mode must render 3 cards, found {grid_cards}"),
        );
        let cells = ElementHandle::find_by_element_type_name(&app, "CalDayCell").count();
        fail(
            &failures1,
            cells == 0,
            &format!("grid mode must render no calendar cells, found {cells}"),
        );

        // ---- The toggle opens the calendar ----
        let Some(toggle) = ElementHandle::find_by_element_type_name(&app, "CalToggle").next()
        else {
            fail(&failures1, false, "DIAG: no CalToggle found");
            slint::quit_event_loop().unwrap();
            return;
        };
        tap(&app, &toggle);

        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(300, move || {
            let app = app2.upgrade().unwrap();
            fail(
                &failures2,
                cal_open_calls1.borrow().as_slice() == [true],
                "tapping the toggle must call upcoming_cal_open",
            );
            fail(
                &failures2,
                app.get_home_cal_open(),
                "calendar must be open after the toggle",
            );

            // ---- Calendar mode: 42 cells, grid cards gone, day cards up ----
            let cells: Vec<_> =
                ElementHandle::find_by_element_type_name(&app, "CalDayCell").collect();
            fail(
                &failures2,
                cells.len() == 42,
                &format!("calendar must render 42 cells, found {}", cells.len()),
            );
            // No dead gap above the dates: the first date row sits right
            // below the month/weekday bands (the column packs from the top
            // instead of sharing the viewport leftover between bands).
            let first_y = cells
                .iter()
                .map(|c| c.absolute_position().y)
                .fold(f32::MAX, f32::min);
            let weekday = ElementHandle::find_by_accessible_label(&app, "Mon")
                .next()
                .expect("weekday label");
            let weekday_bottom = weekday.absolute_position().y + weekday.size().height;
            fail(
                &failures2,
                first_y >= weekday_bottom && first_y - weekday_bottom <= 16.0,
                &format!(
                    "first date row too far below weekdays ({weekday_bottom:.1} -> {first_y:.1}) — slack distributed into the bands"
                ),
            );
            let cards = ElementHandle::find_by_element_type_name(&app, "UpcomingCard")
                .filter(|c| c.size().width > 140.0)
                .count();
            fail(
                &failures2,
                cards == 2,
                &format!("calendar day list must render 2 cards, found {cards}"),
            );

            // Tapping every cell reports exactly the marked epochs (filler
            // and empty days stay silent, nothing double-fires) and must not
            // open any card through the covered landing layer underneath.
            for cell in &cells {
                tap(&app, cell);
            }
            let mut got = picks1.borrow().clone();
            got.sort_unstable();
            fail(
                &failures2,
                got.as_slice() == [E, E2],
                &format!("day taps must report the marked epochs, got {got:?}"),
            );
            fail(
                &failures2,
                opened1.borrow().is_empty(),
                &format!("cell taps must not open cards, got {:?}", opened1.borrow()),
            );

            // The first day card resolves through its full-list index.
            let day_cards: Vec<_> = ElementHandle::find_by_element_type_name(&app, "UpcomingCard")
                .filter(|c| c.size().width > 140.0)
                .collect();
            match day_cards.first() {
                Some(card) => tap(&app, card),
                None => fail(&failures2, false, "DIAG: no day card to tap"),
            }
            fail(
                &failures2,
                opened1.borrow().as_slice() == ["u1"],
                &format!(
                    "day card must open full-list index 1, got {:?}",
                    opened1.borrow()
                ),
            );

            // Month step + keyboard paths reach the backend.
            app.invoke_upcoming_cal_month(1);
            fail(
                &failures2,
                months1.borrow().as_slice() == [1],
                "month step must reach upcoming_cal_month",
            );
            app.set_home_cal_epoch(E);
            key(&app, slint::platform::Key::LeftArrow);
            fail(
                &failures2,
                picks1.borrow().last() == Some(&(E - 1)),
                "Left must pick the previous day",
            );
            key(&app, slint::platform::Key::Return);
            fail(
                &failures2,
                *activates1.borrow() == 1,
                "Enter must activate the selected day",
            );

            // System back closes the calendar before leaving the subpage.
            key(&app, slint::platform::Key::Back);
            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(200, move || {
                let app = app3.upgrade().unwrap();
                fail(
                    &failures3,
                    cal_open_calls1.borrow().as_slice() == [true, false],
                    "Back must close the calendar first",
                );
                fail(
                    &failures3,
                    app.get_home_view() == 2,
                    "Back must not leave the subpage while closing the calendar",
                );
                fail(
                    &failures3,
                    !app.get_home_cal_open(),
                    "calendar must be closed after Back",
                );
                slint::quit_event_loop().unwrap();
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "upcoming calendar failures:\n  {}",
        failures.join("\n  ")
    );
}

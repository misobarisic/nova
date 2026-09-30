//! Navigation selection marker (headless): the accent circle must sit centred
//! on the item it marks — in the wide rail *and* in the narrow bottom bar —
//! must land on the item that was picked (the bar is rebuilt on every switch,
//! with an instantly positioned highlight), and must not drift when something
//! else rebuilds the page (detail modal, player, subpages).
//!
//! Regression: the rail's shared marker was positioned from the item's top
//! edge instead of its centre, so the circle sat 4px high on every item.
//! One test function: the testing backend initializes once per process, so the
//! phases run sequentially on one window.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Centres of the elements of exactly `w`x`h` that satisfy `keep`.
fn centres(
    app: &nova::AppWindow,
    w: f32,
    h: f32,
    keep: impl Fn(f32, f32) -> bool,
) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let s = e.size();
            if (s.width - w).abs() > 0.5 || (s.height - h).abs() > 0.5 {
                return None;
            }
            let p = e.absolute_position();
            let (cx, cy) = (p.x + s.width / 2.0, p.y + s.height / 2.0);
            keep(cx, cy).then_some((cx, cy))
        })
        .collect();
    out.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then(a.1.partial_cmp(&b.1).unwrap())
    });
    out
}

/// The rail lives in the left 64px column; the bottom bar in the last 100px.
const RAIL: fn(f32, f32) -> bool = |cx, cy| cx < 70.0 && cy < 340.0;

fn rail_icons(app: &nova::AppWindow) -> Vec<(f32, f32)> {
    centres(app, 24.0, 24.0, RAIL)
}

fn rail_dots(app: &nova::AppWindow) -> Vec<(f32, f32)> {
    centres(app, 48.0, 48.0, RAIL)
}

fn bar_icons(app: &nova::AppWindow, height: f32) -> Vec<(f32, f32)> {
    centres(app, 24.0, 24.0, move |_, cy| cy > height - 100.0)
}

fn bar_dots(app: &nova::AppWindow, height: f32) -> Vec<(f32, f32)> {
    centres(app, 48.0, 48.0, move |_, cy| cy > height - 100.0)
}

/// The marker must sit on `index`'s item: its centre matches that item's icon
/// centre in the given axis (`true` = the rail, where the axis is y).
fn check_marker(
    label: &str,
    icons: &[(f32, f32)],
    dots: &[(f32, f32)],
    index: usize,
    vertical: bool,
    failures: &Rc<RefCell<Vec<String>>>,
) {
    if icons.len() != 4 {
        failures.borrow_mut().push(format!(
            "{label}: expected 4 nav icons, found {}",
            icons.len()
        ));
        return;
    }
    let icon = icons[index];
    let axis = |p: (f32, f32)| if vertical { p.1 } else { p.0 };
    // In the rail each item also draws its own (invisible) ring, so the marked
    // item carries two 48px squares; the bottom bar has only the marker.
    let on_item = dots
        .iter()
        .filter(|d| (axis(**d) - axis(icon)).abs() < 1.0)
        .count();
    if on_item == 0 {
        failures.borrow_mut().push(format!(
            "{label}: no marker on item {index} (icon centre {:.1}, markers {:?})",
            axis(icon),
            dots.iter().map(|d| axis(*d)).collect::<Vec<_>>()
        ));
    } else if vertical && on_item < 2 {
        failures.borrow_mut().push(format!(
            "{label}: expected the marker next to the item's own ring on item {index}, found {on_item}"
        ));
    }
    // Nothing may sit between the item slots: that is the mis-centred marker.
    let slots: Vec<f32> = icons.iter().map(|i| axis(*i)).collect();
    for dot in dots {
        let a = axis(*dot);
        if !slots.iter().any(|s| (a - s).abs() < 1.0) {
            failures.borrow_mut().push(format!(
                "{label}: a 48px marker sits at {a:.1}, off the item slots {slots:?}"
            ));
        }
    }
}

#[test]
fn nav_marker_is_centred_and_lands_on_the_picked_item() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1100, 800));
    app.window().show().unwrap();
    // The backend switches pages by publishing the section it leaves first.
    app.global::<nova::NavState>().set_from(0);
    app.set_show_home(false);
    app.set_show_settings(true);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let f = failures.clone();

    let app1 = app.as_weak();
    after(700, move || {
        let app = app1.upgrade().unwrap();
        check_marker(
            "rail settings",
            &rail_icons(&app),
            &rail_dots(&app),
            3,
            true,
            &f,
        );

        // Leave with the opposite switch (Settings → Home), as the backend does.
        app.global::<nova::NavState>().set_from(3);
        app.set_show_settings(false);
        app.set_show_home(true);
        let f2 = f.clone();
        let app2 = app.as_weak();
        after(700, move || {
            let app = app2.upgrade().unwrap();
            check_marker(
                "rail home",
                &rail_icons(&app),
                &rail_dots(&app),
                0,
                true,
                &f2,
            );

            // Narrow: the bottom bar's marker is the only 48px square.
            app.window().set_size(slint::PhysicalSize::new(360, 800));
            app.global::<nova::NavState>().set_from(0);
            app.set_show_home(false);
            app.set_show_library(true);
            let f3 = f2.clone();
            let app3 = app.as_weak();
            after(700, move || {
                let app = app3.upgrade().unwrap();
                let h = app.window().size().height as f32;
                check_marker(
                    "bar library",
                    &bar_icons(&app, h),
                    &bar_dots(&app, h),
                    2,
                    false,
                    &f3,
                );

                // A non-nav rebuild (the detail modal) must not shift it.
                app.set_modal_visible(true);
                let f4 = f3.clone();
                let app4 = app.as_weak();
                after(400, move || {
                    let app = app4.upgrade().unwrap();
                    app.set_modal_visible(false);
                    let f5 = f4.clone();
                    let app5 = app.as_weak();
                    after(700, move || {
                        let app = app5.upgrade().unwrap();
                        let h = app.window().size().height as f32;
                        check_marker(
                            "bar library after modal",
                            &bar_icons(&app, h),
                            &bar_dots(&app, h),
                            2,
                            false,
                            &f5,
                        );

                        // Navigation feedback is confined to the marker and
                        // icon; there is no whole-screen flash.
                        slint::quit_event_loop().unwrap();
                    });
                });
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "nav marker failures:\n  {}",
        failures.join("\n  ")
    );
}

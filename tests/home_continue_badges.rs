//! Home → Continue Watching card badges (headless).
//!
//! Cards offering the next episode of a series still being watched carry a
//! "Next up" badge, cards offering a brand-new episode (everything else
//! watched) a "New Episode" badge, both pinned to the poster; resume cards
//! (in-progress episode, progress rail showing) have neither. Covers that
//! each badge renders exactly on its rows, on the landing carousel and in
//! the "see all" subpage grid (same card component both places).

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn continue_badges_mark_next_up_and_new_episode() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(true);
    app.set_home_view(0);
    // New episode, resume, next up. The 2.45-card phone carousel includes
    // a partially visible third delegate; the subpage renders all three.
    let cont: Vec<nova::ContinueRow> = [2, 0, 1]
        .iter()
        .enumerate()
        .map(|(i, badge)| nova::ContinueRow {
            id: s(&format!("s{i}")),
            title: s(&format!("Show {i}")),
            subtitle: s("S1 E1 · Pilot"),
            poster: Default::default(),
            is_loaded: false,
            progress: if *badge == 0 { 0.3 } else { 0.0 },
            badge: *badge,
            ..Default::default()
        })
        .collect();
    app.set_home_continue(Rc::new(VecModel::from(cont)).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        // The landing renders two full cards and part of the third, so both
        // badge delegates exist; the resume card still has no badge.
        let next_up = ElementHandle::find_by_element_type_name(&app, "NextUpBadge").count();
        let new_ep = ElementHandle::find_by_element_type_name(&app, "NewEpisodeBadge").count();
        fail(
            &failures1,
            new_ep == 1,
            &format!("landing must badge the new-episode card, found {new_ep}"),
        );
        fail(
            &failures1,
            next_up == 1,
            &format!("landing must badge its partially visible next-up card, found {next_up}"),
        );

        // Same cards, same badges in the subpage grid (the covered landing
        // layer still instantiates its own badge).
        app.set_home_view(1);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(300, move || {
            let app = app2.upgrade().unwrap();
            let next_up = ElementHandle::find_by_element_type_name(&app, "NextUpBadge").count();
            let new_ep = ElementHandle::find_by_element_type_name(&app, "NewEpisodeBadge").count();
            fail(
                &failures2,
                next_up == 2,
                &format!("landing and subpage must each badge the next-up card, found {next_up}"),
            );
            // Landing's own new-episode badge plus the subpage grid's.
            fail(
                &failures2,
                new_ep == 2,
                &format!("subpage must badge the new-episode card too, found {new_ep}"),
            );
            slint::quit_event_loop().unwrap();
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "continue badge failures:\n  {}",
        failures.join("\n  ")
    );
}

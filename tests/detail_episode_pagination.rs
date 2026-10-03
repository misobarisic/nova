//! Episodes tab pagination (headless).
//!
//! Rust slices a season's filtered episode list into pages of 50; the UI must
//! render one page, resolve picks against the whole season (`episode_page_start
//! + i`), show the totals in the count line and dispatch absolute page moves.

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

fn episode(n: usize) -> nova::EpisodeRow {
    nova::EpisodeRow {
        text: s(format!("Episode {n}").as_str()),
        details: SharedString::default(),
        lines: 2,
        thumb: Default::default(),
        has_thumb: false,
        watched: false,
        progress: 0.0,
        ep_no: s(format!("EP {n}").as_str()),
        date: SharedString::default(),
        runtime: Default::default(),
    }
}

fn click_then(
    app: &nova::AppWindow,
    element: ElementHandle,
    ms: u64,
    body: impl FnOnce(nova::AppWindow) + 'static,
) {
    let weak = app.as_weak();
    slint::spawn_local(async move {
        element
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        after(ms, move || {
            if let Some(app) = weak.upgrade() {
                body(app);
            }
        });
    })
    .unwrap();
}

#[test]
fn episode_page_renders_totals_and_picks_globally() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    // Wide window: the episode header row (count + pager) is the wide layout.
    app.window().set_size(slint::PhysicalSize::new(900, 1000));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    app.set_detail_tab(3);
    // The episode grid + pager live in the `modal_episodes` branch (the
    // picker the Episodes tab opens).
    app.set_modal_episodes(true);
    // No entrance animation: the modal builds its tab body deterministically.
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_selected_title(s("Long Season"));
    app.set_season_names(Rc::new(VecModel::from(vec![s("Season 1")])).into());
    app.set_season_combo_idx(0);
    // Page 2 of 3 of a 120-episode season. The page carries two rows rather
    // than the real 50 so that both pagers (above and under the grid) are on
    // screen — the UI renders whatever page Rust hands it.
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (51..=52).map(episode).collect::<Vec<nova::EpisodeRow>>(),
        ))
        .into(),
    );
    app.set_episode_page(1);
    app.set_episode_page_count(3);
    app.set_episode_page_start(50);
    app.set_episode_total(120);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let picks: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let pages: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let picks = picks.clone();
        app.on_episode_picked(move |i| picks.borrow_mut().push(i));
    }
    {
        let pages = pages.clone();
        app.on_episode_page_picked(move |p| pages.borrow_mut().push(p));
    }
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let picks1 = picks.clone();
    let pages1 = pages.clone();
    // Two ticks: the first builds the modal and the Episodes tab, the second
    // settles its layout — the tab's elements only enter the tree after that
    // pass, so querying in the same tick finds nothing.
    after(300, move || {
        let app0 = app1.clone();
        // The tab body is built by the modal's deferred reveal timers, so it
        // needs a moment beyond the first layout pass.
        after(700, move || {
            let app = app0.upgrade().unwrap();
            // Warm-up: the tab body's delegates are laid out on the first pass a
            // tree query triggers, so probe once before looking for the card.
            let _ = ElementHandle::find_by_element_type_name(&app, "Text").count();

            // The first card of the page is episode 51 → global index 50.
            let Some(card) = ElementHandle::find_by_element_id(&app, "DetailPage::ep_card").next()
            else {
                fail(&failures1, false, "DIAG: no episode card rendered");
                slint::quit_event_loop().unwrap();
                return;
            };
            let failures2 = failures1.clone();
            let picks2 = picks1.clone();
            let pages2 = pages1.clone();
            click_then(&app, card, 350, move |app| {
                fail(
                    &failures2,
                    picks2.borrow().as_slice() == [50],
                    &format!(
                        "a card on page 2 must resolve against the season (got {:?})",
                        picks2.borrow()
                    ),
                );
                // A pager above *and* under the grid. The bottom one sits past 50
                // cards, and the accessibility tree only reports what is on
                // screen, so count the components (the buttons are still checked
                // by label where they are visible).
                let pagers = ElementHandle::find_by_element_type_name(&app, "EpisodePager").count();
                fail(
                    &failures2,
                    pagers == 2,
                    &format!(
                        "the episode list must offer a pager above and under the grid (found {pagers})"
                    ),
                );
                for label in ["Previous page", "Next page"] {
                    let found = ElementHandle::find_by_accessible_label(&app, label).count();
                    fail(
                        &failures2,
                        found == 2,
                        &format!(
                            "{label} must be offered above and under the grid (found {found})"
                        ),
                    );
                }
                let Some(next) = ElementHandle::find_by_accessible_label(&app, "Next page").next()
                else {
                    fail(&failures2, false, "DIAG: no Next page button");
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let failures3 = failures2.clone();
                let pages3 = pages2.clone();
                click_then(&app, next, 350, move |app| {
                    fail(
                        &failures3,
                        pages3.borrow().as_slice() == [2],
                        "Next must ask for the absolute page",
                    );
                    let Some(prev) =
                        ElementHandle::find_by_accessible_label(&app, "Previous page").next()
                    else {
                        fail(&failures3, false, "DIAG: no Previous page button");
                        slint::quit_event_loop().unwrap();
                        return;
                    };
                    let failures4 = failures3.clone();
                    let pages4 = pages3.clone();
                    click_then(&app, prev, 350, move |_app| {
                        fail(
                            &failures4,
                            pages4.borrow().as_slice() == [2, 0],
                            "Previous must ask for the absolute page",
                        );
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
        "episode pagination failures:\n  {}",
        failures.join("\n  ")
    );
}

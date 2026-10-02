//! Discover history survives filter focus changes, replays queries, and clears.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

async fn settle(ms: u64) {
    let completed = Rc::new(Cell::new(false));
    let mut scheduled = false;
    std::future::poll_fn(move |context| {
        if completed.get() {
            return std::task::Poll::Ready(());
        }
        if !scheduled {
            scheduled = true;
            let completed = completed.clone();
            let waker = context.waker().clone();
            after(ms, move || {
                completed.set(true);
                waker.wake();
            });
        }
        std::task::Poll::Pending
    })
    .await;
}

#[test]
fn history_focus_replay_back_and_clear() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(320, 800));
    app.set_show_home(false);
    let names = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|value| SharedString::from(*value))
                .collect::<Vec<_>>(),
        ))
        .into()
    };
    app.set_search_addon_names(names(&["All addons", "AniKoto"]));
    app.set_search_type_names(names(&["All types", "series"]));
    app.set_search_catalog_names(names(&["All catalogs", "Search"]));
    app.set_search_genre_names(names(&["All genres", "Action"]));
    let weak = app.as_weak();
    app.on_search_activated(move || {
        weak.upgrade()
            .unwrap()
            .set_discover_search_filters_open(true);
    });
    let filter_picks = Rc::new(RefCell::new(Vec::new()));
    let recorded = filter_picks.clone();
    app.on_search_filter_picked(move |kind, index| recorded.borrow_mut().push((kind, index)));
    app.set_discover_search_history(
        Rc::new(VecModel::from(vec![
            SharedString::from("Dune"),
            SharedString::from("Star Wars"),
        ]))
        .into(),
    );
    let weak = app.as_weak();
    let submissions = Rc::new(RefCell::new(Vec::<String>::new()));
    let recorded = submissions.clone();
    app.on_search_submitted(move |query| {
        recorded.borrow_mut().push(query.to_string());
        weak.upgrade().unwrap().set_discover_search_open(true);
    });
    let weak = app.as_weak();
    app.on_search_back_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_discover_search_open(false);
        app.set_discover_search_filters_open(false);
        app.set_discover_search_focused(false);
    });
    let weak = app.as_weak();
    app.on_search_history_cleared(move || {
        weak.upgrade()
            .unwrap()
            .set_discover_search_history(Rc::new(VecModel::<SharedString>::default()).into());
    });
    let weak = app.as_weak();
    app.on_search_history_item_removed(move |query| {
        let app = weak.upgrade().unwrap();
        let mut history: Vec<_> = app.get_discover_search_history().iter().collect();
        history.retain(|saved| saved != &query);
        app.set_discover_search_history(Rc::new(VecModel::from(history)).into());
    });
    app.show().unwrap();
    let failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let failures1 = failures.clone();
    let weak = app.as_weak();
    after(250, move || {
        let app = weak.upgrade().unwrap();
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Recent searches")
                .next()
                .is_none(),
            "history must wait for input focus"
        );
        let input = ElementHandle::find_by_element_type_name(&app, "SearchField")
            .next()
            .unwrap();
        let weak = app.as_weak();
        let _ = slint::spawn_local(async move {
            input
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            let expanded = Rc::new(Cell::new(false));
            let last_y = Rc::new(Cell::new(None::<f32>));
            for delay in (20..280).step_by(20) {
                let weak = weak.clone();
                let expanded = expanded.clone();
                let last_y = last_y.clone();
                after(delay, move || {
                    let app = weak.upgrade().unwrap();
                    if let Some(row) = ElementHandle::find_by_accessible_label(&app, "Type").next()
                    {
                        let y = row.absolute_position().y;
                        if last_y.get().is_some_and(|previous| y > previous + 0.1) {
                            expanded.set(true);
                        }
                        last_y.set(Some(y));
                    }
                });
            }
            after(320, move || {
                let app = weak.upgrade().unwrap();
                // Reading successive layouts above exercises the expanding
                // panel rather than only checking its final visible state.
                if !expanded.get() {
                    failures1
                        .borrow_mut()
                        .push("recents did not expand into the page layout".into());
                }
                assert!(
                    ElementHandle::find_by_accessible_label(&app, "Recent searches")
                        .next()
                        .is_some()
                );
                let filter = ElementHandle::find_by_accessible_label(&app, "Search addon")
                    .next()
                    .unwrap();
                let weak = app.as_weak();
                let _ = slint::spawn_local(async move {
                    filter
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    settle(100).await;
                    let app = weak.upgrade().unwrap();
                    assert!(
                        !app.get_discover_search_focused(),
                        "the dropdown must take focus"
                    );
                    assert!(
                        ElementHandle::find_by_accessible_label(&app, "Recent searches")
                            .next()
                            .is_some(),
                        "opening a search filter must keep recent searches visible"
                    );
                    let choice = ElementHandle::find_by_accessible_label(&app, "AniKoto")
                        .next()
                        .expect("search addon option");
                    choice
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    settle(100).await;
                    let app = weak.upgrade().unwrap();
                    assert_eq!(app.get_search_addon_combo_idx(), 1);
                    assert_eq!(filter_picks.borrow().as_slice(), [(0, 1)]);
                    assert!(
                        ElementHandle::find_by_accessible_label(&app, "Recent searches")
                            .next()
                            .is_some(),
                        "selecting a search filter must keep recent searches visible"
                    );
                    let query = ElementHandle::find_by_accessible_label(&app, "Dune")
                        .next()
                        .expect("recent query after changing a filter");
                    query
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(150, move || {
                        let app = weak.upgrade().unwrap();
                        if app.get_search_text() != "Dune"
                            || submissions.borrow().as_slice() != ["Dune"]
                        {
                            failures1.borrow_mut().push(
                                "history selection must populate and submit its query".into(),
                            );
                        }
                        let back = ElementHandle::find_by_accessible_label(&app, "Back")
                            .next()
                            .unwrap();
                        let weak = app.as_weak();
                        let _ = slint::spawn_local(async move {
                            back.single_click(slint::platform::PointerEventButton::Left)
                                .await;
                            after(150, move || {
                                let app = weak.upgrade().unwrap();
                                if !app.get_search_text().is_empty()
                                    || app.get_discover_search_open()
                                    || app.get_discover_search_filters_open()
                                {
                                    failures1.borrow_mut().push(
                                        "Back must clear the query and restore browse".into(),
                                    );
                                }
                                let input =
                                    ElementHandle::find_by_element_type_name(&app, "SearchField")
                                        .next()
                                        .unwrap();
                                let weak = app.as_weak();
                                let _ = slint::spawn_local(async move {
                                    input
                                        .single_click(slint::platform::PointerEventButton::Left)
                                        .await;
                                    after(150, move || {
                                        let app = weak.upgrade().unwrap();
                                        let remove = ElementHandle::find_by_accessible_label(
                                            &app,
                                            "Remove search: Dune",
                                        )
                                        .next()
                                        .unwrap();
                                        let weak = app.as_weak();
                                        let _ = slint::spawn_local(async move {
                                            remove
                                                .single_click(
                                                    slint::platform::PointerEventButton::Left,
                                                )
                                                .await;
                                            after(150, move || {
                                                let app = weak.upgrade().unwrap();
                                                let history = app.get_discover_search_history();
                                                let remaining = history
                                                    .row_data(0)
                                                    .map(|query| query.to_string());
                                                if history.row_count() != 1
                                                    || remaining.as_deref() != Some("Star Wars")
                                                {
                                                    failures1.borrow_mut().push(
                                                         "removing Dune must preserve the other recent search"
                                                             .into(),
                                                     );
                                                }
                                                if submissions.borrow().as_slice() != ["Dune"] {
                                                    failures1.borrow_mut().push(
                                                         "removing a history item must not submit it"
                                                             .into(),
                                                     );
                                                }
                                                let clear =
                                                    ElementHandle::find_by_accessible_label(
                                                        &app,
                                                        "Clear history",
                                                    )
                                                    .next()
                                                    .unwrap();
                                                let weak = app.as_weak();
                                                let _ = slint::spawn_local(async move {
                                                    clear
                                                .single_click(
                                                    slint::platform::PointerEventButton::Left,
                                                )
                                                .await;
                                                    let app = weak.upgrade().unwrap();
                                                    if app.get_discover_search_history().row_count()
                                                        != 0
                                                    {
                                                        failures1.borrow_mut().push(
                                                    "Clear history must remove the recent queries"
                                                        .into(),
                                                );
                                                    }
                                                    slint::quit_event_loop().unwrap();
                                                });
                                            });
                                        });
                                    });
                                });
                            });
                        });
                    });
                });
            });
        });
    });
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "{}",
        failures.borrow().join("\n")
    );
}

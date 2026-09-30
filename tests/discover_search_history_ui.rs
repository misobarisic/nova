//! Discover history appears on focus, replays queries, and can be cleared.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn history_focus_replay_back_and_clear() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(320, 800));
    app.set_show_home(false);
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
    app.on_search_back_picked(move || weak.upgrade().unwrap().set_discover_search_open(false));
    let weak = app.as_weak();
    app.on_search_history_cleared(move || {
        weak.upgrade()
            .unwrap()
            .set_discover_search_history(Rc::new(VecModel::<SharedString>::default()).into());
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
                let query = ElementHandle::find_by_accessible_label(&app, "Dune")
                    .next()
                    .unwrap();
                let weak = app.as_weak();
                let _ = slint::spawn_local(async move {
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
                                        let clear = ElementHandle::find_by_accessible_label(
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
                                            if app.get_discover_search_history().row_count() != 0 {
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
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "{}",
        failures.borrow().join("\n")
    );
}

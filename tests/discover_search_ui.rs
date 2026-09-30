//! Discover's catalog filters stay independent from the global search view.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(value: &str) -> SharedString {
    SharedString::from(value)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn strings(values: &[&str]) -> Vec<SharedString> {
    values.iter().map(|value| s(value)).collect()
}

fn card(index: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: s(&format!("movie:{index}")),
        title: s(&format!("Movie {index}")),
        year: s("2026"),
        poster_path: s(""),
        poster: slint::Image::default(),
        is_loaded: false,
        badge: s(""),
        watched: false,
    }
}

#[test]
fn search_results_back_restores_discover_filters_and_browse_model() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    // All controls fit here; the narrow, clipped filter rail is exercised by
    // discover_reveal_and_filters instead of counting off-screen dropdowns.
    app.window().set_size(slint::PhysicalSize::new(900, 800));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_library(false);
    app.set_show_settings(false);
    app.set_addon_names(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
    app.set_type_names(Rc::new(VecModel::from(strings(&["movie", "series"]))).into());
    app.set_catalog_names(Rc::new(VecModel::from(strings(&["Popular", "Top rated"]))).into());
    app.set_genre_names(
        Rc::new(VecModel::from(strings(&["All genres", "Action", "Drama"]))).into(),
    );
    app.set_type_combo_idx(1);
    app.set_catalog_combo_idx(1);
    app.set_genre_combo_idx(2);
    app.set_catalog(Rc::new(VecModel::from((0..30).map(card).collect::<Vec<_>>())).into());
    app.set_search_results(Rc::new(VecModel::from(vec![card(100), card(101)])).into());

    let weak = app.as_weak();
    app.on_search_back_picked(move || {
        if let Some(app) = weak.upgrade() {
            app.set_discover_search_open(false);
        }
    });

    let failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        let filters = ElementHandle::find_by_element_type_name(&app, "Dropdown").count();
        if filters != 3 {
            failures1.borrow_mut().push(format!(
                "browse view should show type, catalog, and genre filters (found {filters})"
            ));
        }
        if ElementHandle::find_by_accessible_label(&app, "(30 items)")
            .next()
            .is_some()
        {
            failures1
                .borrow_mut()
                .push("Discover should not show a catalog item count".into());
        }

        app.set_discover_search_open(true);
        app.set_search_text(s("Star Wars"));
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(200, move || {
            let app = app2.upgrade().unwrap();
            if ElementHandle::find_by_element_type_name(&app, "Dropdown")
                .next()
                .is_some()
            {
                failures2
                    .borrow_mut()
                    .push("browse filters should be hidden in search results".into());
            }
            if ElementHandle::find_by_element_type_name(&app, "Button")
                .next()
                .is_some()
            {
                failures2
                    .borrow_mut()
                    .push("automatic search should not show a Search button".into());
            }
            if ElementHandle::find_by_accessible_label(&app, "(2 items)")
                .next()
                .is_some()
            {
                failures2
                    .borrow_mut()
                    .push("search results should not show an item count".into());
            }

            let Some(back) = ElementHandle::find_by_accessible_label(&app, "Back").next() else {
                failures2
                    .borrow_mut()
                    .push("search results should expose the icon Back control".into());
                slint::quit_event_loop().unwrap();
                return;
            };

            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            let _ = slint::spawn_local(async move {
                back.single_click(slint::platform::PointerEventButton::Left)
                    .await;
                after(200, move || {
                    let app = app3.upgrade().unwrap();
                    if app.get_discover_search_open() {
                        failures3
                            .borrow_mut()
                            .push("Back should return to the browse view".into());
                    }
                    if !app.get_search_text().is_empty() {
                        failures3.borrow_mut().push("Back should clear the search input".into());
                    }
                    if app.get_type_combo_idx() != 1
                        || app.get_catalog_combo_idx() != 1
                        || app.get_genre_combo_idx() != 2
                    {
                        failures3
                            .borrow_mut()
                            .push("search navigation should preserve the selected filters".into());
                    }
                    if app.get_catalog().row_count() != 30
                        || app.get_search_results().row_count() != 2
                    {
                        failures3
                            .borrow_mut()
                            .push("browse and search results should retain separate models".into());
                    }
                    let filters =
                        ElementHandle::find_by_element_type_name(&app, "Dropdown").count();
                    if filters != 3 {
                        failures3.borrow_mut().push(format!(
                            "returning to browse should restore all three filters (found {filters})"
                        ));
                    }
                    slint::quit_event_loop().unwrap();
                });
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "Discover search UI failures:\n  {}",
        failures.join("\n  ")
    );
}

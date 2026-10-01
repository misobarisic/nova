//! Settings → Home keeps only configured catalog/genre pairs and adds them
//! through the focused catalog picker.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let position = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn tap(app: &nova::AppWindow, position: LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

#[test]
fn home_catalogs_are_added_with_a_genre_and_removed_individually() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_home_catalog_rows(
        Rc::new(VecModel::from(vec![nova::HomeCatalogRow {
            title: "Popular".into(),
            addon: "Addon A".into(),
            media_type: "movie".into(),
            genre: "Action".into(),
            available: true,
            enabled: true,
        }]))
        .into(),
    );
    app.set_home_catalog_candidate_names(
        Rc::new(VecModel::from(vec![
            SharedString::from("Addon A — Popular · movie"),
            SharedString::from("Addon B — Trending · series"),
        ]))
        .into(),
    );
    app.set_home_catalog_candidate_index(0);
    app.set_home_catalog_genre_names(
        Rc::new(VecModel::from(vec![
            SharedString::from("All genres"),
            SharedString::from("Action"),
            SharedString::from("Comedy"),
        ]))
        .into(),
    );
    app.set_home_catalog_genre_index(0);

    let add_opened = Rc::new(RefCell::new(0));
    let add_opened_count = add_opened.clone();
    let app_weak = app.as_weak();
    app.on_home_catalog_add_requested(move || {
        *add_opened_count.borrow_mut() += 1;
        app_weak.upgrade().unwrap().set_home_catalog_add_open(true);
    });

    let added = Rc::new(RefCell::new(Vec::new()));
    let added_items = added.clone();
    let app_weak = app.as_weak();
    app.on_home_catalog_added(move |catalog, genre| {
        added_items.borrow_mut().push((catalog, genre));
        app_weak.upgrade().unwrap().set_home_catalog_add_open(false);
    });

    let removed = Rc::new(RefCell::new(Vec::new()));
    let removed_items = removed.clone();
    app.on_home_catalog_removed(move |index| removed_items.borrow_mut().push(index));

    let app_weak = app.as_weak();
    after(100, move || {
        let app = app_weak.upgrade().unwrap();
        let home_link = ElementHandle::find_by_element_type_name(&app, "SettingsLink")
            .nth(9)
            .expect("Home settings link");
        tap(&app, center(&home_link));

        let app_weak = app.as_weak();
        after(350, move || {
            let app = app_weak.upgrade().unwrap();
            assert_eq!(
                ElementHandle::find_by_element_id(&app, "SettingsPage::home_catalog_remove")
                    .count(),
                1,
                "only the configured catalog/genre entry should be listed"
            );

            let add_button =
                ElementHandle::find_by_element_id(&app, "SettingsPage::home_catalog_add")
                    .next()
                    .expect("Add catalog button");
            tap(&app, center(&add_button));
            assert_eq!(*add_opened.borrow(), 1);
            assert!(app.get_home_catalog_add_open());

            app.set_home_catalog_candidate_index(1);
            app.set_home_catalog_genre_index(2);
            let add_button =
                ElementHandle::find_by_element_id(&app, "SettingsPage::home_catalog_modal_add")
                    .next()
                    .expect("picker Add button");
            tap(&app, center(&add_button));
            assert_eq!(*added.borrow(), vec![(1, 2)]);
            assert!(!app.get_home_catalog_add_open());

            let remove_button =
                ElementHandle::find_by_element_id(&app, "SettingsPage::home_catalog_remove")
                    .next()
                    .expect("configured row Remove button");
            tap(&app, center(&remove_button));
            assert_eq!(*removed.borrow(), vec![0]);
            slint::quit_event_loop().unwrap();
        });
    });

    slint::run_event_loop().unwrap();
}

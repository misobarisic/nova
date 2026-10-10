//! Empty/focused Discover search hides browse content and restores it on Back.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn frame(app: &nova::AppWindow, ms: u64) {
    for element in ElementQuery::from_root(app)
        .match_predicate(|_| true)
        .find_all()
    {
        let _ = (
            element.size(),
            element.absolute_position(),
            element.computed_opacity(),
        );
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..30 {
        frame(app, 16);
    }
}

fn click(app: &nova::AppWindow, element: ElementHandle) {
    let p = element.absolute_position();
    let s = element.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn back(app: &nova::AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn assert_no_browse(app: &nova::AppWindow) {
    assert!(
        ElementHandle::find_by_accessible_label(app, "Browse 0")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_element_id(app, "DiscoverHeader::filter_flick")
            .next()
            .is_none()
    );
}

#[test]
fn search_context_is_isolated_before_typing_and_animates_without_losing_browse_state() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_catalog(
        Rc::new(VecModel::from(
            (0..60)
                .map(|i| nova::MediaCard {
                    id: format!("browse:{i}").into(),
                    title: format!("Browse {i}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let names = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|v| slint::SharedString::from(*v))
                .collect::<Vec<_>>(),
        ))
        .into()
    };
    app.set_addon_names(names(&["Cinemeta", "AniKoto"]));
    app.set_type_names(names(&["movie", "series"]));
    app.set_catalog_names(names(&["Popular", "Top rated"]));
    app.set_genre_names(names(&["All genres", "Action"]));
    app.set_addon_combo_idx(1);
    app.set_type_combo_idx(1);
    app.set_catalog_combo_idx(1);
    app.set_genre_combo_idx(1);
    app.set_search_addon_names(names(&["All addons", "AniKoto"]));
    app.set_search_type_names(names(&["All types", "series"]));
    app.set_search_catalog_names(names(&["All catalogs"]));
    app.set_search_genre_names(names(&["All genres"]));
    app.set_discover_search_history(names(&["Dune", "Frieren"]));
    let weak = app.as_weak();
    app.on_search_activated(move || {
        weak.upgrade()
            .unwrap()
            .set_discover_search_filters_open(true)
    });
    let weak = app.as_weak();
    app.on_search_back_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_discover_search_open(false);
        app.set_discover_search_filters_open(false);
        app.set_discover_search_focused(false);
    });
    let loads = Rc::new(Cell::new(0));
    let counter = loads.clone();
    app.on_load_more(move || counter.set(counter.get() + 1));
    app.set_can_load_more(true);

    for scroll_page in [false, true] {
        for width in [390, 1100] {
            app.set_discover_scroll_page(scroll_page);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            settle(&app);
            app.set_discover_scroll_y(-700.0);
            settle(&app);
            let saved_y = app.get_discover_scroll_y();
            let before_loads = loads.get();
            click(
                &app,
                ElementHandle::find_by_element_id(&app, "DiscoverHeader::search_line")
                    .next()
                    .unwrap(),
            );
            assert!(app.get_discover_search_focused());
            assert!(app.get_discover_search_filters_open());
            assert!(
                !app.get_discover_search_open(),
                "no query/results required to isolate search"
            );
            let mut animated = false;
            for _ in 0..30 {
                frame(&app, 16);
                assert_no_browse(&app);
                if let Some(heading) =
                    ElementHandle::find_by_element_id(&app, "DiscoverHeader::results_heading")
                        .next()
                {
                    let opacity = heading.computed_opacity();
                    animated |= opacity > 0.01 && opacity < 0.99;
                }
            }
            assert!(
                animated,
                "{scroll_page}/{width}: search heading should fade in"
            );
            assert_eq!(
                loads.get(),
                before_loads,
                "empty search must not paginate browse"
            );
            assert!((app.get_discover_scroll_y() - saved_y).abs() < 1.0);
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Recent searches")
                    .next()
                    .is_some()
            );
            // Dropdown focus and rotation keep the search session open.
            app.set_discover_search_focused(false);
            app.window().set_size(slint::PhysicalSize::new(
                if width < 700 { 1100 } else { 390 },
                900,
            ));
            settle(&app);
            assert_no_browse(&app);
            back(&app);
            settle(&app);
            assert!(!app.get_discover_search_filters_open());
            assert!(
                ElementHandle::find_by_element_id(&app, "DiscoverGrid::card_slot")
                    .next()
                    .is_some()
            );
            assert!(
                ElementHandle::find_by_element_id(&app, "DiscoverHeader::filter_flick")
                    .next()
                    .is_some()
            );
            assert!(
                (app.get_discover_scroll_y() - saved_y).abs() < 1.0,
                "Back restores browse scroll"
            );
            assert_eq!(
                (
                    app.get_addon_combo_idx(),
                    app.get_type_combo_idx(),
                    app.get_catalog_combo_idx(),
                    app.get_genre_combo_idx()
                ),
                (1, 1, 1, 1)
            );
        }
    }
    // Both motion switches must snap the context to fully visible immediately.
    for (enabled, transitions) in [(false, true), (true, false)] {
        app.global::<nova::Anim>().set_enabled(enabled);
        app.global::<nova::Anim>().set_transitions(transitions);
        click(
            &app,
            ElementHandle::find_by_element_id(&app, "DiscoverHeader::search_line")
                .next()
                .unwrap(),
        );
        frame(&app, 1);
        assert_no_browse(&app);
        assert_eq!(
            ElementHandle::find_by_element_id(&app, "DiscoverHeader::results_heading")
                .next()
                .unwrap()
                .computed_opacity(),
            1.0
        );
        back(&app);
        settle(&app);
    }
}

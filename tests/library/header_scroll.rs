//! The library hero scrolls away while controls pin over a stable viewport.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(ms));
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        // No renderer evaluates the restored page's layout in this backend.
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        idle(16);
    }
}

/// A timed drag followed by a stationary hold preserves a precise offset,
/// independent of native Flickable momentum.
fn drag_up(app: &nova::AppWindow, from: LogicalPosition, dy: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    // Transparent navigation reserves zero layout height while its real 68px
    // tap targets overlay the scroll viewport. Begin above that tap bar so
    // these gestures exercise scrolling rather than navigation hit testing.
    let window = app.window().size();
    let from = LogicalPosition::new(
        from.x,
        if window.width < 700 {
            from.y.min(window.height as f32 - 68.0 - 30.0)
        } else {
            from.y
        },
    );
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: from,
        button: PointerEventButton::Left,
    });
    idle(120);
    for step in 1..=4 {
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(from.x, from.y - dy * step as f32 / 4.0),
        });
        idle(20);
    }
    idle(160);
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(from.x, from.y - dy),
        button: PointerEventButton::Left,
    });
}

fn card(id: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: s(format!("id{id}").as_str()),
        title: s(format!("Title {id}").as_str()),
        year: s("2024"),
        poster_path: SharedString::default(),
        poster: Default::default(),
        is_loaded: false,
        badge: SharedString::default(),
        watched: false,
        ..Default::default()
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, &format!("LibraryPage::{id}"))
        .next()
        .expect(id)
}

#[test]
fn library_hero_collapses_with_pinned_controls_and_stable_scroll_geometry() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "library_hero_collapses_with_pinned_controls_and_stable_scroll_geometry"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_library_category_names(
        Rc::new(VecModel::from(vec![s("Watching"), s("Completed")])).into(),
    );
    app.set_library(Rc::new(VecModel::from((0..60).map(card).collect::<Vec<_>>())).into());
    app.window().show().unwrap();
    for (width, saved_collections) in [false, true]
        .into_iter()
        .flat_map(|saved| [320, 390, 1280].map(move |width| (width, saved)))
    {
        app.global::<nova::SmartCollectionsBridge>().set_rows(
            Rc::new(VecModel::from(if saved_collections {
                vec![nova::SmartCollectionRow {
                    id: s("weekend"),
                    name: s("Weekend"),
                }]
            } else {
                vec![]
            }))
            .into(),
        );
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        for list in [false, true] {
            app.set_library_list_view(list);
            app.set_library_scroll_y(0.0);
            settle(&app);
            let scroll = element(&app, "lib_scroll");
            let viewport_pos = scroll.absolute_position();
            let viewport_size = scroll.size();
            if width < 700 {
                let bottom = element(&app, "bottom");
                assert!((bottom.absolute_position().y + bottom.size().height - 900.0).abs() < 0.5);
                assert!(bottom.computed_opacity() > 0.99);
                assert!(
                    viewport_pos.y + viewport_size.height <= bottom.absolute_position().y + 0.5
                );
            }
            let original_filter_y = element(&app, "filter_rail").absolute_position().y;
            let hero = element(&app, "library_hero");
            assert!(hero.absolute_position().y >= viewport_pos.y - 0.5);
            drag_up(
                &app,
                LogicalPosition::new(
                    viewport_pos.x + viewport_size.width / 2.0,
                    viewport_pos.y + viewport_size.height - 30.0,
                ),
                280.0,
            );
            settle(&app);
            assert!(
                (app.get_library_scroll_y() + 280.0).abs() < 2.0,
                "header collapse must not change drag distance: width={width}, list={list}, offset={}, viewport={viewport_pos:?} {viewport_size:?}",
                app.get_library_scroll_y()
            );
            // Keep the handle: visible-element queries omit the clipped hero.
            assert!(
                hero.absolute_position().y + hero.size().height <= viewport_pos.y + 0.5,
                "title and description must leave the viewport at {width}px, list={list}"
            );
            let filter_y = element(&app, "filter_rail").absolute_position().y;
            assert!(filter_y < original_filter_y - 40.0);
            if saved_collections {
                let smart = element(&app, "smart_rail");
                let smart_y = smart.absolute_position().y;
                assert!(
                    (smart_y - viewport_pos.y).abs() < 0.5,
                    "saved collection filters pin first"
                );
                assert!(
                    (filter_y - smart_y - smart.size().height - 14.0).abs() < 0.5,
                    "category filters pin immediately below saved collections"
                );
            } else {
                assert!(
                    ElementHandle::find_by_element_id(&app, "LibraryPage::smart_rail")
                        .next()
                        .is_none()
                );
                assert!(
                    (filter_y - viewport_pos.y - 14.0).abs() < 0.5,
                    "category filters keep the standard layout gap without saved collections: {filter_y}, {viewport_pos:?}"
                );
            }
            let controls_y = element(&app, "library_sort_controls").absolute_position().y;
            assert!(controls_y > filter_y);
            drag_up(
                &app,
                LogicalPosition::new(
                    viewport_pos.x + viewport_size.width / 2.0,
                    viewport_pos.y + viewport_size.height - 30.0,
                ),
                200.0,
            );
            settle(&app);
            assert!(
                (app.get_library_scroll_y() + 480.0).abs() < 2.0,
                "second drag preserves exact distance at width={width}, list={list}: {}",
                app.get_library_scroll_y()
            );
            if saved_collections {
                assert!(
                    (element(&app, "smart_rail").absolute_position().y - viewport_pos.y).abs()
                        < 0.5
                );
            }
            assert!((element(&app, "filter_rail").absolute_position().y - filter_y).abs() < 0.5);
            assert!(
                (element(&app, "library_sort_controls").absolute_position().y - controls_y).abs()
                    < 0.5
            );
            assert_eq!(scroll.absolute_position(), viewport_pos);
            assert_eq!(scroll.size(), viewport_size);
            // The pinned controls remain clickable over scrolling cards.
            element(&app, "library_search_button")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert!(app.get_library_search_open());
            assert!(
                (hero.absolute_position().y + hero.size().height - viewport_pos.y).abs() < 15.0
            );
            assert_eq!(scroll.size(), viewport_size);
            element(&app, "library_search_button")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            app.set_library_scroll_y(0.0);
            settle(&app);
            assert!(
                (element(&app, "filter_rail").absolute_position().y - original_filter_y).abs()
                    < 0.5,
                "returning to the top restores the hero"
            );
        }
    }
}

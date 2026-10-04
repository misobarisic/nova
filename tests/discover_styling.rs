//! Discover shares Library's collapsing hero and pinned search/filter controls.
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

fn find(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

#[test]
fn discover_hero_collapses_while_search_and_filters_stay_pinned() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_catalog(Rc::new(VecModel::from((0..60).map(card).collect::<Vec<_>>())).into());
    let names = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<SharedString>>(),
        ))
        .into()
    };
    app.set_addon_names(names(&["All addons", "Cinemeta"]));
    app.set_type_names(names(&["movie", "series"]));
    app.set_catalog_names(names(&["Popular", "Top rated"]));
    app.set_genre_names(names(&["All genres", "Action"]));
    app.window().show().unwrap();
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        for scroll_page in [false, true] {
            app.set_discover_scroll_page(scroll_page);
            app.set_discover_scroll_y(0.0);
            settle(&app);
            let scroll = find(
                &app,
                if scroll_page {
                    "DiscoverPage::page_scroll"
                } else {
                    "DiscoverPage::grid_scroll"
                },
            );
            let pos = scroll.absolute_position();
            let size = scroll.size();
            let hero = find(&app, "DiscoverHeader::discover_hero");
            let search = find(&app, "DiscoverHeader::search_line");
            let filters = find(&app, "DiscoverHeader::filter_flick");
            let original_y = search.absolute_position().y;
            assert!(hero.absolute_position().y >= pos.y - 0.5);
            drag_up(
                &app,
                LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height - 30.0),
                280.0,
            );
            settle(&app);
            assert!((app.get_discover_scroll_y() + 280.0).abs() < 2.0);
            assert!(
                hero.absolute_position().y + hero.size().height <= pos.y + 0.5,
                "hero must scroll away at {width}px, mode={scroll_page}"
            );
            let pinned_search_y = search.absolute_position().y;
            let pinned_filter_y = filters.absolute_position().y;
            assert!(pinned_search_y < original_y - 40.0);
            assert!(pinned_search_y >= pos.y - 0.5);
            assert!(pinned_filter_y > pinned_search_y);
            drag_up(
                &app,
                LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height - 30.0),
                180.0,
            );
            settle(&app);
            assert!((search.absolute_position().y - pinned_search_y).abs() < 0.5);
            assert!((filters.absolute_position().y - pinned_filter_y).abs() < 0.5);
            assert_eq!(scroll.absolute_position(), pos);
            assert_eq!(scroll.size(), size);
            if width < 700 {
                let bottom = find(
                    &app,
                    if scroll_page {
                        "DiscoverPage::bottom_scroll"
                    } else {
                        "DiscoverPage::bottom_fix"
                    },
                );
                assert!((bottom.absolute_position().y + bottom.size().height - 900.0).abs() < 0.5);
                assert!(pos.y + size.height <= bottom.absolute_position().y + 0.5);
            }
            app.set_discover_scroll_y(0.0);
            settle(&app);
            assert!((search.absolute_position().y - original_y).abs() < 0.5);
        }
    }
}

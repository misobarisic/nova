//! The latest Library viewport survives Detail and root-tab recreation.
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

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, &format!("LibraryPage::{id}"))
        .next()
        .expect(id)
}

#[test]
fn detail_return_then_pointer_scroll_survives_root_tab_recreation() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_library(Rc::new(VecModel::from((0..80).map(card).collect::<Vec<_>>())).into());
    app.window().show().unwrap();
    for width in [320, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        for list in [false, true] {
            app.set_library_list_view(list);
            app.set_library_kb_zone(2);
            app.set_library_kb_idx(0);
            app.set_library_scroll_y(0.0);
            settle(&app);
            let scroll = element(&app, "lib_scroll");
            let position = scroll.absolute_position();
            let size = scroll.size();
            drag_up(
                &app,
                LogicalPosition::new(
                    position.x + size.width / 2.0,
                    position.y + size.height - 30.0,
                ),
                150.0,
            );
            settle(&app);
            let detail_offset = app.get_library_scroll_y();
            assert!(detail_offset < -100.0);
            // Opening Detail leaves its card as the keyboard return target.
            app.set_modal_visible(true);
            settle(&app);
            app.set_modal_visible(false);
            settle(&app);
            assert!(
                (app.get_library_scroll_y() - detail_offset).abs() < 2.0,
                "Detail return must retain its exact nonzero offset at {width}px, list={list}"
            );
            let scroll = element(&app, "lib_scroll");
            let position = scroll.absolute_position();
            let size = scroll.size();
            drag_up(
                &app,
                LogicalPosition::new(
                    position.x + size.width / 2.0,
                    position.y + size.height - 30.0,
                ),
                700.0,
            );
            settle(&app);
            let latest = app.get_library_scroll_y();
            assert!(latest < -600.0);
            app.set_library_kb_zone(0);
            app.set_kb_active(false);
            // Pointer activation records a return target before opening the
            // detail page. That bookkeeping must not scroll to the old card.
            app.set_library_kb_zone(2);
            assert!((app.get_library_scroll_y() - latest).abs() < 2.0);
            // Stream/detail navigation recreates Library with lifted keyboard
            // state. Restoring that state must not reveal a stale target even
            // for one frame before the final viewport settles.
            app.set_modal_visible(true);
            app.set_stream_selector_open(true);
            app.set_library_kb_idx(40);
            settle(&app);
            app.set_stream_selector_open(false);
            app.set_modal_visible(false);
            for frame in 0..16 {
                for e in ElementQuery::from_root(&app)
                    .match_predicate(|_| true)
                    .find_all()
                {
                    let _ = (e.size(), e.absolute_position());
                }
                assert!(
                    (app.get_library_scroll_y() - latest).abs() < 2.0,
                    "stream return moved viewport on frame {frame}: {latest} -> {} ({width}px, list={list})",
                    app.get_library_scroll_y()
                );
                idle(16);
            }
            for tab in ["home", "discover", "settings"] {
                app.set_show_library(false);
                app.set_show_home(tab == "home");
                app.set_show_settings(tab == "settings");
                settle(&app);
                app.set_show_home(false);
                app.set_show_settings(false);
                app.set_show_library(true);
                settle(&app);
                assert!(
                    (app.get_library_scroll_y() - latest).abs() < 2.0,
                    "{tab} return must retain the latest scroll at {width}px, list={list}"
                );
            }
            // A refreshed library model must not reveal the stale Detail card.
            app.set_library(Rc::new(VecModel::from((0..80).map(card).collect::<Vec<_>>())).into());
            settle(&app);
            assert!((app.get_library_scroll_y() - latest).abs() < 2.0);
            // Real keyboard navigation still reveals the newly selected row.
            app.set_kb_active(true);
            app.set_library_kb_idx(60);
            settle(&app);
            assert!(app.get_library_scroll_y() < latest - 100.0);
        }
    }
}

//! My Library scroll restore (headless).
//!
//! Leaving the library for an entry and coming back must land on the exact
//! same offset. The page is recreated on return and restores its absolute
//! `scroll_y`, but the one-shot restore used to also *follow* the focused card
//! — which nudged a partially visible row into view and shifted the listing.
//! A row that is fully hidden (the page came back focused elsewhere) must
//! still be revealed: that is the recovery the follow was there for.

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
    }
}

#[test]
fn returning_to_the_library_keeps_the_scroll_offset() {
    i_slint_backend_testing::init_integration_test_with_mock_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_library(
        Rc::new(VecModel::from(
            (0..60).map(card).collect::<Vec<nova::MediaCard>>(),
        ))
        .into(),
    );
    // Focus on the first card: after a small scroll its row is only partly
    // visible — exactly the case the restore used to nudge up.
    app.set_library_kb_zone(2);
    app.set_library_kb_idx(0);

    settle(&app);
    let scroll = ElementHandle::find_by_element_id(&app, "LibraryPage::lib_scroll")
        .next()
        .expect("library grid");
    let pos = scroll.absolute_position();
    let size = scroll.size();
    let start = app.get_library_scroll_y();
    drag_up(
        &app,
        LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0),
        60.0,
    );
    settle(&app);
    let before = app.get_library_scroll_y();
    assert!(
        (before - start + 60.0).abs() <= 2.0,
        "the grid must follow the drag ({start} -> {before})"
    );

    // A partially visible focused row must keep the exact restored offset.
    app.set_modal_visible(true);
    settle(&app);
    app.set_modal_visible(false);
    settle(&app);
    let after_return = app.get_library_scroll_y();
    assert!(
        (after_return - before).abs() <= 0.5,
        "returning from an entry must keep the offset ({before} -> {after_return})"
    );

    // A fully hidden focused card is still revealed on page recreation.
    app.set_modal_visible(true);
    app.set_library_kb_idx(59);
    settle(&app);
    app.set_modal_visible(false);
    settle(&app);
    let revealed = app.get_library_scroll_y();
    assert!(
        revealed < before - 100.0,
        "a fully hidden focused card must be revealed ({before} -> {revealed})"
    );
}

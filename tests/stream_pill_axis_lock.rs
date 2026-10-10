//! Fullscreen stream selectors separate horizontal addon filters from the
//! vertical result viewport. Diagonal addon swipes cannot scroll the results,
//! and vertical result drags leave the fixed header in place.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::rc::Rc;
use std::time::Duration;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

/// A movie's selector with an overflowing addon list, on touch, opened and
/// settled. Every case gets its own instance so it starts from scroll 0 and an
/// unscrolled pill row.
fn setup() -> nova::AppWindow {
    let app = nova::AppWindow::new().unwrap();
    // Geometry/gesture fixture; entrance motion is covered separately.
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_detail_tab(0);
    app.set_modal_episodes(false);
    app.set_selected_title(s("Movie"));
    app.set_stream_addons(
        Rc::new(VecModel::from(
            (0..10)
                .map(|i| s(&format!("Addon Number {i}")))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_streams_hint(s("hint"));
    app.set_streams(
        Rc::new(VecModel::from(
            (0..12)
                .map(|i| nova::StreamRow {
                    id: s(&format!("stream-{i}")),
                    text: s("Torrentio\nMovie 2160p"),
                    details: SharedString::default(),
                    lines: 2,
                    is_download: false,
                    download_progress: 0.0,
                    download_action: 0,
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_touch_menus(true);
    app.window().show().unwrap();
    idle(400);
    app
}

/// The pill row's absolute position: it moves with the bar's own pan
/// horizontally while staying above the vertical result viewport.
fn row_pos(app: &nova::AppWindow) -> LogicalPosition {
    ElementHandle::find_by_element_id(app, "StreamFilterBar::pill-row")
        .next()
        .expect("filter pill row")
        .absolute_position()
}

/// A drag point inside the bar's viewport, so it never lands on a pill that is
/// scrolled out of sight.
fn bar_point(app: &nova::AppWindow, frac: f32) -> LogicalPosition {
    let e = ElementHandle::find_by_element_id(app, "StreamFilterBar::pill-flick")
        .next()
        .expect("filter pill viewport");
    let (p, sz) = (e.absolute_position(), e.size());
    LogicalPosition::new(p.x + 40.0 + (sz.width - 80.0) * frac, p.y + sz.height / 2.0)
}

fn press(app: &nova::AppWindow, at: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: at,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn release(app: &nova::AppWindow, at: LogicalPosition) {
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: at,
                button: slint::platform::PointerEventButton::Left,
            });
}

/// One move event, then a frame of mock time.
fn frame(app: &nova::AppWindow, at: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved { position: at });
    idle(16);
}

/// A drag spread over `frames` frames, in equal steps.
fn drag_frames(app: &nova::AppWindow, from: LogicalPosition, dx: f32, dy: f32, frames: usize) {
    press(app, from);
    idle(30);
    for i in 1..=frames {
        frame(
            app,
            LogicalPosition::new(
                from.x + dx * i as f32 / frames as f32,
                from.y + dy * i as f32 / frames as f32,
            ),
        );
    }
    release(app, LogicalPosition::new(from.x + dx, from.y + dy));
    idle(16);
}

#[test]
fn addon_swipes_and_result_drags_have_independent_viewports() {
    // One backend per process: every case below runs on its own freshly opened
    // page instead of as a separate `#[test]`.
    i_slint_backend_testing::init_integration_test_with_mock_time();

    // Horizontal drag on the pill row: the row pans, the page does not move.
    {
        let app = setup();
        let before = row_pos(&app);
        drag_frames(&app, bar_point(&app, 0.2), -200.0, 0.0, 8);
        let after = row_pos(&app);
        assert!(
            after.x - before.x < -150.0,
            "the row must pan with the drag (dx {:.1})",
            after.x - before.x
        );
        assert!(
            (after.y - before.y).abs() < 0.5,
            "a horizontal drag must not scroll the page (dy {:.1})",
            after.y - before.y
        );
    }

    // The axis lock proper: a drag that drifts vertically — past the 8px the
    // page needs to claim the gesture — still belongs to the row, because the
    // pills claim it first (see `DetailPage::page_pan_enabled`).
    {
        let app = setup();
        let before = row_pos(&app);

        // Wobble first (1px a frame, under the page's threshold), then a drift
        // far past it — the shape of a thumb swipe that is not perfectly
        // straight.
        let start = bar_point(&app, 0.2);
        press(&app, start);
        idle(30);
        for i in 1..=6 {
            frame(
                &app,
                LogicalPosition::new(start.x - 25.0 * i as f32, start.y - 1.0 * i as f32),
            );
        }
        for i in 7..=12 {
            frame(
                &app,
                LogicalPosition::new(
                    start.x - 25.0 * i as f32,
                    start.y - 6.0 - 5.0 * (i - 6) as f32,
                ),
            );
        }
        release(&app, LogicalPosition::new(start.x - 300.0, start.y - 36.0));
        idle(16);

        let after = row_pos(&app);
        assert!(
            after.x - before.x < -150.0,
            "the row must keep panning despite the drift (dx {:.1})",
            after.x - before.x
        );
        assert!(
            (after.y - before.y).abs() < 0.5,
            "the page must not register the vertical part of a horizontal drag (dy {:.1})",
            after.y - before.y
        );
    }

    // A vertical drag on the fixed header neither pans its horizontal bar
    // nor moves it out of view.
    {
        let app = setup();
        let before = row_pos(&app);
        drag_frames(&app, bar_point(&app, 0.2), 0.0, -120.0, 8);
        let after = row_pos(&app);
        assert!(
            (before.y - after.y).abs() < 0.5,
            "a vertical drag must keep the filter header fixed (dy {:.1})",
            after.y - before.y
        );
        assert!(
            (after.x - before.x).abs() < 0.5,
            "a vertical drag must not pan the row (dx {:.1})",
            after.x - before.x
        );
    }

    // The page itself is unaffected: drags below the pills scroll as before.
    {
        let app = setup();
        let before = row_pos(&app);
        let stream = ElementHandle::find_by_element_id(&app, "StreamList::touch_st")
            .next()
            .expect("first stream row");
        let stream_y = stream.absolute_position().y;
        let start = bar_point(&app, 0.5);
        drag_frames(
            &app,
            LogicalPosition::new(start.x, start.y + 260.0),
            0.0,
            -120.0,
            8,
        );
        assert!(
            stream.absolute_position().y < stream_y - 80.0,
            "results must scroll"
        );
        let after = row_pos(&app);
        assert!(
            (before.y - after.y).abs() < 0.5,
            "result scrolling must leave the filter header fixed (dy {:.1})",
            after.y - before.y
        );
        assert!(
            (after.x - before.x).abs() < 0.5,
            "dragging the page must not pan the pill row (dx {:.1})",
            after.x - before.x
        );
    }
}

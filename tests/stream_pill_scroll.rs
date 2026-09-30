//! Stream filter pill bar scrolling + filtering (headless).
//!
//! The addon pill bar lives in flow with the streams and scrolls horizontally
//! inside its own Flickable; on desktop it also gets step chevrons on both
//! ends, each moving by one viewport without disturbing the vertical scroll.
//! Covers that the chevrons render on desktop overflow only, that stepping
//! pans the pills horizontally (and only horizontally), that a faded chevron
//! is inert, that pill taps still filter, that fitting bars keep their faded
//! chevron slots, and that on touch the one in-flow bar scrolls away with the
//! streams (no docked copy rides over them) while its taps keep filtering.
//!
//! The gesture arbitration between the bar and the page scroll (the pill
//! bar's axis lock) lives in `stream_pill_axis_lock.rs`: it needs time-spread
//! gestures, while these events are dispatched back-to-back.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn tap(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let sz = element.size();
    let c = LogicalPosition::new(p.x + sz.width / 2.0, p.y + sz.height / 2.0);
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: c,
            button: slint::platform::PointerEventButton::Left,
        });
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: c,
                button: slint::platform::PointerEventButton::Left,
            });
}

fn chevrons(app: &nova::AppWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "PageButton").collect()
}

fn pill_pos(app: &nova::AppWindow) -> (f32, f32) {
    let e = ElementHandle::find_by_element_type_name(app, "StreamFilterPill")
        .next()
        .expect("filter pills");
    let p = e.absolute_position();
    (p.x, p.y)
}

/// The pill bar's viewport (its own Flickable), for clipping-aware drags.
fn bar_rect(app: &nova::AppWindow) -> (f32, f32, f32, f32) {
    let e = ElementHandle::find_by_element_id(app, "StreamFilterBar::pill-flick")
        .next()
        .expect("filter pill viewport");
    let p = e.absolute_position();
    let sz = e.size();
    (p.x, p.y, sz.width, sz.height)
}

/// Vertical drag on the page body, well below the pill bar.
fn drag(app: &nova::AppWindow, from: LogicalPosition, dx: f32, dy: f32) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: from,
            button: slint::platform::PointerEventButton::Left,
        });
    for step in 1..=4 {
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(
                        from.x + dx * step as f32 / 4.0,
                        from.y + dy * step as f32 / 4.0,
                    ),
                });
    }
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: LogicalPosition::new(from.x + dx, from.y + dy),
                button: slint::platform::PointerEventButton::Left,
            });
}

#[test]
fn pill_chevrons_step_and_the_touch_bar_scrolls_with_the_streams() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(600, 900));
    app.window().show().unwrap();
    app.set_modal_visible(true);
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

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let filters: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let filters = filters.clone();
        app.on_stream_filter_picked(move |i| filters.borrow_mut().push(i));
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let filters1 = filters.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();

        // Desktop overflow: both chevron slots render.
        let buttons = chevrons(&app);
        fail(
            &failures1,
            buttons.len() == 2,
            &format!("overflow must render 2 chevrons, found {}", buttons.len()),
        );
        if buttons.len() != 2 {
            slint::quit_event_loop().unwrap();
            return;
        }
        let mut buttons = buttons;
        buttons.sort_by(|a, b| {
            a.absolute_position()
                .x
                .partial_cmp(&b.absolute_position().x)
                .unwrap()
        });

        // Track one pill across scrolls by handle: the element query only
        // returns viewport-visible delegates, so positions must be compared
        // on the same element, not across queries.
        let tracked = ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
            .last()
            .expect("pills");
        let tx0 = tracked.absolute_position().x;
        let ty0 = tracked.absolute_position().y;

        // The left chevron starts faded (already at the left edge): clicking
        // it must not move anything.
        tap(&app, &buttons[0]);
        let (tx1, ty1) = (tracked.absolute_position().x, tracked.absolute_position().y);
        fail(
            &failures1,
            (tx1 - tx0).abs() < 0.5 && (ty1 - ty0).abs() < 0.5,
            &format!(
                "faded left chevron must be inert (dx {:.1}, dy {:.1})",
                tx1 - tx0,
                ty1 - ty0
            ),
        );

        // Right chevron: the tracked pill pans left by one viewport while
        // keeping its vertical position (no page drift).
        tap(&app, &buttons[1]);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(250, move || {
            let app = app2.upgrade().unwrap();
            let (_, _, flick_w, _) = bar_rect(&app);
            let (tx2, ty2) = (tracked.absolute_position().x, tracked.absolute_position().y);
            fail(
                &failures2,
                (tx2 - (tx0 - flick_w)).abs() < 1.0,
                &format!(
                    "right chevron must pan by one viewport {flick_w:.1} (dx {:.1})",
                    tx2 - tx0
                ),
            );
            fail(
                &failures2,
                (ty2 - ty0).abs() < 0.5,
                &format!(
                    "chevron step must not move the page vertically (dy {:.1})",
                    ty2 - ty0
                ),
            );

            // Left chevron restores the start position.
            tap(&app, &chevrons(&app)[0]);
            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(250, move || {
                let app = app3.upgrade().unwrap();
                let (tx3, ty3) = (tracked.absolute_position().x, tracked.absolute_position().y);
                fail(
                    &failures3,
                    (tx3 - tx0).abs() < 0.5 && (ty3 - ty0).abs() < 0.5,
                    &format!(
                        "left chevron must restore the bar (dx {:.1}, dy {:.1})",
                        tx3 - tx0,
                        ty3 - ty0
                    ),
                );

                // Pill taps still filter: tap every fully visible pill (a
                // half-clipped edge pill's centre can sit outside the
                // viewport, where the tap misses). The reported set is All +
                // the first addons regardless of query order, and tapping
                // must not scroll.
                let (bar_x, _, bar_w, _) = bar_rect(&app);
                let flick_right = bar_x + bar_w;
                let pills: Vec<_> =
                    ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
                        .filter(|e| {
                            let p = e.absolute_position();
                            p.x + e.size().width / 2.0 < flick_right - 0.5
                        })
                        .collect();
                let n = pills.len();
                fail(&failures3, n >= 3, "DIAG: expected several visible pills");
                for pill in &pills {
                    tap(&app, pill);
                }
                let mut got = filters1.borrow().clone();
                got.sort_unstable();
                let expected: Vec<i32> = (0..n as i32).collect();
                fail(
                    &failures3,
                    got.as_slice() == expected.as_slice(),
                    &format!("pill taps must filter (got {got:?})"),
                );
                let (tx4, ty4) = (tracked.absolute_position().x, tracked.absolute_position().y);
                fail(
                    &failures3,
                    (tx4 - tx0).abs() < 0.5 && (ty4 - ty0).abs() < 0.5,
                    "pill taps must not scroll the bar",
                );

                // Fitting pills: chevrons stay faded (inert); touch layouts
                // drop the slots entirely.
                app.set_stream_addons(Rc::new(VecModel::from(vec![s("A"), s("B")])).into());
                app.window().set_size(slint::PhysicalSize::new(1100, 900));
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                after(400, move || {
                    let app = app4.upgrade().unwrap();
                    let (xa, ya) = pill_pos(&app);
                    let buttons = chevrons(&app);
                    fail(
                        &failures4,
                        buttons.len() == 2,
                        "slots stay laid out when pills fit",
                    );
                    if buttons.len() == 2 {
                        let mut buttons = buttons;
                        buttons.sort_by(|a, b| {
                            a.absolute_position()
                                .x
                                .partial_cmp(&b.absolute_position().x)
                                .unwrap()
                        });
                        tap(&app, &buttons[1]);
                        let (xb, yb) = pill_pos(&app);
                        fail(
                            &failures4,
                            (xb - xa).abs() < 0.5 && (yb - ya).abs() < 0.5,
                            "faded chevron must not scroll a fitting bar",
                        );
                    }
                    app.set_touch_menus(true);
                    app.set_stream_addons(
                        Rc::new(VecModel::from(
                            (0..10)
                                .map(|i| s(&format!("Addon Number {i}")))
                                .collect::<Vec<_>>(),
                        ))
                        .into(),
                    );
                    filters1.borrow_mut().clear();
                    let app5 = app.as_weak();
                    let failures5 = failures4.clone();
                    let filters5 = filters1.clone();
                    after(300, move || {
                        let app = app5.upgrade().unwrap();
                        fail(
                            &failures5,
                            chevrons(&app).is_empty(),
                            "touch layouts must not render chevron slots",
                        );

                        // One bar, in flow with the streams: it sits deep in
                        // the page, and it is the only pill row there is (the
                        // docked copy above the scroll is gone).
                        let rows =
                            ElementHandle::find_by_element_id(&app, "StreamFilterBar::pill-row")
                                .count();
                        fail(
                            &failures5,
                            rows == 1,
                            &format!("touch must render exactly one pill row, found {rows}"),
                        );
                        let pills: Vec<_> =
                            ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
                                .collect();
                        fail(
                            &failures5,
                            pills.len() >= 3,
                            &format!("touch must render in-flow pills, found {}", pills.len()),
                        );
                        let (_, _, bar_w, _) = bar_rect(&app);
                        let min_y = pills
                            .iter()
                            .map(|e| e.absolute_position().y)
                            .fold(f32::MAX, f32::min);
                        fail(
                            &failures5,
                            min_y > 300.0,
                            &format!("the pills must sit in flow (min y {min_y:.0})"),
                        );
                        let (bar_x, _, _, _) = bar_rect(&app);
                        let flick_right = bar_x + bar_w;
                        if let Some(last) = pills.last() {
                            let p = last.absolute_position();
                            fail(
                                &failures5,
                                p.x + last.size().width > flick_right + 1.0,
                                "the touch bar must overflow (last visible pill clipped)",
                            );
                        }

                        // Every visible pill taps to filter (a clipped edge
                        // pill's centre can sit outside the viewport, where
                        // the tap misses). Unscrolled, these are All + the
                        // first addons in order.
                        let seen: Vec<_> =
                            ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
                                .filter(|e| {
                                    let p = e.absolute_position();
                                    p.x + e.size().width / 2.0 < flick_right - 0.5
                                })
                                .collect();
                        let k = seen.len();
                        fail(&failures5, k >= 3, "DIAG: expected several visible pills");
                        for pill in &seen {
                            tap(&app, pill);
                        }
                        let mut got = filters5.borrow().clone();
                        got.sort_unstable();
                        let expected: Vec<i32> = (0..k as i32).collect();
                        fail(
                            &failures5,
                            got.as_slice() == expected.as_slice(),
                            &format!("touch pill taps must filter (got {got:?})"),
                        );
                        filters5.borrow_mut().clear();

                        // Scrolling: the bar rides with the streams instead of
                        // docking over them (the bar's own gestures are the
                        // axis-lock test's subject; these events are
                        // back-to-back, which the Flickables read as a flick
                        // with an unbounded velocity, so only the direction is
                        // asserted).
                        let (_, row_y0) = pill_pos(&app);
                        drag(&app, LogicalPosition::new(550.0, 700.0), 0.0, -200.0);
                        let (_, row_y1) = pill_pos(&app);
                        fail(
                            &failures5,
                            row_y0 - row_y1 > 150.0,
                            &format!(
                                "the bar must scroll with the streams (moved {:.1}px down)",
                                row_y0 - row_y1
                            ),
                        );
                        fail(
                            &failures5,
                            row_y1 > 150.0,
                            &format!("no docked bar may ride over the streams (y {row_y1:.0})"),
                        );
                        fail(
                            &failures5,
                            filters5.borrow().is_empty(),
                            "page drags must not pick a filter",
                        );

                        slint::quit_event_loop().unwrap();
                    });
                });
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "pill bar failures:\n  {}",
        failures.join("\n  ")
    );
}

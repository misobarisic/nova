//! Stream-card layout (headless).
//!
//! Two fixes with one cause each: the addon filter pills must keep a single
//! width while addons are still being queried (the spinner used to add to the
//! pill, so the bar reflowed as answers came in), and the download rail must
//! fill left → right (explicitly anchored, like the app's other rails).

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn stream_row(id: &str, is_download: bool) -> nova::StreamRow {
    nova::StreamRow {
        id: s(id),
        text: s("Torrentio\nMovie 2160p"),
        details: if is_download {
            s("Downloading · 42%")
        } else {
            SharedString::default()
        },
        lines: 2,
        is_download,
        download_progress: if is_download { 0.42 } else { 0.0 },
        download_action: if is_download { 1 } else { 0 },
    }
}

#[test]
fn pills_keep_one_width_and_the_download_rail_fills_left_to_right() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    // Keep the complete selector header and both sample rows visible.
    app.window().set_size(slint::PhysicalSize::new(600, 1200));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_detail_tab(0);
    app.set_modal_episodes(false);
    app.set_selected_title(s("Movie"));
    app.set_stream_addons(Rc::new(VecModel::from(vec![s("Addon A"), s("Addon B")])).into());
    app.set_streams(
        Rc::new(VecModel::from(vec![
            stream_row("download:job-1", true),
            stream_row("stream-1", false),
        ]))
        .into(),
    );
    // Fetching: every pill shows its spinner.
    app.set_streams_searching(true);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        let Some(loading_pill) =
            ElementHandle::find_by_element_type_name(&app, "StreamFilterPill").next()
        else {
            fail(&failures1, false, "DIAG: no filter pills rendered");
            slint::quit_event_loop().unwrap();
            return;
        };
        let loading_width = loading_pill.size().width;
        fail(&failures1, loading_width > 0.0, "DIAG: pill has no width");

        // Fetching finished: the spinner goes away, the width must not change.
        app.set_streams_searching(false);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(250, move || {
            let app = app2.upgrade().unwrap();
            let idle_width = ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
                .next()
                .map(|p| p.size().width)
                .unwrap_or(0.0);
            fail(
                &failures2,
                (loading_width - idle_width).abs() <= 0.5,
                &format!(
                    "a filtering pill must keep one width (loading {loading_width}, idle {idle_width})"
                ),
            );

            // The download rail: anchored to the track's left edge and filling
            // in proportion to progress.
            let bar = ElementHandle::find_by_element_id(&app, "StreamList::dl_bar").next();
            let fill = ElementHandle::find_by_element_id(&app, "StreamList::dl_fill").next();
            match (bar, fill) {
                (Some(bar), Some(fill)) => {
                    let bar_pos = bar.absolute_position();
                    let fill_pos = fill.absolute_position();
                    fail(
                        &failures2,
                        (fill_pos.x - bar_pos.x).abs() <= 0.5,
                        &format!(
                            "the download rail must start at the track's left edge (bar {:.1}, fill {:.1})",
                            bar_pos.x, fill_pos.x
                        ),
                    );
                    let expected = bar.size().width * 0.42;
                    fail(
                        &failures2,
                        (fill.size().width - expected).abs() <= 1.0,
                        &format!(
                            "the download rail must fill by progress (fill {:.1}, expected {expected:.1})",
                            fill.size().width
                        ),
                    );
                }
                _ => fail(&failures2, false, "DIAG: no download rail rendered"),
            }
            slint::quit_event_loop().unwrap();
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "stream card failures:\n  {}",
        failures.join("\n  ")
    );
}

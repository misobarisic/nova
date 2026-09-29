//! Vertical-clipping regression (headless): a stream row whose text wraps on a
//! narrow screen must grow to fit it. The Rust-side `StreamRow.lines` estimate
//! under-counts wrapped lines, so trusting it clipped the row; the hidden
//! measurement must drive the height instead.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn wrapped_stream_row_grows_to_fit() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(320, 700));
    app.window().show().unwrap();

    let text = "Torrentio averylongunbrokenstreamlabelfilenamewithoutanyspaces that must wrap over many lines on this narrow phone width";
    app.set_modal_visible(true);
    app.set_detail_tab(0);
    app.set_modal_episodes(false);
    app.set_selected_title(s("Movie"));
    app.set_selected_description(s(""));
    app.set_streams(
        Rc::new(VecModel::from(vec![nova::StreamRow {
            id: s("stream-1"),
            text: s(text),
            details: s(""),
            // Deliberately too small: the wrapped measurement must override it.
            lines: 1,
            is_download: false,
            download_progress: 0.0,
            download_action: 0,
        }]))
        .into(),
    );

    let app1 = app.as_weak();
    after(400, move || {
        let app = app1.upgrade().unwrap();

        // The row's full-size hit area has the row's height.
        let row = ElementHandle::find_by_element_type_name(&app, "TouchArea")
            .into_iter()
            .find(|e| {
                let p = e.absolute_position();
                let sz = e.size();
                sz.width > 250.0 && sz.height >= 48.0 && p.y > 100.0
            })
            .expect("stream row hit area");
        let row_pos = row.absolute_position();
        let row_size = row.size();

        // The tallest text laid out within the row is the wrapped stream text
        // (the visible copy and the hidden measurement copy agree).
        let wrapped_h = ElementQuery::from_root(&app)
            .match_type_name("Text")
            .find_all()
            .into_iter()
            .filter_map(|e| {
                let p = e.absolute_position();
                let inside =
                    p.y >= row_pos.y - 1.0 && p.y <= row_pos.y + row_size.height + 1.0;
                inside.then_some(e.size().height)
            })
            .fold(0.0_f32, f32::max);

        assert!(
            wrapped_h > 30.0,
            "DIAG: stream text did not wrap at 320px (tallest text h={wrapped_h})"
        );
        assert!(
            row_size.height >= wrapped_h + 18.0,
            "stream row clipped: row height {} vs wrapped text {wrapped_h}",
            row_size.height
        );

        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();
}

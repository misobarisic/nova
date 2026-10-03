//! Vertical-clipping regression for `SettingsRow` (headless): a downloaded-
//! episode row with a wrapping title and description must grow to fit both.
//! The old height reserved only a single title line, so a wrapping series name
//! clipped the description on narrow screens.

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// The smallest `Rectangle` that fully contains `target`, i.e. the row.
fn enclosing_rect(
    app: &nova::AppWindow,
    target: (f32, f32, f32, f32),
) -> Option<(f32, f32, f32, f32)> {
    ElementQuery::from_root(app)
        .match_inherits("Rectangle")
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let p = e.absolute_position();
            let sz = e.size();
            // Skip the target element itself (e.g. the button), only larger
            // containers qualify.
            let same = (p.x - target.0).abs() < 1.0
                && (p.y - target.1).abs() < 1.0
                && (sz.width - target.2).abs() < 1.0
                && (sz.height - target.3).abs() < 1.0;
            if same {
                return None;
            }
            let contains = p.x <= target.0 + 0.5
                && p.y <= target.1 + 0.5
                && p.x + sz.width >= target.0 + target.2 - 0.5
                && p.y + sz.height >= target.1 + target.3 - 0.5;
            contains.then_some((sz.width * sz.height, p.x, p.y, sz.width, sz.height))
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .map(|(_, x, y, w, h)| (x, y, w, h))
}

#[test]
fn wrapped_download_row_grows_to_fit() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(320, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_downloads_rows(
        Rc::new(VecModel::from(vec![nova::DownloadRow {
            id: s("d1"),
            title: s("An Extremely Long Series Title That Must Wrap Over Several Lines"),
            subtitle: s("S1 E1 · An Episode Title That Is Also Quite Long · 1080p"),
            details: s("a-very-long-episode-file-name.mkv · 1.2 GB"),
        }]))
        .into(),
    );

    let app1 = app.as_weak();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Downloads is the 9th landing entry (index 8).
        let Some(downloads) = ({
            app.set_settings_search_query("Downloads".into());
            destinations::find(&app, "settings:downloads").next()
        }) else {
            slint::quit_event_loop().unwrap();
            panic!("DIAG: no Downloads landing entry");
        };
        let app2 = app.as_weak();
        slint::spawn_local(async move {
            downloads
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let Some(view) =
                    ElementHandle::find_by_element_type_name(&app, "PillButton").next()
                else {
                    slint::quit_event_loop().unwrap();
                    panic!("DIAG: no Downloaded episodes link");
                };
                let app3 = app.as_weak();
                slint::spawn_local(async move {
                    view.single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = app3.upgrade().unwrap();

                        // The only Delete button lives in the one row.
                        let delete =
                            ElementHandle::find_by_element_type_name(&app, "PillButton")
                                .into_iter()
                                .find(|e| {
                                    let sz = e.size();
                                    sz.width < 120.0 && sz.height < 60.0
                                })
                                .expect("Delete button");
                        let dp = delete.absolute_position();
                        let ds = delete.size();
                        let (_rx, ry, _rw, rh) =
                            enclosing_rect(&app, (dp.x, dp.y, ds.width, ds.height))
                                .expect("enclosing row rectangle");

                        // Title and description appear twice each (visible +
                        // hidden measurement); the two tallest are the wrapped
                        // title and description, whose sum the row must hold.
                        let mut heights: Vec<f32> = ElementQuery::from_root(&app)
                            .match_type_name("Text")
                            .find_all()
                            .into_iter()
                            .filter_map(|e| {
                                let p = e.absolute_position();
                                let inside = p.y >= ry - 1.0 && p.y <= ry + rh + 1.0;
                                inside.then_some(e.size().height)
                            })
                            .collect();
                        heights.sort_by(|a, b| b.partial_cmp(a).unwrap());
                        let wrapped_total: f32 = heights.iter().take(2).sum();

                        assert!(
                            wrapped_total > 40.0,
                            "DIAG: title+description did not wrap (heights={heights:?})"
                        );
                        assert!(
                            rh >= wrapped_total + 20.0,
                            "download row clipped: row height {rh} vs wrapped text {wrapped_total} (heights={heights:?})"
                        );
                        slint::quit_event_loop().unwrap();
                    });
                })
                .unwrap();
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();
}

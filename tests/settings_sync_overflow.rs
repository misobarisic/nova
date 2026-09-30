//! Settings → Sync subpage horizontal-overflow regression test (headless).
//!
//! The Sync subpage renders long unbreakable tokens: the endpoint id, the
//! invite ticket (`NV1` + base32, ~82 chars) and status/error lines that embed
//! endpoint ids. Wrapped text reports its longest token as its minimum width,
//! so without `min-width: 0` these would widen the whole settings content
//! layer and let the page pan sideways. Asserts nothing extends past the
//! window edge at phone widths.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Rightmost edge over elements whose left edge is on screen. The inactive
/// landing layer (slid left) is ignored by the `x` bound; a real overflow
/// keeps its left edge visible while its right edge runs past the window.
fn max_right_edge(app: &nova::AppWindow) -> f32 {
    let width = app.window().size().width as f32;
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let sz = e.size();
            let p = e.absolute_position();
            (p.x >= 0.0 && p.x < width && sz.width > 0.0).then_some(p.x + sz.width)
        })
        .fold(0.0f32, f32::max)
}

#[test]
fn settings_sync_subpage_has_no_horizontal_overflow() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_sync_enabled(true);
    // Exercise the Android-only scanner button layout too.
    app.set_sync_qr_scan_available(true);
    let long_id = "a".repeat(64);
    app.set_sync_identity(s(long_id.as_str()));
    app.set_sync_invite_ticket(s(
        "NV1MFRGGZDFMZTWQ2LKNNWG23TPOBYXE43UOWLZVCVNBQWY5DFMFRGGZDFMZTWQ2LKNNWG23T",
    ));
    app.set_sync_status(s(&format!(
        "Last sync failed: connect to {long_id} timed out"
    )));
    app.set_sync_link_notice(s(
        "Paired with a-device-with-an-extremely-long-name-for-testing",
    ));
    app.set_sync_peers(
        Rc::new(VecModel::from(vec![nova::SyncPeer {
            id: s(long_id.as_str()),
            name: s("A device with an extremely long name for testing the layout"),
            last_seen: s("Last connected 2m ago"),
        }]))
        .into(),
    );

    let failures: Rc<std::cell::RefCell<Vec<String>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Sync is the last landing entry: addons, categories, cache, display,
        // p2p, player, look and feel, sync.
        let sync_link = ElementHandle::find_by_element_type_name(&app, "SettingsLink")
            .nth(7)
            .expect("Sync landing entry");
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            sync_link
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let edge = max_right_edge(&app);
                if edge > 361.0 {
                    failures2.borrow_mut().push(format!(
                        "360px sync: content reaches {edge:.1}, want <= 360"
                    ));
                }
                app.window().set_size(slint::PhysicalSize::new(320, 800));
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    let edge = max_right_edge(&app);
                    if edge > 321.0 {
                        failures3.borrow_mut().push(format!(
                            "320px sync: content reaches {edge:.1}, want <= 320"
                        ));
                    }
                    // Translated labels run longer than their English sources
                    // ("Join" → "Pridružite se", the invite ticket heading),
                    // and a card heading that cannot wrap reports its full
                    // single-line width as the page minimum: re-check the
                    // narrowest width in Croatian. Skipped in the interpreter
                    // build, which bundles no catalogs.
                    if slint::select_bundled_translation("hr").is_err() {
                        slint::quit_event_loop().unwrap();
                        return;
                    }
                    app.set_sync_status(s(&format!(
                        "Posljednja sinkronizacija nije uspjela: veza s {long_id} je istekla"
                    )));
                    app.set_sync_link_notice(s("Upareno s uređajem s-izuzetno-dugackim-nazivom"));
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        let edge = max_right_edge(&app);
                        if edge > 321.0 {
                            failures4.borrow_mut().push(format!(
                                "hr 320px sync: content reaches {edge:.1}, want <= 320"
                            ));
                        }
                        slint::quit_event_loop().unwrap();
                    });
                });
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "overflow failures:\n  {}",
        failures.join("\n  ")
    );
}

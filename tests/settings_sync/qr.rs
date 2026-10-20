//! Settings → Sync invite QR (headless).
//!
//! Guards the Android scanner wiring: with the scanner available the Sync
//! subpage shows a "Scan QR code" button, and tapping it dispatches
//! `sync_scan_qr` (which Rust maps to launching `QrScanActivity`). The QR
//! image itself is covered by the `src/app/qr.rs` round-trip unit test.
//! One test function: the testing backend initializes once per process.

#[path = "../support/settings.rs"]
mod settings_support;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn sync_scan_qr_button_dispatches() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "sync_scan_qr_button_dispatches"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    // Use a tall phone viewport, and still reveal Scan through real scrolling:
    // wrapped Sync descriptions and the invite QR can exceed its height.
    app.window().set_size(slint::PhysicalSize::new(360, 2400));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_sync_enabled(true);
    app.set_sync_qr_scan_available(true);
    // A ticket makes the QR + Copy code button render too.
    app.set_sync_invite_ticket(s(
        "NV1MFRGGZDFMZTWQ2LKNNWG23TPOBYXE43UOWLZVCVNBQWY5DFMFRGGZDFMZTWQ2LKNNWG23T",
    ));

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let scans = Rc::new(RefCell::new(0usize));
    {
        let scans = scans.clone();
        app.on_sync_scan_qr(move || *scans.borrow_mut() += 1);
    }

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let scans1 = scans.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Use the production More → Sync route. Keeping both More and
        // Settings visible creates an overlapping fixture that can intercept
        // the landing click; navigation itself is covered by its own suite.
        app.invoke_more_bridge_sync_picked();
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let scans2 = scans1.clone();
        after(400, move || {
            let app = app2.upgrade().unwrap();
            slint::spawn_local(async move {
                let mut scan = None;
                for _ in 0..12 {
                    // A headless backend does not render frames. Measure the
                    // nested leaf so it can publish its full scroll height.
                    for element in ElementQuery::from_root(&app).match_predicate(|_| true).find_all() {
                        let _ = (element.size(), element.absolute_position());
                    }
                    scan = ElementHandle::find_by_accessible_label(&app, "Scan QR code")
                        .find(|element| element.accessible_role() == Some(AccessibleRole::Button));
                    if scan.is_some() { break; }
                    app.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(180.0, 1200.0),
                        delta_x: 0.0,
                        delta_y: -700.0,
                    });
                    settings_support::settle().await;
                }
                let Some(scan) = scan else {
                    let bridge = app.global::<nova::SettingsBridge>();
                    failures2.borrow_mut().push(format!(
                        "Sync subpage must show the Scan QR code button after scrolling; selected={}, detail_open={}, sync_enabled={}, scanner_available={}, bridged_enabled={}, bridged_scanner={}, scroll_y={}",
                        app.get_settings_selected_id(), app.get_settings_detail_open(), app.get_sync_enabled(), app.get_sync_qr_scan_available(),
                        bridge.get_sync_enabled(), bridge.get_sync_qr_scan_available(), app.get_settings_scroll_y()
                    ));
                    slint::quit_event_loop().unwrap();
                    return;
                };
                settings_support::click(&app, &scan).await;
                if *scans2.borrow() != 1 {
                    failures2.borrow_mut().push("tapping Scan QR must dispatch sync_scan_qr".to_string());
                }
                slint::quit_event_loop().unwrap();
            }).unwrap();
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "settings qr failures:\n  {}",
        failures.join("\n  ")
    );
}

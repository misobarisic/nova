//! Settings → Sync invite QR (headless).
//!
//! Guards the Android scanner wiring: with the scanner available the Sync
//! subpage shows a "Scan QR code" button, and tapping it dispatches
//! `sync_scan_qr` (which Rust maps to launching `QrScanActivity`). The QR
//! image itself is covered by the `src/app/qr.rs` round-trip unit test.
//! One test function: the testing backend initializes once per process.

#[path = "support/settings.rs"]
mod settings_support;

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::ElementHandle;
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
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    // Tall window so the whole Sync subpage (Scan sits near its end) is on
    // screen; clicking an element scrolled out of view would hit whatever
    // overlay is at its coordinates.
    app.window().set_size(slint::PhysicalSize::new(360, 2400));
    app.window().show().unwrap();
    app.set_show_settings(true);
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
        // Sync is the last landing entry (addons, categories, cache, display,
        // p2p, player, look and feel, sync).
        let Some(sync_link) = ({
            app.set_settings_search_query("Sync".into());
            destinations::find(&app, "settings:sync").next()
        }) else {
            failures1
                .borrow_mut()
                .push("DIAG: no Sync landing entry".to_string());
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let scans2 = scans1.clone();
        slint::spawn_local(async move {
            sync_link
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let Some(scan) =
                    ElementHandle::find_by_accessible_label(&app, "Scan QR code").next()
                else {
                    failures2
                        .borrow_mut()
                        .push("Sync subpage must show the Scan QR code button".to_string());
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let failures3 = failures2.clone();
                let scans3 = scans2.clone();
                slint::spawn_local(async move {
                    settings_support::click(&app, &scan).await;
                    if *scans3.borrow() != 1 {
                        failures3
                            .borrow_mut()
                            .push("tapping Scan QR must dispatch sync_scan_qr".to_string());
                    }
                    slint::quit_event_loop().unwrap();
                })
                .unwrap();
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "settings qr failures:\n  {}",
        failures.join("\n  ")
    );
}

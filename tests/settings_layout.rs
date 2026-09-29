//! Settings subpages stay top-aligned (headless).
//!
//! Layouts distribute free vertical space to children by `vertical-stretch`,
//! so an unpinned row absorbs the slack and stretches (see the DiscoverPage
//! header note). Settings pages used to stretch their blank space into the
//! cards; this asserts a short subpage (Categories with no entries) keeps its
//! card at content height, top-aligned, instead of filling the screen — and
//! that a long subpage (Sync with many peers) still scrolls.
//! One test function: the testing backend initializes once per process.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn back(app: &nova::AppWindow) {
    app.window().dispatch_event(slint::platform::WindowEvent::KeyPressed {
        text: slint::platform::Key::Back.into(),
    });
}

fn press(app: &nova::AppWindow, position: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn moved(app: &nova::AppWindow, position: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved { position });
}

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn short_settings_subpage_is_top_aligned_not_stretched() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    // Zero categories: the empty state is the shortest possible subpage.
    // A long Sync peer list backs the scroll phase below.
    app.set_sync_enabled(true);
    app.set_sync_peers(
        Rc::new(VecModel::from(
            (0..14)
                .map(|i| nova::SyncPeer {
                    id: s(&format!("peer-{i:02}")),
                    name: s(&format!("Device {i}")),
                    last_seen: s("Last connected 2m ago"),
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

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Categories is the 2nd landing entry (index 1).
        let Some(categories) =
            ElementHandle::find_by_element_type_name(&app, "SettingsLink").nth(1)
        else {
            fail(&failures1, false, "DIAG: no Categories landing entry");
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            categories
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let window_h = app.window().size().height as f32;
                if let Some(card) =
                    ElementHandle::find_by_element_id(&app, "SettingsPage::cat_card").next()
                {
                    let pos = card.absolute_position();
                    let size = card.size();
                    fail(
                        &failures2,
                        size.height < 420.0,
                        &format!(
                            "Categories card must stay content-height, got {:.0}px (window {window_h:.0})",
                            size.height
                        ),
                    );
                    fail(
                        &failures2,
                        pos.y < 260.0,
                        &format!("Categories card must be top-aligned, got y={:.0}", pos.y),
                    );
                    fail(
                        &failures2,
                        pos.y + size.height < window_h - 120.0,
                        &format!(
                            "Categories card must not reach the bottom, bottom at {:.0} of {window_h:.0}",
                            pos.y + size.height
                        ),
                    );
                } else {
                    fail(&failures2, false, "DIAG: no Categories card");
                }

                // ---- Long page still scrolls: Sync with 14 peers. ----
                back(&app);
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    // Sync is the 8th landing entry (index 7).
                    let Some(sync) =
                        ElementHandle::find_by_element_type_name(&app, "SettingsLink").nth(7)
                    else {
                        fail(&failures3, false, "DIAG: no Sync landing entry");
                        slint::quit_event_loop().unwrap();
                        return;
                    };
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    slint::spawn_local(async move {
                        sync.single_click(slint::platform::PointerEventButton::Left)
                            .await;
                        after(400, move || {
                            let app = app4.upgrade().unwrap();
                            // Drag empty row space (center-left, clear of the
                            // Remove buttons) upwards.
                            let start = LogicalPosition::new(120.0, 480.0);
                            press(&app, start);
                            for dy in [10.0, 40.0, 90.0, 150.0] {
                                moved(&app, LogicalPosition::new(120.0, 480.0 - dy));
                            }
                            release(&app, LogicalPosition::new(120.0, 330.0));
                            fail(
                                &failures4,
                                app.get_settings_scroll_y() != 0.0,
                                "a long settings subpage must still scroll",
                            );
                            slint::quit_event_loop().unwrap();
                        });
                    })
                    .unwrap();
                });
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "settings layout failures:\n  {}",
        failures.join("\n  ")
    );
}

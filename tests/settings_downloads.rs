//! Settings → Downloads subpage + downloaded-episodes list (headless).
//!
//! Guards two things: the Downloads subpage actually renders (its auto-delete
//! toggle must be instantiated — a nesting slip once hid the whole subpage
//! behind the Sync section), and the nested "Downloaded episodes" list opens,
//! lists completed episodes and dispatches a remove for the tapped row.
//! One test function: the testing backend initializes once per process.

#[path = "support/settings.rs"]
mod settings_support;

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn back(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn downloads_subpage_shows_auto_delete_and_lists_episodes() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_download_auto_delete_watched(true);
    app.set_downloads_rows(
        Rc::new(VecModel::from(vec![
            nova::DownloadRow {
                id: s("d1"),
                title: s("Show One"),
                subtitle: s("S1 E1 · 1080p"),
                details: s("ep1.mp4 · 10 MB"),
            },
            nova::DownloadRow {
                id: s("d2"),
                title: s("Show Two"),
                subtitle: s("S2 E3 · 720p"),
                details: s("ep2.mp4 · 20 MB"),
            },
        ]))
        .into(),
    );

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let removed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let removed = removed.clone();
        app.on_download_list_remove(move |id| removed.borrow_mut().push(id.to_string()));
    }
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    let removed1 = removed.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Downloads is the 9th landing entry (index 8).
        let Some(downloads) = ({
            app.set_settings_search_query("Downloads".into());
            destinations::find(&app, "settings:downloads").next()
        }) else {
            fail(&failures1, false, "DIAG: no Downloads landing entry");
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let removed2 = removed1.clone();
        slint::spawn_local(async move {
            downloads
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                // The subpage must actually render: the auto-delete toggle is
                // the regression that matters (it was nested under Sync).
                let toggles =
                    ElementHandle::find_by_element_type_name(&app, "ToggleSwitch").count();
                fail(
                    &failures2,
                    toggles >= 1,
                    "Downloads subpage must show the auto-delete toggle",
                );
                let Some(view) = ElementHandle::find_by_accessible_label(&app, "View").find(|e| {
                    e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
                }) else {
                    fail(&failures2, false, "DIAG: no Downloaded episodes link");
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                let removed3 = removed2.clone();
                slint::spawn_local(async move {
                    settings_support::click(&app, &view).await;
                    after(400, move || {
                        let app = app3.upgrade().unwrap();
                        // Two Delete buttons, one per listed episode.
                        let deletes: Vec<_> =
                            ElementHandle::find_by_accessible_label(&app, "Delete")
                                .filter(|e| {
                                    e.accessible_role()
                                        == Some(i_slint_backend_testing::AccessibleRole::Button)
                                })
                                .collect();
                        fail(
                            &failures3,
                            deletes.len() == 2,
                            "downloaded-episodes list must show one row per episode",
                        );
                        let app4 = app.as_weak();
                        let failures4 = failures3.clone();
                        let removed4 = removed3.clone();
                        slint::spawn_local(async move {
                            if let Some(first) = deletes.first() {
                                first
                                    .single_click(slint::platform::PointerEventButton::Left)
                                    .await;
                            }
                            fail(
                                &failures4,
                                removed4.borrow().len() == 1 && removed4.borrow()[0] == "d1",
                                "tapping Delete must remove that episode",
                            );

                            // Back returns to the Downloads subpage (not the
                            // landing): the auto-delete toggle is visible again.
                            let app5 = app4.upgrade().unwrap();
                            back(&app5);
                            after(400, move || {
                                let app = app4.upgrade().unwrap();
                                let toggles =
                                    ElementHandle::find_by_element_type_name(&app, "ToggleSwitch")
                                        .count();
                                fail(
                                    &failures4,
                                    toggles >= 1,
                                    "back from the list must return to Downloads",
                                );
                                slint::quit_event_loop().unwrap();
                            });
                        })
                        .unwrap();
                    });
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
        "settings downloads failures:\n  {}",
        failures.join("\n  ")
    );
}

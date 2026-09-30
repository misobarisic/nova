//! Settings → About → Licenses (headless).
//!
//! The About page keeps the generated license catalog behind its own nested
//! page, and source-link buttons stay vertically centered in their rows on
//! narrow and wide layouts. One test function: the testing backend initializes
//! once per process.

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

fn back(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
}

fn check_alignment(app: &nova::AppWindow, label: &str, failures: &Rc<RefCell<Vec<String>>>) {
    let rows: Vec<_> =
        ElementHandle::find_by_element_id(app, "SettingsPage::source_card").collect();
    let buttons: Vec<_> =
        ElementHandle::find_by_element_id(app, "SettingsPage::source_view_button").collect();
    if rows.len() != 2 || buttons.len() != 2 {
        failures.borrow_mut().push(format!(
            "{label}: expected two source rows/buttons, found {} rows and {} buttons",
            rows.len(),
            buttons.len()
        ));
        return;
    }

    for (index, (row, button)) in rows.iter().zip(&buttons).enumerate() {
        let row_y = row.absolute_position().y;
        let row_x = row.absolute_position().x;
        let row_w = row.size().width;
        let row_h = row.size().height;
        let button_x = button.absolute_position().x;
        let button_y = button.absolute_position().y;
        let button_w = button.size().width;
        let button_h = button.size().height;
        let row_center = row_y + row_h / 2.0;
        let button_center = button_y + button_h / 2.0;
        if (row_center - button_center).abs() > 1.0 {
            failures.borrow_mut().push(format!(
                "{label}: source button {index} center y={button_center:.1} differs from row center y={row_center:.1}"
            ));
        }
        if button_x < row_x - 0.5
            || button_x + button_w > row_x + row_w + 0.5
            || button_y < row_y - 0.5
            || button_y + button_h > row_y + row_h + 0.5
        {
            failures.borrow_mut().push(format!(
                "{label}: source button {index} at x={button_x:.1} y={button_y:.1} w={button_w:.1} h={button_h:.1} escapes row x={row_x:.1} y={row_y:.1} w={row_w:.1} h={row_h:.1}"
            ));
        }
    }
}

#[test]
fn about_opens_nested_licenses_and_centers_source_buttons() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_license_catalog("Test license catalog contents".into());
    app.set_license_sources(
        Rc::new(VecModel::from(vec![
            nova::LicenseSource {
                label: s("crate-one"),
                license: s("MIT"),
                url: s("https://example.com/crate-one"),
            },
            nova::LicenseSource {
                label: s("crate-two"),
                license: s("Apache-2.0"),
                url: s("https://example.com/crate-two"),
            },
        ]))
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
        // About is the final Settings landing entry (after Downloads).
        let Some(about) = ElementHandle::find_by_element_type_name(&app, "SettingsLink").nth(9)
        else {
            fail(&failures1, false, "DIAG: no About landing entry");
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            about
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let Some(licenses) =
                    ElementHandle::find_by_accessible_label(&app, "Licenses").next()
                else {
                    fail(&failures2, false, "About page must offer a Licenses button");
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                slint::spawn_local(async move {
                    licenses
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = app3.upgrade().unwrap();
                        let source_buttons = ElementHandle::find_by_element_id(
                            &app,
                            "SettingsPage::source_view_button",
                        )
                        .count();
                        fail(
                            &failures3,
                            source_buttons == 2,
                            "Licenses page must contain both source links",
                        );
                        check_alignment(&app, "narrow layout", &failures3);

                        // Resize without leaving the nested page so the same
                        // source rows are measured at desktop button height.
                        app.window().set_size(slint::PhysicalSize::new(900, 750));
                        let app4 = app.as_weak();
                        let failures4 = failures3.clone();
                        after(300, move || {
                            let app = app4.upgrade().unwrap();
                            check_alignment(&app, "wide layout", &failures4);

                            back(&app);
                            let app5 = app.as_weak();
                            let failures5 = failures4.clone();
                            after(400, move || {
                                let app = app5.upgrade().unwrap();
                                fail(
                                    &failures5,
                                    ElementHandle::find_by_element_id(
                                        &app,
                                        "SettingsPage::about_licenses_button",
                                    )
                                    .count()
                                        == 1,
                                    "Back from Licenses must return to About",
                                );

                                back(&app);
                                let app6 = app.as_weak();
                                let failures6 = failures5.clone();
                                after(400, move || {
                                    let app = app6.upgrade().unwrap();
                                    fail(
                                        &failures6,
                                        ElementHandle::find_by_element_type_name(
                                            &app,
                                            "SettingsLink",
                                        )
                                        .count()
                                            == 10
                                            && ElementHandle::find_by_element_id(
                                                &app,
                                                "SettingsPage::about_licenses_button",
                                            )
                                            .count()
                                                == 0,
                                        "Back from About must return to the Settings landing menu",
                                    );
                                    slint::quit_event_loop().unwrap();
                                });
                            });
                        });
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
        "settings About failures:\n  {}",
        failures.join("\n  ")
    );
}

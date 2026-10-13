//! Standalone About → Licenses (headless).
//!
//! The About page keeps the generated license catalog behind its own nested
//! page, and source-link buttons stay vertically centered in their rows on
//! narrow and wide layouts. One test function: the testing backend initializes
//! once per process.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn back(app: &nova::AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn settle(app: &nova::AppWindow) {
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(400));
    let _ = ElementHandle::find_by_element_type_name(app, "AboutPage")
        .next()
        .map(|e| e.size());
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let size = element.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn drag_horizontal(app: &nova::AppWindow, from: LogicalPosition, to: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: from,
            button: slint::platform::PointerEventButton::Left,
        });
    for step in 1..=4 {
        let t = step as f32 / 4.0;
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(
                        from.x + (to.x - from.x) * t,
                        from.y + (to.y - from.y) * t,
                    ),
                });
    }
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: to,
                button: slint::platform::PointerEventButton::Left,
            });
}

fn check_alignment(app: &nova::AppWindow, label: &str, failures: &Rc<RefCell<Vec<String>>>) {
    let rows: Vec<_> = ElementHandle::find_by_element_id(app, "AboutPage::source_card").collect();
    let buttons: Vec<_> =
        ElementHandle::find_by_element_id(app, "AboutPage::source_view_button").collect();
    if rows.len() != 2 || buttons.len() != 2 {
        failures.borrow_mut().push(format!(
            "{label}: expected two source rows/buttons, found {} rows and {} buttons",
            rows.len(),
            buttons.len()
        ));
        return;
    }

    let viewport_width = app.window().size().width as f32;
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
        if row_x < -0.5 || row_x + row_w > viewport_width + 0.5 {
            failures.borrow_mut().push(format!(
                "{label}: source row {index} at x={row_x:.1} w={row_w:.1} exceeds viewport width {viewport_width:.1}"
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
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "about_opens_nested_licenses_and_centers_source_buttons"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 1600));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_about(true);
    let weak = app.as_weak();
    app.on_home_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_about(false);
        app.set_show_home(true);
    });
    let opened = Rc::new(RefCell::new(Vec::new()));
    let urls = opened.clone();
    app.on_license_source_requested(move |url| urls.borrow_mut().push(url.to_string()));
    // Preserve intrinsic-width and source-row geometry coverage with long
    // generated-style crate names and legal text, including unbroken tokens.
    let mut license_text = (0..64)
        .map(|_| "A long license clause with attributed contributors and redistribution terms.")
        .collect::<Vec<_>>()
        .join("\n");
    license_text.push('\n');
    license_text.push_str(&"X".repeat(128));
    app.set_license_catalog(s(&license_text));
    app.set_license_sources(
        Rc::new(VecModel::from(vec![
            nova::LicenseSource {
                label: s(&"crate-one".repeat(32)),
                license: s(&"License".repeat(24)),
                url: s("https://example.com/crate-one"),
            },
            nova::LicenseSource {
                label: s(&"crate-two".repeat(32)),
                license: s(&"License".repeat(24)),
                url: s("https://example.com/crate-two"),
            },
        ]))
        .into(),
    );
    let failures = Rc::new(RefCell::new(Vec::new()));
    settle(&app);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "BottomNav")
            .next()
            .is_none()
    );
    let licenses = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .unwrap();
    click(&app, &licenses);
    settle(&app);
    assert!(app.get_about_licenses_open());
    check_alignment(&app, "narrow layout", &failures);
    let row = ElementHandle::find_by_element_id(&app, "AboutPage::source_card")
        .next()
        .unwrap();
    let before_x = row.absolute_position().x;
    let y = row.absolute_position().y + row.size().height / 2.0;
    drag_horizontal(
        &app,
        LogicalPosition::new(336.0, y),
        LogicalPosition::new(176.0, y),
    );
    let row = ElementHandle::find_by_element_id(&app, "AboutPage::source_card")
        .next()
        .unwrap();
    assert!(
        (row.absolute_position().x - before_x).abs() <= 1.0,
        "long legal text must not pan horizontally"
    );
    let source = ElementHandle::find_by_element_id(&app, "AboutPage::source_view_button")
        .next()
        .unwrap();
    click(&app, &source);
    assert_eq!(*opened.borrow(), ["https://example.com/crate-one"]);
    // Resize while inside Licenses; the rail appears without losing the page.
    app.window().set_size(slint::PhysicalSize::new(900, 750));
    settle(&app);
    assert!(app.get_about_licenses_open());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SideItem").count(),
        6
    );
    check_alignment(&app, "wide layout", &failures);
    back(&app);
    settle(&app);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
    assert!(
        ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
            .next()
            .is_some()
    );
    back(&app);
    settle(&app);
    assert!(app.get_show_home() && !app.get_show_about());
    assert!(
        failures.borrow().is_empty(),
        "About geometry failures: {:?}",
        failures.borrow()
    );
}

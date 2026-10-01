//! Crowded Settings rows adapt to the available width (headless).
//!
//! Wide segmented controls stack below their title/description when keeping
//! them on the right would leave the text column too narrow. At wide widths
//! they stay beside the text, and the controls remain inside their rows.

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

type Rect = (f32, f32, f32, f32);

fn rect(element: &ElementHandle) -> Rect {
    let p = element.absolute_position();
    let size = element.size();
    (p.x, p.y, size.width, size.height)
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

fn row_for(app: &nova::AppWindow, target: Rect) -> Option<Rect> {
    let (tx, ty, tw, th) = target;
    let center = (tx + tw / 2.0, ty + th / 2.0);
    ElementHandle::find_by_element_type_name(app, "SettingsRow")
        .map(|row| rect(&row))
        .filter(|(x, y, w, h)| {
            center.0 >= *x - 0.5
                && center.0 <= x + w + 0.5
                && center.1 >= *y - 0.5
                && center.1 <= y + h + 0.5
        })
        .min_by(|a, b| (a.2 * a.3).partial_cmp(&(b.2 * b.3)).unwrap())
}

fn check_segmented_rows(
    app: &nova::AppWindow,
    label: &str,
    expected: usize,
    stacked: bool,
    failures: &Rc<RefCell<Vec<String>>>,
) {
    let controls: Vec<_> = ElementHandle::find_by_element_type_name(app, "Segmented")
        .filter(|e| {
            let size = e.size();
            size.width > 0.0 && size.height > 0.0
        })
        .collect();
    if controls.len() != expected {
        failures.borrow_mut().push(format!(
            "{label}: expected {expected} segmented controls, found {}",
            controls.len()
        ));
        return;
    }

    let viewport = app.window().size().width as f32;
    for (index, control) in controls.iter().enumerate() {
        let control = rect(control);
        let Some(row) = row_for(app, control) else {
            failures.borrow_mut().push(format!(
                "{label}: segmented control {index} has no SettingsRow"
            ));
            continue;
        };
        let (x, y, width, height) = control;
        let (row_x, row_y, row_width, row_height) = row;
        if x < row_x + 14.0
            || y < row_y - 0.5
            || x + width > row_x + row_width - 14.0 + 0.5
            || y + height > row_y + row_height + 0.5
            || x + width > viewport + 0.5
        {
            failures.borrow_mut().push(format!(
                "{label}: segmented control {index} at {control:?} escapes row {row:?} or viewport"
            ));
        }

        let control_center = y + height / 2.0;
        let row_center = row_y + row_height / 2.0;
        if stacked && control_center <= row_center + 4.0 {
            failures.borrow_mut().push(format!(
                "{label}: segmented control {index} should sit below its text, control {control:?}, row {row:?}"
            ));
        } else if !stacked && (control_center - row_center).abs() > 2.0 {
            failures.borrow_mut().push(format!(
                "{label}: wide segmented control {index} is not vertically centered, control {control:?}, row {row:?}"
            ));
        }
    }
}

#[test]
fn crowded_settings_controls_stack_on_narrow_windows() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(320, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Display is the 4th landing entry (index 3).
        let Some(display) = ElementHandle::find_by_element_type_name(&app, "SettingsLink").nth(3)
        else {
            failures1
                .borrow_mut()
                .push("missing Display landing entry".into());
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            display
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                check_segmented_rows(&app, "320px Display", 2, true, &failures2);

                app.window().set_size(slint::PhysicalSize::new(360, 800));
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(300, move || {
                    let app = app3.upgrade().unwrap();
                    check_segmented_rows(&app, "360px Display", 2, true, &failures3);

                    // Croatian labels are wider; the rows should adapt without
                    // forcing their pickers back beside a cramped text column.
                    if slint::select_bundled_translation("hr").is_ok() {
                        after(200, move || {
                            let app = app3.upgrade().unwrap();
                            check_segmented_rows(
                                &app,
                                "Croatian 360px Display",
                                2,
                                true,
                                &failures3,
                            );
                            continue_to_player(app, failures3);
                        });
                    } else {
                        continue_to_player(app, failures3);
                    }
                });
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "responsive Settings row failures:\n  {}",
        failures.join("\n  ")
    );
}

fn continue_to_player(app: nova::AppWindow, failures: Rc<RefCell<Vec<String>>>) {
    // Restore the source language when available, then verify the wide layout
    // still keeps controls beside their text.
    let _ = slint::select_bundled_translation("en");
    app.window().set_size(slint::PhysicalSize::new(1100, 800));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        check_segmented_rows(&app, "1100px Display", 3, false, &failures1);
        back(&app);

        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(400, move || {
            let app = app2.upgrade().unwrap();
            app.window().set_size(slint::PhysicalSize::new(320, 800));
            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(300, move || {
                let app = app3.upgrade().unwrap();
                // Player follows the P2P entry (index 5).
                let Some(player) =
                    ElementHandle::find_by_element_type_name(&app, "SettingsLink").nth(5)
                else {
                    failures3
                        .borrow_mut()
                        .push("missing Player landing entry".into());
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                slint::spawn_local(async move {
                    player
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        check_segmented_rows(&app, "320px Player", 2, true, &failures4);
                        slint::quit_event_loop().unwrap();
                    });
                })
                .unwrap();
            });
        });
    });
}

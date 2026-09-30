//! The shared input's compact, touch and prominent layouts follow long text.

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

type Failures = Rc<RefCell<Vec<String>>>;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn long_text() -> slint::SharedString {
    "https://example.com/this/is/a/very/long/url/that/must/not/expand/the/input/field"
        .repeat(3)
        .into()
}

fn exercise_field(
    app: &nova::AppWindow,
    failures: Failures,
    settings: bool,
    done: impl FnOnce(nova::AppWindow, Failures) + 'static,
) {
    let field = ElementHandle::find_by_element_type_name(app, "SearchField")
        .next()
        .unwrap();
    let viewport = ElementHandle::find_by_element_id(app, "SearchField::input_viewport")
        .next()
        .unwrap();
    let input = ElementHandle::find_by_element_id(app, "SearchField::inner")
        .next()
        .unwrap();
    let clear = ElementHandle::find_by_element_id(app, "SearchField::clear_button")
        .next()
        .expect("nonempty field has a clear button");
    if field.absolute_position().x + field.size().width > app.window().size().width as f32 + 0.5 {
        failures
            .borrow_mut()
            .push("long text pushed its field outside the window".into());
    }
    let weak = app.as_weak();
    let _ = slint::spawn_local(async move {
        field
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        let app = weak.upgrade().unwrap();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::End.into(),
            });
        let weak = app.as_weak();
        after(50, move || {
            let app = weak.upgrade().unwrap();
            if input.absolute_position().x >= viewport.absolute_position().x - 1.0 {
                failures
                    .borrow_mut()
                    .push("End did not scroll long text to the caret".into());
            }
            let width = if app.window().size().width > 600 {
                900
            } else {
                320
            };
            app.window().set_size(slint::PhysicalSize::new(width, 800));
            let weak = app.as_weak();
            after(50, move || {
                let app = weak.upgrade().unwrap();
                if input.absolute_position().x + input.size().width
                    > viewport.absolute_position().x + viewport.size().width + 1.0
                {
                    failures
                        .borrow_mut()
                        .push("shrinking the window hid the end of the input".into());
                }
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: slint::platform::Key::Home.into(),
                    });
                let weak = app.as_weak();
                after(50, move || {
                    let app = weak.upgrade().unwrap();
                    if (input.absolute_position().x - viewport.absolute_position().x).abs() > 1.0 {
                        failures
                            .borrow_mut()
                            .push("Home did not scroll back to the beginning".into());
                    }
                    let weak = app.as_weak();
                    let _ = slint::spawn_local(async move {
                        clear
                            .single_click(slint::platform::PointerEventButton::Left)
                            .await;
                        let app = weak.upgrade().unwrap();
                        let text = if settings {
                            app.get_addon_url()
                        } else {
                            app.get_search_text()
                        };
                        if !text.is_empty() {
                            failures
                                .borrow_mut()
                                .push("clear button did not empty its input".into());
                        }
                        done(app, failures);
                    });
                });
            });
        });
    });
}

#[test]
fn all_input_sizes_follow_the_caret_and_clear_text() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.set_show_home(false);
    app.set_search_text(long_text());
    app.show().unwrap();
    let failures: Failures = Rc::new(RefCell::new(Vec::new()));
    let failures1 = failures.clone();
    let weak = app.as_weak();
    after(300, move || {
        let app = weak.upgrade().unwrap();
        // Prominent Discover input.
        exercise_field(&app, failures1, false, move |app, failures| {
            app.set_show_settings(true);
            app.set_addon_url(long_text());
            let weak = app.as_weak();
            after(300, move || {
                let app = weak.upgrade().unwrap();
                let addons = ElementHandle::find_by_element_type_name(&app, "SettingsLink")
                    .next()
                    .unwrap();
                let weak = app.as_weak();
                let _ = slint::spawn_local(async move {
                    addons
                        .single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = weak.upgrade().unwrap();
                        // Touch-sized Settings input.
                        exercise_field(&app, failures, true, move |app, failures| {
                            app.window().set_size(slint::PhysicalSize::new(1100, 800));
                            app.set_addon_url(long_text());
                            let weak = app.as_weak();
                            after(300, move || {
                                let app = weak.upgrade().unwrap();
                                // Compact Settings input.
                                exercise_field(&app, failures, true, move |_, _| {
                                    slint::quit_event_loop().unwrap();
                                });
                            });
                        });
                    });
                });
            });
        });
    });
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "{}",
        failures.borrow().join("\n")
    );
}

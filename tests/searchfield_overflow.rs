//! The shared input's compact, touch and prominent layouts follow long text.

#[path = "support/destinations.rs"]
mod destinations;
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
    let field = if settings {
        ElementHandle::find_by_element_id(app, "SettingsPage::addon_url_narrow")
            .chain(ElementHandle::find_by_element_id(
                app,
                "SettingsPage::addon_url_wide",
            ))
            .next()
            .unwrap()
    } else {
        ElementHandle::find_by_element_type_name(app, "SearchField")
            .next()
            .unwrap()
    };
    let child = |id: &str| field.query_descendants().match_id(id).find_first().unwrap();
    let viewport = child("SearchField::input_viewport");
    let input = child("SearchField::inner");
    let clear = child("SearchField::clear_button");
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
    // Caret following needs settled geometry. A two-pane transition can move
    // the clear button between the synthetic pointer press and release.
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
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
                let addons = ({
                    app.set_settings_search_query("Addons".into());
                    destinations::find(&app, "settings:addons").next()
                })
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
                            app.window().set_size(slint::PhysicalSize::new(1400, 800));
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

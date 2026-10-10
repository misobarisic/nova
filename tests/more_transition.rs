//! More destinations use page motion without delaying Android Back.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, platform::WindowEvent};
use std::time::Duration;

fn page(app: &nova::AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_element_type_name(app, name)
        .next()
        .unwrap()
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap()
}
fn tap(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    for event in [
        WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}

#[test]
fn more_subpages_slide_fade_and_accept_back_during_entry() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        for (label, component) in [("Settings", "SettingsPage"), ("About", "AboutPage")] {
            let app = nova::AppWindow::new().unwrap();
            app.set_is_android(true);
            app.set_touch_menus(true);
            app.window().set_size(slint::PhysicalSize::new(390, 800));
            app.global::<nova::Anim>().set_enabled(enabled);
            app.global::<nova::Anim>().set_transitions(transitions);
            app.on_settings_search_matches(|_, _| true);
            let weak = app.as_weak();
            app.on_settings_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_about(false);
                a.set_show_settings(true);
            });
            let weak = app.as_weak();
            app.on_about_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_settings(false);
                a.set_show_about(true);
            });
            app.show().unwrap();
            app.set_more_open(true);
            let _ = page(&app, "MorePage").size();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            tap(&app, button(&app, label));
            let start = page(&app, component);
            let x = start.absolute_position().x;
            let opacity = start.computed_opacity();
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
                0
            );
            if enabled && transitions {
                assert!(x > 0.0 && opacity < 0.01);
            } else {
                assert!(x.abs() < 0.1 && opacity > 0.99);
            }
            let mut slide = false;
            let mut fade = false;
            for _ in 0..35 {
                let p = page(&app, component);
                let current = p.absolute_position().x;
                let alpha = p.computed_opacity();
                slide |= current > 0.1 && current < x - 0.1;
                fade |= alpha > 0.01 && alpha < 0.99;
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            }
            let p = page(&app, component);
            assert!(p.absolute_position().x.abs() < 0.1 && p.computed_opacity() > 0.99);
            if enabled && transitions {
                assert!(slide && fade);
            }
            // Reopen and press navbar Back while the entrance is in progress.
            tap(&app, button(&app, "Back"));
            let _ = page(&app, "MorePage").size();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            tap(&app, button(&app, label));
            let _ = page(&app, component).size();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            for event in [
                WindowEvent::KeyPressed {
                    text: slint::platform::Key::Back.into(),
                },
                WindowEvent::KeyPressRepeated {
                    text: slint::platform::Key::Back.into(),
                },
                WindowEvent::KeyReleased {
                    text: slint::platform::Key::Back.into(),
                },
            ] {
                assert!(matches!(
                    app.window().dispatch_event_with_result(event).unwrap(),
                    slint::platform::WindowEventDispatchResult::Accepted
                ));
            }
            assert!(app.get_more_open());
            assert!(!app.get_show_settings() && !app.get_show_about());
        }
    }
}

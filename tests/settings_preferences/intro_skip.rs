//! Confidence presets retain independent touch, keyboard and sync controls.
#![cfg(not(feature = "live-preview"))]
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.absolute_position(), e.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}

fn preset(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "SettingsPlayerModule::confidence_preset")
        .next()
        .expect("confidence selector")
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

#[test]
fn confidence_presets_work_by_touch_and_keyboard_in_both_settings_layouts() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::confidence_presets_work_by_touch_and_keyboard_in_both_settings_layouts"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    assert_eq!(app.get_intro_skip_confidence_index(), 2);
    for android in [false, true] {
        app.set_is_android(android);
        for width in [360, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_player_backend_index(0);
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            for attempt in 0..60 {
                settle(&app);
                if ElementHandle::find_by_element_id(
                    &app,
                    "SettingsPlayerModule::confidence_preset",
                )
                .next()
                .is_some()
                {
                    break;
                }
                app.set_settings_scroll_y(-60.0 * (attempt + 1) as f32);
            }
            let y = preset(&app).absolute_position().y;
            app.set_settings_scroll_y((app.get_settings_scroll_y() - y + 240.0).min(0.0));
            settle(&app);
            for (index, label) in [
                "Any (0%)",
                "Low (25%)",
                "Balanced (50%)",
                "High (80%)",
                "Very high (95%)",
            ]
            .into_iter()
            .enumerate()
            {
                preset(&app).mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                ElementHandle::find_by_accessible_label(&app, label)
                    .last()
                    .expect(label)
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(app.get_intro_skip_confidence_index(), index as i32);
            }
            assert!(preset(&app).size().height >= 44.0);
            assert!(preset(&app).absolute_position().x >= 0.0);
            assert!(
                preset(&app).absolute_position().x + preset(&app).size().width
                    <= width as f32 + 1.0
            );
            key(&app, slint::platform::Key::LeftArrow);
            assert_eq!(app.get_intro_skip_confidence_index(), 3);
            key(&app, slint::platform::Key::Return);
            assert_eq!(app.get_intro_skip_confidence_index(), 4);
            app.set_intro_skip_mode_index(1);
            key(&app, slint::platform::Key::UpArrow);
            key(&app, slint::platform::Key::RightArrow);
            assert_eq!(app.get_intro_skip_mode_index(), 2);
            assert_eq!(app.get_intro_skip_confidence_index(), 4);
            key(&app, slint::platform::Key::UpArrow);
            key(&app, slint::platform::Key::RightArrow);
            assert!(app.get_auto_next_episode());
            app.set_player_backend_index(1);
            settle(&app);
            assert!(
                ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::confidence_preset")
                    .next()
                    .is_none()
            );
        }
    }
    assert!(
        edits
            .borrow()
            .iter()
            .any(|field| field == "intro_skip_confidence")
    );
}

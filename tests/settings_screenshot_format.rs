//! Screenshot preferences stay independent and usable in both Settings layouts.
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
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
    }
}
fn item(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}
fn choose(app: &nova::AppWindow, label: &str) {
    // Compiled section delegates outside the viewport are instantiated lazily.
    for attempt in 0..40 {
        if ElementHandle::find_by_element_id(app, "SettingsPlayerModule::screenshot_format")
            .next()
            .is_some()
        {
            break;
        }
        app.set_settings_scroll_y(-50.0 * (attempt + 1) as f32);
        settle(app);
    }
    let dropdown = item(app, "SettingsPlayerModule::screenshot_format");
    let y = dropdown.absolute_position().y;
    app.set_settings_scroll_y((app.get_settings_scroll_y() - (y - 240.0)).min(0.0));
    settle(app);
    item(app, "SettingsPlayerModule::screenshot_format")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
    ElementHandle::find_by_accessible_label(app, label)
        .last()
        .expect(label)
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}

#[test]
fn formats_folders_and_quality_work_by_touch_and_keyboard() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.set_cache_images(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let captured = edits.clone();
    app.on_settings_edited(move |field| captured.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for android in [false, true] {
        app.set_is_android(android);
        for width in [360, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            settle(&app);
            app.set_player_backend_index(0);
            app.set_screenshot_jpeg_quality(67.0);
            app.set_screenshot_webp_quality(92.0);
            for (index, label) in ["PNG (lossless)", "JPEG", "WebP (lossless)", "WebP (lossy)"]
                .into_iter()
                .enumerate()
            {
                choose(&app, label);
                assert_eq!(app.get_screenshot_format_index(), index as i32);
                let dropdown = item(&app, "SettingsPlayerModule::screenshot_format");
                assert!(dropdown.absolute_position().x >= 0.0);
                assert!(dropdown.absolute_position().x + dropdown.size().width <= width as f32);
                assert!(dropdown.size().height >= 44.0);
                if index == 1 || index == 3 {
                    key(&app, slint::platform::Key::DownArrow); // folder
                    key(&app, slint::platform::Key::RightArrow);
                    assert!(app.get_screenshot_show_folder());
                    key(&app, slint::platform::Key::DownArrow); // quality
                    key(&app, slint::platform::Key::RightArrow);
                    if index == 1 {
                        assert_eq!(app.get_screenshot_jpeg_quality(), 68.0);
                        assert_eq!(app.get_screenshot_webp_quality(), 92.0);
                    } else {
                        assert_eq!(app.get_screenshot_jpeg_quality(), 68.0);
                        assert_eq!(app.get_screenshot_webp_quality(), 93.0);
                    }
                }
                assert_eq!(
                    ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::jpeg_quality")
                        .count(),
                    usize::from(index == 1)
                );
                assert_eq!(
                    ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::webp_quality")
                        .count(),
                    usize::from(index == 3)
                );
            }
            choose(&app, "PNG (lossless)");
            key(&app, slint::platform::Key::RightArrow);
            assert_eq!(
                app.get_screenshot_format_index(),
                1,
                "keyboard chooses JPEG"
            );
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::LeftArrow);
            assert!(!app.get_screenshot_show_folder());
            app.set_player_backend_index(1);
            settle(&app);
            assert!(
                ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::screenshot_format")
                    .next()
                    .is_none()
            );
        }
    }
    for field in [
        "screenshot_format",
        "screenshot_show_folder",
        "screenshot_jpeg_quality",
        "screenshot_webp_quality",
    ] {
        assert!(
            edits.borrow().iter().any(|seen| seen == field),
            "save callback: {field}"
        );
    }
}

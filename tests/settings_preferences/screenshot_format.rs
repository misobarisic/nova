//! Screenshot preferences stay independent and usable in both Settings layouts.
#![cfg(not(feature = "live-preview"))]
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
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
fn choose_control(app: &nova::AppWindow, id: &str, label: &str) {
    // Compiled section delegates outside the viewport are instantiated lazily.
    for attempt in 0..40 {
        if ElementHandle::find_by_element_id(app, id).next().is_some() {
            break;
        }
        app.set_settings_scroll_y(-50.0 * (attempt + 1) as f32);
        settle(app);
    }
    let dropdown = item(app, id);
    let y = dropdown.absolute_position().y;
    app.set_settings_scroll_y((app.get_settings_scroll_y() - (y - 240.0)).min(0.0));
    settle(app);
    item(app, id).mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
    ElementHandle::find_by_accessible_label(app, label)
        // The closed dropdown also exposes its current label. With the popup
        // overlaid on top, clicking that handle selects the first popup row.
        .filter(|element| {
            element.accessible_role() == Some(AccessibleRole::Button)
                && element.type_name().as_deref() != Some("Dropdown")
        })
        .last()
        .expect(label)
        .invoke_accessible_default_action();
    settle(app);
}

fn choose(app: &nova::AppWindow, label: &str) {
    choose_control(app, "SettingsPlayerModule::screenshot_format", label);
}

#[test]
fn formats_folders_and_quality_work_by_touch_and_keyboard() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "formats_folders_and_quality_work_by_touch_and_keyboard"
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
        for width in [360, 1280, 1920] {
            app.global::<nova::InterfaceStyle>().set_tv(width == 1920);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            settle(&app);
            app.set_player_backend_index(0);
            app.set_screenshot_jpeg_quality(67.0);
            app.set_screenshot_webp_quality(92.0);
            app.set_screenshot_avif_quality(85.0);
            app.set_screenshot_jxl_quality(85.0);
            for (index, label) in [
                "PNG (lossless)",
                "JPEG",
                "WebP (lossless)",
                "WebP (lossy)",
                "AVIF",
                "JPEG XL (lossless)",
                "JPEG XL (lossy)",
            ]
            .into_iter()
            .enumerate()
            {
                choose(&app, label);
                assert_eq!(app.get_screenshot_format_index(), index as i32);
                let dropdown = item(&app, "SettingsPlayerModule::screenshot_format");
                assert!(dropdown.absolute_position().x >= 0.0);
                assert!(dropdown.absolute_position().x + dropdown.size().width <= width as f32);
                assert!(dropdown.size().height >= 44.0);
                if index == 1 || index == 3 || index == 4 || index == 6 {
                    key(&app, slint::platform::Key::DownArrow); // folder
                    key(&app, slint::platform::Key::RightArrow);
                    assert!(app.get_screenshot_show_folder());
                    key(&app, slint::platform::Key::DownArrow); // quality
                    key(&app, slint::platform::Key::RightArrow);
                    if index == 1 {
                        assert_eq!(app.get_screenshot_jpeg_quality(), 68.0);
                        assert_eq!(app.get_screenshot_webp_quality(), 92.0);
                    } else if index == 3 {
                        assert_eq!(app.get_screenshot_jpeg_quality(), 68.0);
                        assert_eq!(app.get_screenshot_webp_quality(), 93.0);
                        assert_eq!(app.get_screenshot_avif_quality(), 85.0);
                    } else if index == 4 {
                        let quality = item(&app, "SettingsPlayerModule::avif_quality");
                        assert!(quality.absolute_position().x >= 0.0);
                        assert!(
                            quality.absolute_position().x + quality.size().width <= width as f32
                        );
                        assert!(quality.size().height >= 44.0);
                        assert_eq!(app.get_screenshot_jpeg_quality(), 68.0);
                        assert_eq!(app.get_screenshot_webp_quality(), 93.0);
                        assert_eq!(app.get_screenshot_avif_quality(), 86.0);
                        app.set_screenshot_avif_quality(100.0);
                        key(&app, slint::platform::Key::RightArrow);
                        assert_eq!(app.get_screenshot_avif_quality(), 100.0);
                        app.set_screenshot_avif_quality(1.0);
                        key(&app, slint::platform::Key::LeftArrow);
                        assert_eq!(app.get_screenshot_avif_quality(), 1.0);
                    } else {
                        assert_eq!(app.get_screenshot_jxl_quality(), 86.0);
                        assert_eq!(app.get_screenshot_avif_quality(), 1.0);
                        let quality = item(&app, "SettingsPlayerModule::jxl_quality");
                        assert!(quality.absolute_position().x >= 0.0);
                        assert!(
                            quality.absolute_position().x + quality.size().width <= width as f32
                        );
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
                assert_eq!(
                    ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::avif_quality")
                        .count(),
                    usize::from(index == 4)
                );
            }
            // New formats append to established persisted indices.
            choose(&app, "JPEG XL (lossy)");
            assert_eq!(
                app.get_screenshot_format_index(),
                6,
                "reselect JPEG XL: android={android}, width={width}"
            );
            key(&app, slint::platform::Key::RightArrow);
            assert_eq!(
                app.get_screenshot_format_index(),
                6,
                "format maximum: android={android}, width={width}"
            );
            key(&app, slint::platform::Key::LeftArrow);
            assert_eq!(app.get_screenshot_format_index(), 5);
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
        "screenshot_avif_quality",
        "screenshot_jxl_quality",
    ] {
        assert!(
            edits.borrow().iter().any(|seen| seen == field),
            "save callback: {field}"
        );
    }
}

#[test]
fn cache_avif_selection_preserves_other_quality_and_permanent_bridge() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::cache_avif_selection_preserves_other_quality_and_permanent_bridge"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(2);
    app.set_settings_detail_open(true);
    app.set_settings_advanced(true);
    app.set_cache_images(true);
    app.set_cache_enabled(true);
    app.show().unwrap();

    for width in [360, 1280, 1920] {
        app.global::<nova::InterfaceStyle>().set_tv(width == 1920);
        app.window().set_size(slint::PhysicalSize::new(width, 1800));
        app.set_settings_scroll_y(0.0);
        app.set_cache_format_index(1);
        app.set_cache_quality(63.0);
        app.set_cache_avif_quality(85.0);
        settle(&app);
        choose_control(&app, "SettingsImageCacheModule::cache_format", "AVIF");
        assert_eq!(app.get_cache_format_index(), 2);
        let quality = item(&app, "SettingsImageCacheModule::cache_avif_quality");
        assert!(quality.absolute_position().x >= 0.0);
        assert!(quality.absolute_position().x + quality.size().width <= width as f32);
        quality
            .query_descendants()
            .match_predicate(|element| {
                element.accessible_label().as_deref() == Some("Increase value")
            })
            .find_first()
            .expect("AVIF quality increase")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(app.get_cache_avif_quality(), 86.0);
        assert_eq!(app.get_cache_quality(), 63.0);
        let bridge = app.global::<nova_ui_shared::SettingsBridge>();
        let content = (*app).global::<nova_ui_shared::SettingsContent>();
        bridge.set_kb_zone(5);
        app.set_kb_active(true);
        bridge.invoke_focus_shell();
        content.set_kb_hr(3);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_cache_avif_quality(), 87.0);
        assert_eq!(app.get_cache_quality(), 63.0);
        app.set_cache_avif_quality(100.0);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_cache_avif_quality(), 100.0);
        app.set_cache_avif_quality(1.0);
        key(&app, slint::platform::Key::LeftArrow);
        assert_eq!(app.get_cache_avif_quality(), 1.0);
        content.set_kb_hr(2);
        key(&app, slint::platform::Key::LeftArrow);
        assert_eq!(app.get_cache_format_index(), 1);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_cache_quality(), 64.0);
        assert_eq!(app.get_cache_avif_quality(), 1.0);
        app.set_cache_jxl_quality(85.0);
        app.set_cache_jxl_lossless_jpeg(false);
        choose_control(
            &app,
            "SettingsImageCacheModule::cache_format",
            "JPEG XL (lossless)",
        );
        assert_eq!(app.get_cache_format_index(), 3);
        assert!(
            ElementHandle::find_by_element_id(&app, "SettingsImageCacheModule::cache_jxl_quality")
                .next()
                .is_none()
        );
        bridge.set_kb_zone(5);
        app.set_kb_active(true);
        bridge.invoke_focus_shell();
        content.set_kb_hr(2);
        key(&app, slint::platform::Key::DownArrow);
        assert_eq!(content.get_kb_hr(), 12, "JXL exposes the transcode toggle");
        key(&app, slint::platform::Key::RightArrow);
        assert!(app.get_cache_jxl_lossless_jpeg());
        key(&app, slint::platform::Key::DownArrow);
        assert_eq!(content.get_kb_hr(), 4, "lossless skips quality");
        choose_control(
            &app,
            "SettingsImageCacheModule::cache_format",
            "JPEG XL (lossy)",
        );
        assert_eq!(app.get_cache_format_index(), 4);
        bridge.set_kb_zone(5);
        app.set_kb_active(true);
        bridge.invoke_focus_shell();
        content.set_kb_hr(3);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_cache_jxl_quality(), 86.0);
        assert_eq!(app.get_cache_avif_quality(), 1.0);
        assert_eq!(app.get_cache_quality(), 64.0);
    }

    // Backends keep their property bindings when the Settings page is unmounted.
    app.set_show_settings(false);
    settle(&app);
    app.set_cache_avif_quality(79.0);
    app.set_screenshot_avif_quality(81.0);
    app.set_cache_jxl_quality(77.0);
    app.set_screenshot_jxl_quality(83.0);
    let bridge = app.global::<nova_ui_shared::SettingsBridge>();
    assert_eq!(bridge.get_cache_avif_quality(), 79.0);
    assert_eq!(bridge.get_screenshot_avif_quality(), 81.0);
    assert_eq!(bridge.get_cache_jxl_quality(), 77.0);
    assert_eq!(bridge.get_screenshot_jxl_quality(), 83.0);
    bridge.set_cache_avif_quality(78.0);
    bridge.set_screenshot_avif_quality(82.0);
    bridge.set_cache_jxl_quality(75.0);
    bridge.set_screenshot_jxl_quality(84.0);
    assert_eq!(app.get_cache_avif_quality(), 78.0);
    assert_eq!(app.get_screenshot_avif_quality(), 82.0);
    assert_eq!(app.get_cache_jxl_quality(), 75.0);
    assert_eq!(app.get_screenshot_jxl_quality(), 84.0);
}

#[test]
fn codec_quality_search_reveals_correct_control_in_both_languages() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::codec_quality_search_reveals_correct_control_in_both_languages"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_advanced(false);
    app.set_player_backend_index(0);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    for (language, query) in [("en", "AVIF quality"), ("hr", "AVIF kvaliteta")] {
        slint::select_bundled_translation(language).unwrap();
        let jpeg_query = if language == "en" {
            "JPEG quality"
        } else {
            "JPEG kvaliteta"
        };
        let quality_query = if language == "en" {
            "Quality"
        } else {
            "Kvaliteta"
        };
        let jxl_query = if language == "en" {
            "JPEG XL quality"
        } else {
            "Kvaliteta JPEG XL"
        };
        for (section, destination, screenshot_format, cache_format, query, expected) in [
            (5, "settings:player", 6, 0, "autoplay", "auto_next_episode"),
            (5, "settings:player", 0, 0, jxl_query, "screenshot_format"),
            (
                5,
                "settings:player",
                6,
                0,
                jxl_query,
                "screenshot_jxl_quality",
            ),
            (2, "settings:image-cache", 6, 3, jxl_query, "format"),
            (2, "settings:image-cache", 6, 4, jxl_query, "jxl_quality"),
            (5, "settings:player", 0, 0, query, "screenshot_format"),
            (5, "settings:player", 4, 0, query, "screenshot_avif_quality"),
            (2, "settings:image-cache", 4, 0, query, "format"),
            (2, "settings:image-cache", 4, 2, query, "avif_quality"),
            (2, "settings:image-cache", 4, 2, jpeg_query, "format"),
            (2, "settings:image-cache", 4, 0, quality_query, "quality"),
            (
                2,
                "settings:image-cache",
                4,
                2,
                quality_query,
                "avif_quality",
            ),
        ] {
            app.set_settings_detail_open(false);
            app.set_screenshot_format_index(screenshot_format);
            app.set_cache_format_index(cache_format);
            app.set_settings_search_query(query.into());
            settle(&app);
            crate::destinations::find(&app, destination)
                .next()
                .expect(destination)
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert_eq!(app.get_settings_selected_id(), section);
            let content = (*app).global::<nova_ui_shared::SettingsContent>();
            assert_eq!(content.get_search_target_key(), expected);
            if expected == "auto_next_episode" {
                assert_eq!(
                    content.get_kb_pr(),
                    content.invoke_kb_player_speed_row() + 14
                );
            }
            assert!(
                crate::destinations::find(&app, &format!("setting:{expected}"))
                    .next()
                    .is_some(),
                "{language}: the selected codec's real setting is mounted"
            );
            assert_eq!(app.get_screenshot_format_index(), screenshot_format);
            assert_eq!(app.get_cache_format_index(), cache_format);
        }
    }
}

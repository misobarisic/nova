//! Shared subtitle controls keep Settings defaults and title edits independent.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_config::playback_preferences::SubtitleStyle;
use slint::{ComponentHandle, Model};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.absolute_position(), element.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(25));
        slint::platform::update_timers_and_animations();
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn button(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
}
fn click(app: &nova::AppWindow, label: &str) {
    button(app, label)
        .unwrap_or_else(|| panic!("missing {label}"))
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}

#[test]
fn subtitle_defaults_and_title_controls_fit_and_route_independently() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::subtitle_defaults_and_title_controls_fit_and_route_independently"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let global_edits = Rc::new(RefCell::new(Vec::new()));
    let title_edits = Rc::new(RefCell::new(Vec::new()));
    let resets = Rc::new(RefCell::new(Vec::new()));
    let edits = global_edits.clone();
    let weak = app.as_weak();
    app.on_default_subtitle_style_changed(move |i, v| {
        edits.borrow_mut().push((i, v));
        let app = weak.upgrade().unwrap();
        let current = app.get_subtitle_style();
        let mut style = SubtitleStyle::default();
        for index in 0..7 {
            style = style.with_value(index, current.row_data(index as usize).unwrap());
        }
        app.set_subtitle_style(slint::ModelRc::new(slint::VecModel::from(
            style.with_value(i, v).values().to_vec(),
        )));
    });
    let edits = title_edits.clone();
    app.on_subtitle_style_changed(move |i, v| edits.borrow_mut().push((i, v)));
    let captured = resets.clone();
    app.on_restore_playback_default(move |kind| captured.borrow_mut().push(kind));
    app.show().unwrap();

    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        let (increase, timing, appearance) = if language == "hr" {
            (
                "Povećaj",
                "Vremenski pomak titlova (sekunde)",
                "Izgled i vremenski pomak titlova",
            )
        } else {
            (
                "Increase",
                "Subtitle timing (seconds)",
                "Subtitle appearance and timing",
            )
        };
        for (width, height, android) in [
            (320, 740, true),
            (390, 844, true),
            (360, 800, true),
            (900, 360, true),
            (1280, 800, false),
        ] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            app.set_is_android(android);
            app.set_show_home(false);
            app.set_show_settings(true);
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            app.set_player_backend_index(0);
            app.set_settings_scroll_y(0.0);
            settle(&app);
            let target = format!("{increase} {timing}");
            for attempt in 0..100 {
                if button(&app, &target).is_some() {
                    break;
                }
                app.set_settings_scroll_y(-50.0 * (attempt + 1) as f32);
                settle(&app);
            }
            let control = button(&app, &target).unwrap_or_else(|| {
                panic!("Settings subtitle timing step missing: {language} {width}×{height}")
            });
            // Inspection includes partly clipped descendants. Center the full
            // touch target between the sticky header and viewport bottom.
            let center = control.absolute_position().y + control.size().height / 2.0;
            app.set_settings_scroll_y(
                (app.get_settings_scroll_y() + height as f32 / 2.0 - center).min(0.0),
            );
            settle(&app);
            let control =
                button(&app, &target).expect("Settings timing step remains visible after reveal");
            let position = control.absolute_position();
            let size = control.size();
            let context = format!(
                "{language} {width}×{height}: position={position:?}, size={size:?}, scroll={}",
                app.get_settings_scroll_y()
            );
            assert!(size.width >= 44.0, "{context}");
            assert!(
                position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5,
                "{context}"
            );
            assert!(
                position.y >= 80.0 && position.y + size.height <= height as f32 - 20.0,
                "{context}"
            );
            let previous = global_edits.borrow().len();
            click(&app, &target);
            assert_eq!(global_edits.borrow().len(), previous + 1, "{context}");
            assert!(title_edits.borrow().is_empty());
            let settings = app.global::<nova_ui_shared::SettingsBridge>();
            let content = (*app).global::<nova_ui_shared::SettingsContent>();
            let style_row = content.invoke_kb_player_speed_row()
                + if matches!(app.get_screenshot_format_index(), 1 | 3) {
                    5
                } else {
                    4
                };
            settings.set_kb_zone(9);
            app.set_kb_active(true);
            settings.invoke_focus_shell();
            content.set_kb_pr(style_row + 5);
            settle(&app);
            key(&app, slint::platform::Key::Return);
            assert_eq!(global_edits.borrow().last().unwrap().0, 5);
            content.set_kb_pr(style_row + 6);
            settle(&app);
            key(&app, slint::platform::Key::Return);
            assert_eq!(global_edits.borrow().last().unwrap().0, 6);
            for (caption_id, button_id) in [
                ("color_caption", "color_button"),
                ("embedded_caption", "embedded_button"),
            ] {
                let caption = ElementHandle::find_by_element_id(
                    &app,
                    &format!("SubtitleControls::{caption_id}"),
                )
                .next()
                .expect("subtitle caption");
                let button = ElementHandle::find_by_element_id(
                    &app,
                    &format!("SubtitleControls::{button_id}"),
                )
                .next()
                .expect("subtitle control");
                assert!(
                    (44.0..=76.0).contains(&button.size().height),
                    "subtitle control keeps its intrinsic height: {language} {width}px {button_id}: {:?}",
                    button.size()
                );
                let gap = button.absolute_position().y
                    - caption.absolute_position().y
                    - caption.size().height;
                assert!(
                    (gap - 4.0).abs() < 0.5,
                    "subtitle caption/control gap: {language} {width}px {button_id}: {gap}"
                );
            }

            app.set_show_settings(false);
            app.set_effective_subtitle_style(slint::ModelRc::new(slint::VecModel::from(vec![
                0.0, 38.0, 19.0, 100.0, 1.65, 0.0, 0.0,
            ])));
            app.set_player_open(true);
            app.set_is_paused(true);
            app.set_playback_started(true);
            app.set_osd_visible(true);
            settle(&app);
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
                .next()
                .unwrap()
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            // Subtitles owns the two tool shortcuts on every layout.
            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::Return);
            assert!(
                ElementHandle::find_by_accessible_label(&app, appearance)
                    .next()
                    .is_some()
            );
            key(&app, slint::platform::Key::DownArrow); // timing
            let previous_global = global_edits.borrow().len();
            let previous = title_edits.borrow().len();
            key(&app, slint::platform::Key::RightArrow);
            assert_eq!(title_edits.borrow().len(), previous + 1);
            assert_eq!(title_edits.borrow().last().unwrap().0, 0);
            assert_eq!(global_edits.borrow().len(), previous_global);
            let control =
                button(&app, &target).expect("OSD timing control is revealed by keyboard focus");
            assert!(control.absolute_position().x >= 0.0);
            assert!(control.absolute_position().x + control.size().width <= width as f32 + 0.5);
            // Native outline widths may be fractional (mpv 0.41 uses 1.65).
            for _ in 0..4 {
                key(&app, slint::platform::Key::DownArrow);
            }
            // Slint formats numeric strings with the bundled locale's decimal
            // separator; Croatian correctly displays a comma.
            let outline_label = if language == "hr" {
                "1,65 px"
            } else {
                "1.65 px"
            };
            assert!(
                ElementHandle::find_by_accessible_label(&app, outline_label)
                    .next()
                    .is_some(),
                "missing fractional outline {outline_label}: {language} {width}×{height}; visible labels: {:?}",
                ElementHandle::find_by_element_type_name(&app, "Text")
                    .filter_map(|item| item.accessible_label())
                    .collect::<Vec<_>>()
            );
            key(&app, slint::platform::Key::RightArrow);
            let outline_edit = *title_edits.borrow().last().unwrap();
            assert_eq!(outline_edit.0, 4);
            assert!((outline_edit.1 - 1.70).abs() < 0.001);
            // Enter activates the full-width color and embedded-style buttons.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(title_edits.borrow().last(), Some(&(5, 1.0)));
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(title_edits.borrow().last(), Some(&(6, 1.0)));
            for _ in 0..7 {
                key(&app, slint::platform::Key::UpArrow);
            }
            key(&app, slint::platform::Key::Return);
            assert_eq!(resets.borrow().last(), Some(&3));
            key(&app, slint::platform::Key::Escape);
            key(&app, slint::platform::Key::Escape);
            key(&app, slint::platform::Key::Escape);
            app.set_player_open(false);
            title_edits.borrow_mut().clear();
        }
    }
    slint::select_bundled_translation("en").unwrap();
    app.hide().unwrap();
}

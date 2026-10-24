//! Listening edits share permanent device state across Settings and the OSD.
use crate::preference_support::{key, reveal, settle, toggle};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, platform::Key};
use std::{cell::RefCell, rc::Rc};

#[test]
fn listening_controls_route_pointer_and_keyboard_edits_in_settings_and_playing_osd() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::listening_controls_route_pointer_and_keyboard_edits_in_settings_and_playing_osd"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.show().unwrap();

    for (width, height, android) in [(390, 844, true), (1280, 800, false), (900, 360, true)] {
        slint::select_bundled_translation("en").unwrap();
        app.set_is_android(android);
        app.set_desktop_decoder_available(!android);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_player_open(false);
        app.set_show_settings(true);
        app.set_settings_selected_id(5);
        app.set_settings_detail_open(true);
        app.set_dialogue_boost(false);
        app.set_loudness_normalization(false);
        app.set_night_mode(false);
        let settings = app.global::<nova::SettingsBridge>();
        for (index, label, field) in [
            (0, "Dialogue boost", "dialogue_boost"),
            (1, "Loudness normalization", "loudness_normalization"),
            (2, "Night mode", "night_mode"),
        ] {
            let control = reveal(&app, || toggle(&app, label), label);
            control.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert!(settings.invoke_listening_enabled(index));
            assert_eq!(control.accessible_checked(), Some(true));
            assert_eq!(edits.borrow().last().map(String::as_str), Some(field));
            key(&app, Key::LeftArrow);
            assert!(!settings.invoke_listening_enabled(index));
            assert_eq!(control.accessible_checked(), Some(false));
            key(&app, Key::RightArrow);
            assert!(settings.invoke_listening_enabled(index));
            assert_eq!(control.accessible_checked(), Some(true));
        }

        app.set_show_settings(false);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_duration(100.0);
        app.set_position(42.0);
        app.set_osd_visible(true);
        settle(&app);
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
            .next()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        // Last root row, with decoder available and no chapter rows.
        for _ in 0..8 {
            key(&app, Key::DownArrow);
        }
        key(&app, Key::Return);
        for (index, field) in [
            (0, "dialogue_boost"),
            (1, "loudness_normalization"),
            (2, "night_mode"),
        ] {
            key(&app, Key::Return);
            assert!(!settings.invoke_listening_enabled(index));
            assert_eq!(edits.borrow().last().map(String::as_str), Some(field));
            if index < 2 {
                key(&app, Key::DownArrow);
            }
        }
        assert_eq!(app.get_position(), 42.0);
        assert_eq!(app.get_duration(), 100.0);
        // Back restores the invoking row and Return reopens the same submenu.
        key(&app, Key::Escape);
        key(&app, Key::Return);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Dialogue boost")
                .any(|item| item.accessible_role() == Some(AccessibleRole::Button))
        );
        key(&app, Key::Escape);
        key(&app, Key::Escape);
    }
}

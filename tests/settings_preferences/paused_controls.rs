//! Paused OSD preference uses the normal accessible Settings toggle and row routing.
#![cfg(not(feature = "live-preview"))]
use crate::preference_support::{key, reveal, settle, toggle};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

#[test]
fn paused_controls_toggle_translates_and_keeps_neighboring_keyboard_rows() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::paused_controls_toggle_translates_and_keeps_neighboring_keyboard_rows"
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
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for (locale, label) in [
        ("en", "Keep controls visible while paused"),
        ("hr", "Zadrži kontrole vidljivima tijekom pauze"),
    ] {
        slint::select_bundled_translation(locale).unwrap();
        for android in [false, true] {
            app.set_is_android(android);
            for width in [360, 1280] {
                app.window().set_size(slint::PhysicalSize::new(width, 900));
                app.set_player_backend_index(0);
                app.set_settings_scroll_y(0.0);
                app.set_keep_controls_visible_while_paused(false);
                let item = reveal(&app, || toggle(&app, label), label);
                assert!(item.size().height >= 44.0);
                assert!(item.absolute_position().x >= 0.0);
                assert!(item.absolute_position().x + item.size().width <= width as f32 + 1.0);
                item.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert!(app.get_keep_controls_visible_while_paused());
                key(&app, slint::platform::Key::LeftArrow);
                assert!(!app.get_keep_controls_visible_while_paused());
                key(&app, slint::platform::Key::Return);
                assert!(app.get_keep_controls_visible_while_paused());
                key(&app, slint::platform::Key::UpArrow);
                key(&app, slint::platform::Key::LeftArrow);
                assert_eq!(
                    app.get_intro_skip_confidence_index(),
                    1,
                    "previous row remains confidence"
                );
                app.set_intro_skip_confidence_index(2);
                key(&app, slint::platform::Key::UpArrow);
                app.set_intro_skip_mode_index(2);
                key(&app, slint::platform::Key::LeftArrow);
                assert_eq!(app.get_intro_skip_mode_index(), 1);
                key(&app, slint::platform::Key::Return);
                assert_eq!(app.get_intro_skip_mode_index(), 2);
                key(&app, slint::platform::Key::UpArrow);
                app.set_auto_next_episode(true);
                key(&app, slint::platform::Key::LeftArrow);
                assert!(!app.get_auto_next_episode());
                key(&app, slint::platform::Key::Return);
                assert!(app.get_auto_next_episode());
                key(&app, slint::platform::Key::UpArrow);
                app.set_show_chapters(true);
                key(&app, slint::platform::Key::LeftArrow);
                assert!(!app.get_show_chapters());
                key(&app, slint::platform::Key::Return);
                assert!(app.get_show_chapters());
                // Return to confidence, then pass the paused-controls row
                // through the loading-text preference into stream rules.
                for _ in 0..3 {
                    key(&app, slint::platform::Key::DownArrow);
                }
                key(&app, slint::platform::Key::DownArrow);
                key(&app, slint::platform::Key::DownArrow);
                key(&app, slint::platform::Key::DownArrow);
                key(&app, slint::platform::Key::LeftArrow);
                assert!(
                    !app.global::<nova::StreamFiltersBridge>().get_enabled(),
                    "next row remains stream rules"
                );
                app.set_player_backend_index(1);
                settle(&app);
                assert!(
                    toggle(&app, label).is_none(),
                    "external player does not expose OSD preferences"
                );
            }
        }
    }
    assert!(
        edits
            .borrow()
            .iter()
            .any(|field| field == "keep_controls_visible_while_paused")
    );
}

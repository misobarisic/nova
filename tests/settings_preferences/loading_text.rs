//! The loading-text preference is global, accessible and saved through Settings.
use crate::preference_support::{key, reveal, settle, toggle};
use slint::{ComponentHandle, platform::Key};
use std::{cell::RefCell, rc::Rc};

#[test]
fn loading_text_toggle_translates_saves_and_routes_keyboard_to_the_correct_row() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::loading_text_toggle_translates_saves_and_routes_keyboard_to_the_correct_row"
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
    assert!(app.get_show_stream_loading_text());
    for (locale, label) in [
        ("en", "Show loading status text"),
        ("hr", "Prikaži tekst statusa učitavanja"),
    ] {
        slint::select_bundled_translation(locale).unwrap();
        for (width, android) in [(390, true), (1280, false)] {
            app.set_is_android(android);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_settings_scroll_y(0.0);
            let item = reveal(&app, || toggle(&app, label), label);
            app.set_show_stream_loading_text(true);
            item.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert!(!app.get_show_stream_loading_text());
            assert_eq!(
                edits.borrow().last().map(String::as_str),
                Some("show_stream_loading_text")
            );
            key(&app, Key::RightArrow);
            assert!(
                app.get_show_stream_loading_text(),
                "locale={locale} android={android} zone={} row={} expected={} backend={}",
                (*app)
                    .global::<nova_ui_shared::SettingsBridge>()
                    .get_kb_zone(),
                (*app)
                    .global::<nova_ui_shared::SettingsContent>()
                    .get_kb_pr(),
                (*app)
                    .global::<nova_ui_shared::SettingsContent>()
                    .invoke_kb_player_loading_text_row(),
                app.get_player_backend_index()
            );
            assert!(
                app.global::<nova::PlayerOsdBridge>()
                    .get_show_stream_loading_text()
            );
        }
    }
}

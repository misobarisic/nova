//! RAM-only P2P buffering remains accessible and uses the normal autosave route.
#![cfg(not(feature = "live-preview"))]
use crate::preference_support::{key, reveal, settle, toggle};
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};

fn slider(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Slider))
}

#[test]
fn torrent_ram_budget_translates_routes_and_respects_no_cache_and_platform_limits() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::torrent_ram_budget_translates_routes_and_respects_no_cache_and_platform_limits"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(4);
    app.set_settings_detail_open(true);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let saves = Rc::new(Cell::new(0));
    let recorded = saves.clone();
    app.on_save_settings(move || recorded.set(recorded.get() + 1));
    let clears = Rc::new(Cell::new(0));
    let recorded = clears.clone();
    app.on_clear_torrent_cache(move || recorded.set(recorded.get() + 1));
    app.show().unwrap();
    for (locale, label, no_cache) in [
        ("en", "Torrent RAM buffer", "No caching"),
        ("hr", "RAM međuspremnik za torrente", "Bez predmemoriranja"),
    ] {
        slint::select_bundled_translation(locale).unwrap();
        for android in [false, true] {
            app.set_is_android(android);
            for width in [360, 1280] {
                app.window().set_size(slint::PhysicalSize::new(width, 900));
                app.set_torrent_enabled(true);
                app.set_torrent_no_cache(false);
                app.set_torrent_ram_buffer_mb(64.0);
                app.set_settings_scroll_y(0.0);
                let item = reveal(&app, || slider(&app, label), label);
                assert_eq!(item.accessible_enabled(), Some(false));
                let toggle = reveal(&app, || toggle(&app, no_cache), no_cache);
                toggle.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert!(app.get_torrent_no_cache());
                let item = reveal(&app, || slider(&app, label), label);
                assert_eq!(item.accessible_enabled(), Some(true));
                assert!(item.size().height >= 44.0);
                assert!(item.absolute_position().x >= 0.0);
                assert!(item.absolute_position().x + item.size().width <= width as f32 + 1.0);
                item.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                let content = (*app).global::<nova_ui_shared::SettingsContent>();
                assert_eq!(
                    content.get_kb_tr(),
                    5,
                    "pointer selects the RAM keyboard row"
                );
                app.set_torrent_ram_buffer_mb(64.0);
                key(&app, slint::platform::Key::RightArrow);
                assert_eq!(app.get_torrent_ram_buffer_mb(), 96.0);
                app.set_torrent_ram_buffer_mb(if android { 512.0 } else { 2048.0 });
                key(&app, slint::platform::Key::RightArrow);
                assert_eq!(
                    app.get_torrent_ram_buffer_mb(),
                    if android { 512.0 } else { 2048.0 }
                );
                app.set_torrent_ram_buffer_mb(32.0);
                key(&app, slint::platform::Key::LeftArrow);
                assert_eq!(app.get_torrent_ram_buffer_mb(), 32.0);
                app.set_torrent_no_cache(false);
                key(&app, slint::platform::Key::RightArrow);
                assert_eq!(
                    app.get_torrent_ram_buffer_mb(),
                    32.0,
                    "disabled RAM row cannot edit"
                );
                key(&app, slint::platform::Key::UpArrow);
                key(&app, slint::platform::Key::Return);
                assert!(
                    app.get_torrent_no_cache(),
                    "previous row remains No caching"
                );
                key(&app, slint::platform::Key::DownArrow);
                key(&app, slint::platform::Key::Return);
                assert_eq!(clears.get(), 0, "Enter on RAM must never clear disk data");
                key(&app, slint::platform::Key::DownArrow);
                key(&app, slint::platform::Key::Return);
                assert!(clears.get() > 0, "next row remains explicit cache cleanup");
                clears.set(0);
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    app.set_settings_detail_open(false);
    app.set_settings_search_query("torrent RAM buffer".into());
    settle(&app);
    ElementQuery::from_root(&app)
        .match_predicate(|item| item.accessible_id().as_deref() == Some("settings:p2p"))
        .find_first()
        .expect("RAM search finds P2P")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    reveal(
        &app,
        || slider(&app, "Torrent RAM buffer"),
        "RAM search destination",
    );
    assert_eq!(
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_kb_tr(),
        5
    );
    assert!(
        saves.get() > 0,
        "RAM edits follow immediate settings capture"
    );
    app.set_torrent_enabled(false);
    settle(&app);
    assert!(slider(&app, "Torrent RAM buffer").is_none());
    assert_eq!(
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_kb_tr(),
        0
    );
}

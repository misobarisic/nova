//! A search hit names and reveals its real control across compiled leaf mounts.
#![cfg(not(feature = "live-preview"))]

use crate::destinations;
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..10 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.absolute_position(), item.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn result(app: &nova::AppWindow, id: &str) -> ElementHandle {
    destinations::find(app, id).next().expect(id)
}

fn open(app: &nova::AppWindow, id: &str, section: i32, keyboard: bool) {
    if keyboard {
        let bridge = app.global::<nova_ui_shared::SettingsBridge>();
        bridge.set_kb_zone(8);
        app.set_settings_focused_id(section);
        bridge.invoke_focus_shell();
        key(app, slint::platform::Key::Return);
    } else {
        result(app, id).mock_single_click(slint::platform::PointerEventButton::Left);
        settle(app);
    }
    assert_eq!(app.get_settings_selected_id(), section);
}

fn revealed(app: &nova::AppWindow, setting: &str) {
    let content = (**app).global::<nova_ui_shared::SettingsContent>();
    assert_eq!(content.get_search_target_key(), setting);
    assert!(
        !content.get_search_target_pending(),
        "mounted row consumed reveal"
    );
    let row = result(app, &format!("setting:{setting}"));
    let highlight = result(app, &format!("setting-highlight:{setting}"));
    let viewport = ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll")
        .next()
        .unwrap();
    let p = row.absolute_position();
    let s = row.size();
    let v = viewport.absolute_position();
    let bounds = viewport.size();
    let top = (content.get_detail_sticky_top()
        + content.get_detail_header_height()
        + content.get_detail_pinned_controls_height())
    .max(v.y);
    assert!(
        p.y >= top - 1.0 && p.y + s.height <= v.y + bounds.height + 1.0,
        "{setting} row {p:?}/{s:?} must clear sticky header {top} and viewport {v:?}/{bounds:?}"
    );
    assert!(p.x >= -0.5 && p.x + s.width <= app.window().size().width as f32 + 0.5);
    assert_eq!(
        highlight.size(),
        row.size(),
        "highlight covers the control row"
    );
}

#[test]
fn exact_hits_reveal_highlight_and_focus_controls_across_layouts() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "exact_hits_reveal_highlight_and_focus_controls_across_layouts"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    let writes = Rc::new(Cell::new(0));
    let count = writes.clone();
    app.on_settings_edited(move |_| count.set(count.get() + 1));
    let count = writes.clone();
    app.on_save_settings(move || count.set(count.get() + 1));
    app.show().unwrap();

    for (width, height, android, tv, language, keyboard) in [
        (360, 800, false, false, "en", false),
        (800, 360, true, false, "en", true),
        (1280, 800, false, false, "hr", false),
        (960, 540, true, true, "hr", true),
    ] {
        slint::select_bundled_translation(language).unwrap();
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.set_is_android(android);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_settings_detail_open(false);
        app.set_player_backend_index(0);
        app.set_screenshot_format_index(0);
        app.set_screenshot_show_folder(false);
        app.set_settings_search_query("Screenshot folder".into());
        settle(&app);
        assert_eq!(
            result(&app, "settings:player")
                .accessible_description()
                .as_deref(),
            Some(if language == "hr" {
                "Mapa snimki zaslona"
            } else {
                "Screenshot folder"
            })
        );
        writes.set(0);
        open(&app, "settings:player", 5, keyboard);
        revealed(&app, "screenshot_show_folder");
        assert_eq!(writes.get(), 0, "opening search never edits a setting");
        key(&app, slint::platform::Key::RightArrow);
        assert!(
            app.get_screenshot_show_folder(),
            "remote focus is on the highlighted folder control"
        );
        assert_eq!(app.get_screenshot_format_index(), 0);
        key(&app, slint::platform::Key::Back);
        assert_eq!(
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_search_target_key(),
            ""
        );

        // The retained query can open the same target again after its previous
        // one-shot request was consumed; saved section scroll cannot win.
        app.set_settings_scroll_y(0.0);
        open(&app, "settings:player", 5, keyboard);
        revealed(&app, "screenshot_show_folder");
        key(&app, slint::platform::Key::Back);
    }

    // A translated query and a non-Player leaf use the same anchor contract.
    app.global::<nova::InterfaceStyle>().set_tv(false);
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    app.set_settings_search_query("Prikažite sličice nepogledanih epizoda".into());
    settle(&app);
    assert_eq!(
        result(&app, "settings:display")
            .accessible_description()
            .as_deref(),
        Some("Prikažite sličice nepogledanih epizoda")
    );
    open(&app, "settings:display", 3, true);
    revealed(&app, "show_unwatched_thumbs");
    key(&app, slint::platform::Key::Back);
    slint::select_bundled_translation("en").unwrap();
    app.set_settings_search_query("Default playback speed".into());
    settle(&app);
    open(&app, "settings:player", 5, true);
    revealed(&app, "playback_speed");
    // Replacing the stacked speed anchor during rotation re-arms its reveal.
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    settle(&app);
    revealed(&app, "playback_speed");
}

#[test]
fn hidden_controls_and_abandoned_search_requests_do_not_reveal_later() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "hidden_controls_and_abandoned_search_requests_do_not_reveal_later"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(430, 800));
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_screenshot_format_index(0);
    app.show().unwrap();
    app.set_settings_search_query("JPEG quality".into());
    settle(&app);
    assert_eq!(
        result(&app, "settings:player")
            .accessible_description()
            .as_deref(),
        Some("JPEG quality › Screenshot format")
    );
    open(&app, "settings:player", 5, false);
    revealed(&app, "screenshot_format");
    assert_eq!(app.get_screenshot_format_index(), 0, "search preserves PNG");
    key(&app, slint::platform::Key::Back);
    app.set_screenshot_format_index(1);
    settle(&app);
    open(&app, "settings:player", 5, false);
    revealed(&app, "screenshot_jpeg_quality");
    key(&app, slint::platform::Key::Back);

    // Open without advancing the row's reveal timer, then replace the query
    // and destination. The outgoing leaf cannot consume the new request.
    app.set_settings_search_query("Screenshot folder".into());
    settle(&app);
    result(&app, "settings:player").mock_single_click(slint::platform::PointerEventButton::Left);
    app.set_settings_search_query("Relative episode dates".into());
    app.set_settings_detail_open(false);
    settle(&app);
    open(&app, "settings:display", 3, true);
    revealed(&app, "date_relative");
    key(&app, slint::platform::Key::Back);

    // A known section keyword with no supported visible control stays honest.
    app.set_player_backend_index(1);
    app.set_settings_search_query("Screenshot folder".into());
    settle(&app);
    assert_eq!(
        result(&app, "settings:player")
            .accessible_description()
            .as_deref(),
        Some("Built-in or external player")
    );
    open(&app, "settings:player", 5, false);
    assert_eq!(
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_search_target_key(),
        ""
    );
    assert!(
        !(*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_search_target_pending()
    );
    app.set_player_backend_index(0);
    settle(&app);
    assert_eq!(
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_search_target_key(),
        ""
    );
}

//! Settings view selection and preservation of advanced preferences.
#![cfg(not(feature = "live-preview"))]
use crate::preference_support::{key, reveal, settle, toggle};
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

#[test]
fn settings_view_selector_is_available_at_phone_desktop_and_tv_sizes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::settings_view_selector_is_available_at_phone_desktop_and_tv_sizes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    let edits = Rc::new(RefCell::new(Vec::new()));
    let captured = edits.clone();
    app.on_settings_view_changed(move |advanced| captured.borrow_mut().push(advanced));
    app.show().unwrap();
    for (width, height, tv) in [
        (360, 780, false),
        (720, 360, false),
        (1280, 900, false),
        (1920, 1080, true),
    ] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_settings_detail_open(false);
        app.set_settings_advanced(false);
        settle(&app);
        let detail_before = app.get_settings_detail_open();
        let selected_before = app.get_settings_selected_id();
        let advanced = ElementHandle::find_by_accessible_label(&app, "Advanced")
            .last()
            .expect("Advanced selector");
        let pos = advanced.absolute_position();
        let size = advanced.size();
        assert!(
            pos.x >= 0.0
                && pos.y >= 0.0
                && pos.x + size.width <= width as f32 + 1.0
                && pos.y + size.height <= height as f32 + 1.0,
            "selector fits {width}x{height}"
        );
        advanced.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(app.get_settings_advanced());
        assert_eq!(edits.borrow().last(), Some(&true));
        assert_eq!(
            app.get_settings_detail_open(),
            detail_before,
            "view selection preserves navigation at {width}×{height}"
        );
        assert_eq!(app.get_settings_selected_id(), selected_before);
        ElementHandle::find_by_accessible_label(&app, "Simple")
            .last()
            .expect("Simple selector")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(!app.get_settings_advanced());
        assert_eq!(edits.borrow().last(), Some(&false));
        key(&app, slint::platform::Key::RightArrow);
        assert!(app.get_settings_advanced(), "remote can select Advanced");
        key(&app, slint::platform::Key::LeftArrow);
        assert!(!app.get_settings_advanced(), "remote can return to Simple");
    }
}

#[test]
fn simple_view_preserves_custom_cache_values_and_search_temporarily_reveals_them() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::simple_view_preserves_custom_cache_values_and_search_temporarily_reveals_them"
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
    app.set_settings_advanced(false);
    app.set_cache_images(true);
    app.set_cache_enabled(true);
    app.set_cache_format_index(0);
    app.set_cache_quality(73.0);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let disk = reveal(&app, || toggle(&app, "Disk caching"), "disk caching");
    assert!(toggle(&app, "Compress images").is_none());
    disk.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(!app.get_cache_images());
    disk.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(app.get_cache_images());
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    content.set_kb_hr(1);
    key(&app, slint::platform::Key::RightArrow);
    assert!(
        app.get_cache_enabled(),
        "a stale hidden row must not change compression"
    );
    assert_eq!(app.get_cache_quality(), 73.0);
    app.set_settings_advanced(true);
    settle(&app);
    let compress = reveal(
        &app,
        || toggle(&app, "Compress images"),
        "compression in Advanced",
    );
    assert_eq!(compress.accessible_checked(), Some(true));
    assert_eq!(app.get_cache_format_index(), 0);
    assert_eq!(app.get_cache_quality(), 73.0);
    app.set_settings_advanced(false);
    app.set_settings_detail_open(false);
    app.set_settings_search_query("Compress images".into());
    settle(&app);
    ElementQuery::from_root(&app)
        .match_predicate(|item| item.accessible_id().as_deref() == Some("settings:image-cache"))
        .find_first()
        .expect("search finds image cache")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let compression = reveal(
        &app,
        || toggle(&app, "Compress images"),
        "advanced search destination",
    );
    assert!(
        !app.get_settings_advanced(),
        "search does not persist a view change"
    );
    compression.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    toggle(&app, "Compress images")
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, slint::platform::Key::DownArrow);
    assert_eq!(content.get_kb_hr(), 2, "Down reaches the format row");
    assert!(
        toggle(&app, "Compress images").is_some(),
        "arrow navigation retains the search reveal"
    );
    key(&app, slint::platform::Key::UpArrow);
    assert_eq!(content.get_kb_hr(), 1, "Up returns to compression");
    assert!(
        toggle(&app, "Compress images").is_some(),
        "reverse navigation retains the search reveal"
    );
    assert!(!app.get_settings_advanced());
    assert!(app.get_cache_enabled());
    assert_eq!(app.get_cache_format_index(), 0);
    assert_eq!(app.get_cache_quality(), 73.0);
    app.set_settings_detail_open(false);
    settle(&app);
    app.set_settings_search_query("".into());
    app.set_settings_selected_id(2);
    app.set_settings_detail_open(true);
    settle(&app);
    assert!(toggle(&app, "Compress images").is_none());
    assert!(app.get_cache_enabled());
    assert_eq!(app.get_cache_format_index(), 0);
    assert_eq!(app.get_cache_quality(), 73.0);
}

#[test]
fn simple_keyboard_skips_hidden_display_and_player_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::simple_keyboard_skips_hidden_display_and_player_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_advanced(false);
    app.set_settings_selected_id(3);
    app.set_settings_detail_open(true);
    app.set_cache_prefetch_metadata(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let thumbnails = reveal(
        &app,
        || toggle(&app, "Show unwatched thumbnails"),
        "thumbnails",
    );
    assert!(toggle(&app, "Prefetch episode metadata").is_none());
    thumbnails.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    key(&app, slint::platform::Key::UpArrow);
    assert_eq!(
        content.get_kb_dr(),
        7,
        "navigation skips the hidden date and metadata rows"
    );
    content.set_kb_dr(1);
    key(&app, slint::platform::Key::Return);
    assert!(
        app.get_cache_prefetch_metadata(),
        "a stale hidden display row cannot change its value"
    );
    app.set_settings_advanced(true);
    settle(&app);
    assert_eq!(
        reveal(
            &app,
            || toggle(&app, "Prefetch episode metadata"),
            "metadata in Advanced"
        )
        .accessible_checked(),
        Some(true)
    );

    app.set_settings_advanced(false);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.set_show_stream_loading_text(true);
    settle(&app);
    let paused = reveal(
        &app,
        || toggle(&app, "Keep controls visible while paused"),
        "paused controls",
    );
    assert!(toggle(&app, "Show loading status text").is_none());
    paused.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    key(&app, slint::platform::Key::DownArrow);
    assert_ne!(
        content.get_kb_pr(),
        content.invoke_kb_player_loading_text_row(),
        "keyboard skips hidden player rows"
    );
    content.set_kb_pr(content.invoke_kb_player_loading_text_row());
    key(&app, slint::platform::Key::Return);
    assert!(
        app.get_show_stream_loading_text(),
        "a stale hidden player row cannot change its value"
    );
    app.set_settings_advanced(true);
    settle(&app);
    assert_eq!(
        reveal(
            &app,
            || toggle(&app, "Show loading status text"),
            "loading text in Advanced"
        )
        .accessible_checked(),
        Some(true)
    );
}

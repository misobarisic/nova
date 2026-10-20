//! Scope badges open the comparison sheet without changing ordinary controls.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..16 {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.size(), e.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn badge(app: &nova::AppWindow, key: &str) -> ElementHandle {
    let id = format!("setting-scope:{key}");
    ElementQuery::from_root(app)
        .match_predicate(move |e| e.accessible_id().as_deref() == Some(id.as_str()))
        .find_all()
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!(
                "visible scope badge {key}; sync enabled {}",
                app.get_sync_enabled()
            )
        })
}
fn has_badge(app: &nova::AppWindow, key: &str) -> bool {
    let id = format!("setting-scope:{key}");
    ElementQuery::from_root(app)
        .match_predicate(move |e| e.accessible_id().as_deref() == Some(id.as_str()))
        .find_all()
        .into_iter()
        .next()
        .is_some()
}
fn click_label(app: &nova::AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(app, label)
        .last()
        .expect(label)
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}
fn reveal_speed(app: &nova::AppWindow) {
    // The new language defaults precede speed. Queries omit clipped controls,
    // so reveal the caption before comparing its bounds with the scope badge.
    for _ in 0..24 {
        if let Some(title) =
            ElementHandle::find_by_element_id(app, "SettingsPlayerModule::speed_title").next()
        {
            let y = title.absolute_position().y;
            app.set_settings_scroll_y(app.get_settings_scroll_y() + 250.0 - y);
            settle(app);
            return;
        }
        app.set_settings_scroll_y(app.get_settings_scroll_y() - 100.0);
        settle(app);
    }
    panic!("missing speed caption after scrolling");
}
#[test]
fn scoped_setting_badges_and_comparison_modal_fit_and_dispatch() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "scoped_setting_badges_and_comparison_modal_fit_and_dispatch"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.global::<nova::Anim>().set_enabled(false);
    let rows = Rc::new(VecModel::from(vec![
        nova::SettingSyncInfo {
            key: "quality".into(),
            status: 1,
            can_override: true,
            local_value: "80%".into(),
            shared_value: "80%".into(),
            has_shared: true,
            shared_supported: true,
        },
        nova::SettingSyncInfo {
            key: "playback_speed".into(),
            status: 0,
            local_value: "1×".into(),
            ..Default::default()
        },
        nova::SettingSyncInfo {
            key: "sync_enabled".into(),
            status: 0,
            local_value: "On".into(),
            ..Default::default()
        },
    ]));
    app.global::<nova::SettingsSync>()
        .set_rows(rows.clone().into());
    let indexed = rows.clone();
    app.global::<nova::SettingsSync>().on_lookup(move |key| {
        indexed
            .iter()
            .position(|row| row.key == key)
            .map(|i| i as i32)
            .unwrap_or(-1)
    });
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    let updated = rows.clone();
    app.global::<nova::SettingsSync>()
        .on_override_changed(move |key, enabled| {
            recorded.borrow_mut().push((key.to_string(), enabled));
            let mut row = updated.row_data(0).unwrap();
            row.status = if enabled { 2 } else { 1 };
            updated.set_row_data(0, row);
        });
    app.set_cache_images(true);
    app.set_cache_enabled(true);
    app.set_settings_selected_id(2);
    app.set_settings_detail_open(true);
    app.window().show().unwrap();
    settle(&app);
    assert!(
        !has_badge(&app, "quality"),
        "scope badges stay hidden when sync is disabled"
    );
    app.set_sync_enabled(true);
    settle(&app);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1400));
        settle(&app);
        let scope = badge(&app, "quality");
        assert_eq!(scope.accessible_label().as_deref(), Some("Synced"));
        if width == 1280 {
            let badge_position = scope.absolute_position();
            let badge_size = scope.size();
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Quality").any(|title| {
                    let position = title.absolute_position();
                    let size = title.size();
                    position.x + size.width <= badge_position.x - 7.5
                        && (position.y + size.height / 2.0
                            - badge_position.y
                            - badge_size.height / 2.0)
                            .abs()
                            < 0.5
                }),
                "a roomy title row must place Synced inline; badge at {badge_position:?} size {badge_size:?}; titles {:?}",
                ElementHandle::find_by_accessible_label(&app, "Quality")
                    .map(|title| (title.absolute_position(), title.size()))
                    .collect::<Vec<_>>()
            );
        }
        scope.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(app.global::<nova::SettingsSync>().get_open());
        let panel = ElementHandle::find_by_element_id(&app, "SettingSyncModal::panel")
            .next()
            .unwrap();
        let p = panel.absolute_position();
        let size = panel.size();
        assert!(p.x >= -0.5 && p.x + size.width <= width as f32 + 0.5);
        assert!(p.y >= -0.5 && p.y + size.height <= 1400.5);
        click_label(&app, "Use device override");
        assert!(!app.global::<nova::SettingsSync>().get_open());
        assert_eq!(
            badge(&app, "quality").accessible_label().as_deref(),
            Some("This device (overridden)")
        );
        // A remote change updates the comparison while the effective local value stays put.
        let mut row = rows.row_data(0).unwrap();
        row.local_value = "95%".into();
        row.shared_value = "60%".into();
        rows.set_row_data(0, row);
        badge(&app, "quality").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "60%")
                .next()
                .is_some()
        );
        click_label(&app, "Keep override and edit setting");
        assert_eq!(rows.row_data(0).unwrap().status, 2);
        badge(&app, "quality").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        click_label(&app, "Use synced value");
        assert_eq!(rows.row_data(0).unwrap().status, 1);
    }
    // This real Sync enablement row has a long title, a short pill, and a
    // narrow toggle. At 470px the combined line exceeds the old 62% text cap
    // while fitting in the actual space before the toggle.
    app.invoke_more_bridge_sync_picked();
    app.set_settings_scroll_y(0.0);
    app.window().set_size(slint::PhysicalSize::new(470, 1000));
    settle(&app);
    let sync_title = "Enable cross-device sync";
    let sync_row = ElementHandle::find_by_element_type_name(&app, "SettingsRow")
        .find(|row| {
            let title = sync_title.to_owned();
            row.query_descendants()
                .match_predicate(move |e| e.accessible_label().as_deref() == Some(title.as_str()))
                .find_first()
                .is_some()
        })
        .expect("Sync enablement row");
    let sync_badge = badge(&app, "sync_enabled");
    let bp = sync_badge.absolute_position();
    let title_element = sync_row
        .query_descendants()
        .match_predicate({
            let title = sync_title.to_owned();
            move |e| e.accessible_label().as_deref() == Some(title.as_str())
        })
        .find_all()
        .into_iter()
        .find(|title| {
            let p = title.absolute_position();
            let size = title.size();
            p.x + size.width <= bp.x - 7.5
                && (p.y + size.height / 2.0 - bp.y - sync_badge.size().height / 2.0).abs() < 0.5
        })
        .expect("the title and pill should share a line");
    let toggle = sync_row
        .query_descendants()
        .match_accessible_role(AccessibleRole::Checkbox)
        .find_first()
        .expect("sync enablement toggle");
    let inner_width = sync_row.size().width - 32.0;
    let line_width = title_element.size().width + 8.0 + sync_badge.size().width;
    let row_x = sync_row.absolute_position().x;
    let actual_space = toggle.absolute_position().x - 16.0 - (row_x + 16.0);
    assert!(
        line_width > inner_width * 0.62,
        "fixture must exceed the old text cap"
    );
    assert!(
        line_width <= actual_space + 0.5,
        "line must fit before the controls"
    );

    assert_eq!(
        actions.borrow().as_slice(),
        [
            ("quality".into(), true),
            ("quality".into(), false),
            ("quality".into(), true),
            ("quality".into(), false),
            ("quality".into(), true),
            ("quality".into(), false)
        ]
    );
    app.set_settings_selected_id(2);
    app.set_settings_scroll_y(0.0);
    app.window().set_size(slint::PhysicalSize::new(1280, 1400));
    settle(&app);
    let mut unsupported = rows.row_data(0).unwrap();
    unsupported.status = 2;
    unsupported.shared_supported = false;
    unsupported.shared_value = "future".into();
    rows.set_row_data(0, unsupported);
    badge(&app, "quality").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let before = actions.borrow().len();
    click_label(&app, "Use synced value");
    assert!(app.global::<nova::SettingsSync>().get_open());
    assert_eq!(
        actions.borrow().len(),
        before,
        "unsupported shared values cannot be applied"
    );
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    app.set_settings_selected_id(5);
    // The narrow speed title used to center itself underneath its inline
    // badge. Require separate bounds for both inline and stacked captions,
    // including translated labels and the longer override badge.
    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        for width in [320, 390, 699] {
            app.window().set_size(slint::PhysicalSize::new(width, 1400));
            for status in [0, 2] {
                let mut speed = rows.row_data(1).unwrap();
                speed.status = status;
                rows.set_row_data(1, speed);
                settle(&app);
                reveal_speed(&app);
                let title =
                    ElementHandle::find_by_element_id(&app, "SettingsPlayerModule::speed_title")
                        .next()
                        .expect("narrow playback speed title");
                let scope = badge(&app, "playback_speed");
                let tp = title.absolute_position();
                let ts = title.size();
                let bp = scope.absolute_position();
                let bs = scope.size();
                assert!(
                    tp.x + ts.width <= bp.x - 7.5 || tp.y + ts.height <= bp.y - 2.5,
                    "{language}/{width}/{status}: speed title {tp:?}/{ts:?} overlaps badge {bp:?}/{bs:?}"
                );
                assert!(bp.x >= 0.0 && bp.x + bs.width <= width as f32 + 0.5);
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    let mut speed = rows.row_data(1).unwrap();
    speed.status = 0;
    rows.set_row_data(1, speed);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    settle(&app);
    reveal_speed(&app);
    badge(&app, "playback_speed").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Use synced value")
            .next()
            .is_none()
    );
    let count = actions.borrow().len();
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!app.global::<nova::SettingsSync>().get_open());
    assert!(
        app.get_settings_detail_open(),
        "Back dismisses the modal before the page"
    );
    assert_eq!(actions.borrow().len(), count);
    rows.push(nova::SettingSyncInfo {
        key: "tracking_automatic_0".into(),
        status: 1,
        can_override: false,
        local_value: "On".into(),
        shared_value: "On".into(),
        has_shared: true,
        shared_supported: true,
    });
    rows.push(nova::SettingSyncInfo {
        key: "tracking_client_0".into(),
        status: 0,
        local_value: "fixture".into(),
        ..Default::default()
    });
    app.set_tracking_accounts(
        Rc::new(VecModel::from(vec![nova::TrackingAccountRow {
            service: 0,
            automatic: true,
            client_id: "fixture".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_settings_selected_id(12);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1400));
        settle(&app);
        if !has_badge(&app, "tracking_automatic_0") {
            ElementHandle::find_by_accessible_label(&app, "MyAnimeList")
                .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                .expect("MAL account disclosure")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
        }
        let sync = badge(&app, "tracking_automatic_0");
        assert_eq!(sync.accessible_label().as_deref(), Some("Synced"));
        sync.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(app.global::<nova::SettingsSync>().get_open());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Use device override")
                .next()
                .is_none()
        );
        click_label(&app, "Close");
        assert_eq!(
            actions.borrow().len(),
            count,
            "automatic tracking stays sync-only"
        );
        assert!(
            !has_badge(&app, "tracking_client_0"),
            "internal client registration has no account-card field or badge"
        );
        if width == 320 {
            let scope = badge(&app, "tracking_automatic_0");
            let bp = scope.absolute_position();
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Automatic sync").any(|title| {
                    let p = title.absolute_position();
                    let s = title.size();
                    p.y + s.height <= bp.y - 2.0 || p.x + s.width <= bp.x - 4.0
                }),
                "the tracking preference title and sync badge must not overlap"
            );
        }
    }
    // The narrow sheet enters from below; its handle stays centred and
    // either animation preference can make the entrance immediate.
    rows.push(nova::SettingSyncInfo {
        key: "language".into(),
        status: 1,
        can_override: true,
        local_value: "English".into(),
        shared_value: "Hrvatski".into(),
        has_shared: true,
        shared_supported: true,
    });
    // A badge beside a SettingsRow title supplies the natural text width.
    // The same label must stay compact in vertical cards and the comparison
    // sheet, whose cross-axis used to stretch it across all available space.
    for key in ["torrent_dir", "sync_device_name"] {
        rows.push(nova::SettingSyncInfo {
            key: key.into(),
            status: 1,
            ..Default::default()
        });
    }
    app.set_show_torrents(true);
    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        for width in [320, 390, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1400));
            for status in [0, 1, 2] {
                for key in ["language", "torrent_dir", "sync_device_name"] {
                    let index = rows.iter().position(|row| row.key == key).unwrap();
                    let mut row = rows.row_data(index).unwrap();
                    row.status = status;
                    rows.set_row_data(index, row);
                }
                app.set_settings_selected_id(3);
                app.set_settings_scroll_y(0.0);
                settle(&app);
                let reference = badge(&app, "language");
                let natural_width = reference.size().width;
                reference.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                let panel = ElementHandle::find_by_element_id(&app, "SettingSyncModal::panel")
                    .next()
                    .unwrap();
                let modal_badge = panel
                    .query_descendants()
                    .match_predicate(|e| {
                        e.accessible_id().as_deref() == Some("setting-scope:language")
                    })
                    .find_first()
                    .expect("comparison sheet scope badge");
                assert!(
                    (modal_badge.size().width - natural_width).abs() < 0.5,
                    "{language}/{width}/{status}: modal badge {} must fit its text width {natural_width}",
                    modal_badge.size().width,
                );
                app.global::<nova::SettingsSync>().set_open(false);
                for (page, key) in [(4, "torrent_dir"), (7, "sync_device_name")] {
                    app.set_settings_selected_id(page);
                    app.set_settings_scroll_y(0.0);
                    settle(&app);
                    let scope = badge(&app, key);
                    let position = scope.absolute_position();
                    let size = scope.size();
                    assert!(
                        (size.width - natural_width).abs() < 0.5,
                        "{language}/{width}/{status}/{key}: vertical card badge {} must fit its text width {natural_width}",
                        size.width,
                    );
                    assert!(position.x >= -0.5 && position.x + size.width <= width as f32 + 0.5);
                }
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    let index = rows.iter().position(|row| row.key == "language").unwrap();
    let mut language = rows.row_data(index).unwrap();
    language.status = 1;
    rows.set_row_data(index, language);
    app.set_settings_selected_id(3);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    settle(&app);
    badge(&app, "language").mock_single_click(slint::platform::PointerEventButton::Left);
    let panel = ElementHandle::find_by_element_id(&app, "SettingSyncModal::panel")
        .next()
        .unwrap();
    let mut positions = vec![];
    for _ in 0..35 {
        positions.push(panel.absolute_position().y);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
    let first = positions[0];
    let last = *positions.last().unwrap();
    assert!(
        first - last > 20.0,
        "the narrow sheet must slide up from below"
    );
    assert!(
        positions
            .iter()
            .any(|y| *y > last + 1.0 && *y < first - 1.0),
        "entrance must pass through intermediate positions"
    );
    let handle = ElementHandle::find_by_element_id(&app, "SettingSyncModal::grab_handle")
        .next()
        .unwrap();
    assert!(
        (handle.absolute_position().x + handle.size().width / 2.0
            - panel.absolute_position().x
            - panel.size().width / 2.0)
            .abs()
            < 0.5,
        "the grab handle must be centred on the sheet"
    );
    for (enabled, transitions) in [(false, true), (true, false)] {
        app.global::<nova::SettingsSync>().set_open(false);
        app.global::<nova::Anim>().set_enabled(enabled);
        app.global::<nova::Anim>().set_transitions(transitions);
        settle(&app);
        badge(&app, "language").mock_single_click(slint::platform::PointerEventButton::Left);
        let y = panel.absolute_position().y;
        settle(&app);
        assert!(
            (panel.absolute_position().y - y).abs() < 0.5,
            "disabled motion must open immediately"
        );
    }
    app.global::<nova::SettingsSync>().set_open(false);
    app.set_sync_enabled(false);
    app.set_settings_selected_id(5);
    settle(&app);
    reveal_speed(&app);
    assert!(
        !has_badge(&app, "playback_speed"),
        "the custom playback-speed caption hides its pill while sync is off"
    );

    app.set_sync_enabled(true);
    app.set_settings_selected_id(3);
    app.set_settings_scroll_y(0.0);
    settle(&app);
    badge(&app, "language").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(app.global::<nova::SettingsSync>().get_open());
    let language_row = rows.iter().find(|row| row.key == "language").unwrap();
    assert_eq!(language_row.status, 1);
    app.set_sync_enabled(false);
    settle(&app);
    assert!(
        !app.global::<nova::SettingsSync>().get_open(),
        "disabling sync closes a comparison sheet before hiding its badge"
    );
    assert!(!has_badge(&app, "language"));
    assert_eq!(
        rows.iter()
            .find(|row| row.key == "language")
            .unwrap()
            .status,
        language_row.status,
        "display state leaves the stored scope policy unchanged"
    );
}

#[test]
fn comparison_modal_keeps_long_folder_values_inside_the_panel() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::comparison_modal_keeps_long_folder_values_inside_the_panel"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.set_sync_enabled(true);
    app.invoke_more_bridge_sync_picked();
    app.set_settings_detail_open(true);
    app.global::<nova::Anim>().set_enabled(false);
    let local = format!("/storage/emulated/0/Download/{}", "a".repeat(100));
    let shared = format!("/home/nova/Videos/{}", "b".repeat(100));
    let sync = app.global::<nova::SettingsSync>();
    sync.set_rows(
        Rc::new(VecModel::from(vec![nova::SettingSyncInfo {
            key: "torrent_dir".into(),
            status: 2,
            can_override: true,
            local_value: local.clone().into(),
            shared_value: shared.clone().into(),
            has_shared: true,
            shared_supported: true,
        }]))
        .into(),
    );
    sync.on_lookup(|_| 0);
    app.show().unwrap();
    settle(&app);
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for (width, height) in [(320, 800), (360, 800), (390, 844), (720, 360), (1280, 900)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            sync.invoke_show("torrent_dir".into(), "Download folder".into());
            settle(&app);
            let panel = ElementHandle::find_by_element_id(&app, "SettingSyncModal::panel")
                .next()
                .expect("comparison panel");
            let p = panel.absolute_position();
            let size = panel.size();
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(
                        p.x + size.width / 2.0,
                        p.y + size.height / 2.0,
                    ),
                    delta_x: 0.0,
                    delta_y: 10000.0,
                });
            settle(&app);
            let left = panel.absolute_position().x;
            let right = left + panel.size().width;
            for value in [&local, &shared] {
                let mut visible = None;
                for _ in 0..30 {
                    if let Some(text) = ElementHandle::find_by_accessible_label(&app, value).next()
                    {
                        visible = Some(text);
                        break;
                    }
                    let p = panel.absolute_position();
                    let size = panel.size();
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(
                                p.x + size.width / 2.0,
                                p.y + size.height / 2.0,
                            ),
                            delta_x: 0.0,
                            delta_y: -48.0,
                        });
                    settle(&app);
                }
                let text = visible.unwrap_or_else(|| panic!(
                    "{locale} comparison at {width}x{height}: cannot reveal {value}; modal open {}; panel {:?} {:?}; labels {:?}",
                    sync.get_open(), panel.absolute_position(), panel.size(),
                    panel.query_descendants().match_predicate(|_| true).find_all().iter()
                        .filter_map(|element| element.accessible_label()).collect::<Vec<_>>()
                ));
                let p = text.absolute_position();
                assert!(
                    p.x >= left && p.x + text.size().width <= right + 0.5,
                    "{locale} comparison at {width}x{height}: value {:?} {:?} exceeds panel {left}..{right}",
                    p,
                    text.size()
                );
            }
            sync.set_open(false);
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn tmdb_badge_opens_masked_comparison_and_switches_scope() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tmdb_badge_opens_masked_comparison_and_switches_scope"
    )) {
        return;
    }
    // This case runs in its own subprocess, so both backend languages can be
    // checked here without a separate UI library test executable.
    for (language, configured, empty) in [
        (
            nova_config::Language::English,
            "Configured (hidden)",
            "Not configured",
        ),
        (
            nova_config::Language::Croatian,
            "Postavljeno (skriveno)",
            "Nije postavljeno",
        ),
    ] {
        nova_ui::backend_text::set_language(language);
        assert_eq!(
            nova_ui::backend_text::setting_sync_value(
                "tmdb_api_key",
                &serde_json::json!("0123456789abcdef0123456789abcdef")
            ),
            configured
        );
        for value in [
            serde_json::json!(""),
            serde_json::json!("   "),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                nova_ui::backend_text::setting_sync_value("tmdb_api_key", &value),
                empty
            );
        }
    }
    nova_ui::backend_text::set_language(nova_config::Language::English);
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    app.window().set_size(slint::PhysicalSize::new(390, 4000));
    let rows = Rc::new(VecModel::from(vec![nova::SettingSyncInfo {
        key: "tmdb_api_key".into(),
        status: 1,
        can_override: true,
        local_value: nova_ui::backend_text::setting_sync_value(
            "tmdb_api_key",
            &serde_json::json!("local-key"),
        )
        .into(),
        shared_value: nova_ui::backend_text::setting_sync_value(
            "tmdb_api_key",
            &serde_json::json!("shared-key"),
        )
        .into(),
        has_shared: true,
        shared_supported: true,
    }]));
    app.global::<nova::SettingsSync>()
        .set_rows(rows.clone().into());
    app.global::<nova::SettingsSync>()
        .on_lookup(|key| if key == "tmdb_api_key" { 0 } else { -1 });
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    let updated = rows.clone();
    app.global::<nova::SettingsSync>()
        .on_override_changed(move |key, enabled| {
            recorded.borrow_mut().push((key.to_string(), enabled));
            let mut row = updated.row_data(0).unwrap();
            row.status = if enabled { 2 } else { 1 };
            updated.set_row_data(0, row);
        });
    app.show().unwrap();
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
            .next()
            .is_none(),
        "TMDB starts as a collapsed service card"
    );
    ElementHandle::find_by_accessible_label(&app, "TMDB")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("TMDB enrichment disclosure")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
            .next()
            .is_some(),
        "opening TMDB reveals the masked key editor"
    );
    assert!(!has_badge(&app, "tmdb_api_key"));
    app.set_sync_enabled(true);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 4000));
        settle(&app);
        let scope = badge(&app, "tmdb_api_key");
        assert_eq!(scope.accessible_label().as_deref(), Some("Synced"));
        let p = scope.absolute_position();
        let size = scope.size();
        assert!(p.x >= 0.0 && p.x + size.width <= width as f32 + 0.5);
        assert!(size.width < 220.0, "pill retains its natural width");
        scope.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(
            app.global::<nova::SettingsSync>().get_selected_key(),
            "tmdb_api_key"
        );
        assert_eq!(
            app.global::<nova::SettingsSync>().get_selected_title(),
            "TMDB recommendations"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Configured (hidden)")
                .next()
                .is_some()
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "local-key")
                .next()
                .is_none()
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "shared-key")
                .next()
                .is_none()
        );
        click_label(&app, "Use device override");
        assert_eq!(
            badge(&app, "tmdb_api_key").accessible_label().as_deref(),
            Some("This device (overridden)")
        );
        badge(&app, "tmdb_api_key").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        click_label(&app, "Use synced value");
        assert_eq!(
            badge(&app, "tmdb_api_key").accessible_label().as_deref(),
            Some("Synced")
        );
    }
    let expected = (0..3)
        .flat_map(|_| {
            [
                ("tmdb_api_key".to_owned(), true),
                ("tmdb_api_key".to_owned(), false),
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(&*actions.borrow(), &expected);
    app.set_sync_enabled(false);
    settle(&app);
    assert!(!has_badge(&app, "tmdb_api_key"));
}

#[test]
fn tmdb_disclosure_fits_and_keeps_key_editing_and_back_local() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tmdb_disclosure_fits_and_keeps_key_editing_and_back_local"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        app.set_settings_scroll_y(0.0);
        settle(&app);
        let header = || {
            ElementHandle::find_by_accessible_label(&app, "TMDB")
                .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                .expect("TMDB service disclosure")
        };
        assert!(
            ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
                .next()
                .is_none()
        );
        let p = header().absolute_position();
        let size = header().size();
        assert!(p.x >= 0.0 && p.x + size.width <= width as f32 + 0.5);
        assert!(
            size.height >= 96.0 && size.height <= 130.0,
            "TMDB icon must keep a compact disclosure header"
        );
        header().mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        let input = ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
            .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .expect("expanded masked TMDB editor");
        let p = input.absolute_position();
        let size = input.size();
        assert!(p.x >= 0.0 && p.x + size.width <= width as f32 + 0.5);
        assert!(size.height >= 44.0);
        input.set_accessible_value("samplekey");
        settle(&app);
        assert_eq!(app.get_tmdb_api_key(), "samplekey");
        assert_eq!(
            edits.borrow().last().map(String::as_str),
            Some("tmdb_api_key")
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "samplekey")
                .next()
                .is_none(),
            "configured key must not become a visible caption"
        );
        input.mock_single_click(slint::platform::PointerEventButton::Left);
        let text: slint::SharedString = "x".into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        settle(&app);
        assert!(
            app.get_tmdb_api_key().contains('x'),
            "the real key input receives typed text"
        );
        assert!(
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_tracking_input_focused()
        );
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle(&app);
        assert!(
            !(*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_tracking_input_focused()
        );
        assert!(
            app.get_show_settings() && app.get_settings_detail_open(),
            "Back blurs the key before leaving Integrations"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
                .next()
                .is_some()
        );
        header().mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
                .next()
                .is_none()
        );
        assert!(
            !(*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_tracking_input_focused()
        );
    }
}

#[test]
#[ignore = "requires a display; optional native TMDB integration card preview"]
fn render_tmdb_integration_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_tmdb_integration_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    app.set_tmdb_api_key("sample-preview-key".into());
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let step = Rc::new(std::cell::Cell::new(0usize));
    let phase = step.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let names = [
                "tmdb-integration-collapsed-desktop",
                "tmdb-integration-expanded-desktop",
                "tmdb-integration-collapsed-phone",
                "tmdb-integration-expanded-phone",
            ];
            let pixels = app.window().take_snapshot().unwrap();
            if let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    std::path::PathBuf::from(dir).join(format!("{}.png", names[phase.get()])),
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            let toggle = || {
                let header = ElementHandle::find_by_accessible_label(&app, "TMDB")
                    .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .expect("TMDB header in native preview");
                let p = header.absolute_position();
                let s = header.size();
                let position =
                    slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    });
            };
            match phase.get() {
                0 => {
                    toggle();
                    app.set_settings_scroll_y(-400.0);
                }
                1 => {
                    toggle();
                    app.window().set_size(slint::PhysicalSize::new(390, 844));
                    app.set_settings_scroll_y(-300.0);
                }
                2 => {
                    toggle();
                    app.set_settings_scroll_y(-500.0);
                }
                _ => slint::quit_event_loop().unwrap(),
            }
            phase.set(phase.get() + 1);
        },
    );
    app.run().unwrap();
}

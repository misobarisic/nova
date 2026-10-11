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
            ElementHandle::find_by_element_id(app, "SettingsPage::speed_title").next()
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
    app.set_settings_selected_id(7);
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
                let title = ElementHandle::find_by_element_id(&app, "SettingsPage::speed_title")
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
        assert_eq!(
            badge(&app, "tracking_client_0")
                .accessible_label()
                .as_deref(),
            Some("This device")
        );
        if width == 320 {
            let scope = badge(&app, "tracking_automatic_0");
            let bp = scope.absolute_position();
            assert!(
                ElementHandle::find_by_accessible_label(
                    &app,
                    "Automatic tracking for linked releases"
                )
                .any(|title| {
                    let p = title.absolute_position();
                    let s = title.size();
                    p.y + s.height <= bp.y - 2.0
                }),
                "a narrow row must stack its long title above the scope pill"
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

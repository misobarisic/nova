//! Scope badges open the comparison sheet without changing ordinary controls.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
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
        .expect("visible scope badge")
}
fn click_label(app: &nova::AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(app, label)
        .last()
        .expect(label)
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
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
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1400));
        settle(&app);
        let scope = badge(&app, "quality");
        assert_eq!(scope.accessible_label().as_deref(), Some("Synced"));
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
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.set_settings_selected_id(5);
    settle(&app);
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
}

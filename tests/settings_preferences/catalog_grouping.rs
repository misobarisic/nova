//! Presentation rules preserve native source actions and normal Settings focus.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        if let Some(view) =
            ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll").next()
        {
            let _ = (view.size(), view.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn by_id(app: &nova::AppWindow, id: &str) -> Option<ElementHandle> {
    let id = id.to_owned();
    ElementQuery::from_root(app)
        .match_predicate(move |item| item.accessible_id().as_deref() == Some(id.as_str()))
        .find_first()
}
fn tap(app: &nova::AppWindow, item: &ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    for event in [
        WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}
fn reveal(app: &nova::AppWindow, id: &str) -> ElementHandle {
    for attempt in 0..45 {
        if let Some(item) = by_id(app, id) {
            let view = ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll")
                .next()
                .unwrap();
            let content = (**app).global::<nova_ui_shared::SettingsContent>();
            let top = (content.get_detail_sticky_top() + content.get_detail_header_height())
                .max(view.absolute_position().y)
                + 4.0;
            let bottom = view.absolute_position().y + view.size().height - 8.0;
            let p = item.absolute_position();
            let s = item.size();
            if p.y >= top && p.y + s.height <= bottom {
                return item;
            }
            app.set_settings_scroll_y((app.get_settings_scroll_y() + top - p.y).min(0.0));
        } else if attempt == 0 {
            app.set_settings_scroll_y(0.0);
        } else {
            app.set_settings_scroll_y(app.get_settings_scroll_y() - 180.0);
        }
        settle(app);
    }
    panic!("could not reveal {id}")
}
fn rows() -> Vec<nova::CatalogGroupingRow> {
    vec![
        nova::CatalogGroupingRow {
            key: "opaque-a".into(),
            title: "A long catalog title with several confirmed seasons".into(),
            addon: "Installed addon".into(),
            media_type: "TV".into(),
            available: true,
            home_mode: 1,
            discover_mode: 3,
        },
        nova::CatalogGroupingRow {
            key: "opaque-old".into(),
            title: "Saved catalog".into(),
            addon: "Unavailable addon".into(),
            media_type: "TV".into(),
            available: false,
            home_mode: 2,
            discover_mode: 0,
        },
    ]
}
#[test]
fn grouping_controls_fit_translate_route_independent_modes_and_keep_unavailable_overrides() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::grouping_controls_fit_translate_route_independent_modes_and_keep_unavailable_overrides"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(3);
    app.set_settings_detail_open(true);
    let model = Rc::new(VecModel::from(rows()));
    app.set_catalog_grouping_rows(model.clone().into());
    let sync = app.global::<nova::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(
        Rc::new(VecModel::from(
            [
                "catalog_grouping_default",
                "catalog_grouping_overrides",
                "catalog_grouping_infer_series",
            ]
            .into_iter()
            .map(|key| nova::SettingSyncInfo {
                key: key.into(),
                status: 1,
                can_override: true,
                has_shared: true,
                shared_supported: true,
                local_value: "Original".into(),
                shared_value: "Original".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
        ))
        .into(),
    );
    sync.on_lookup(|key| match key.as_str() {
        "catalog_grouping_default" => 0,
        "catalog_grouping_overrides" => 1,
        "catalog_grouping_infer_series" => 2,
        _ => -1,
    });

    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    for field in [
        "catalog_grouping_default",
        "catalog_grouping_overrides",
        "catalog_grouping_infer_series",
    ] {
        tap(&app, &reveal(&app, &format!("setting-scope:{field}")));
        settle(&app);
        assert!(sync.get_open());
        assert_eq!(sync.get_selected_key(), field);
        key(&app, slint::platform::Key::Escape);
        assert!(!sync.get_open());
    }
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for width in [320, 390, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.window().dispatch_event(WindowEvent::Resized {
                size: slint::LogicalSize::new(width as f32, 900.0),
            });
            settle(&app);
            for id in [
                "catalog-grouping-default",
                "catalog-grouping-infer-series",
                "catalog-grouping-search",
                "catalog-grouping-0:opaque-a",
                "catalog-grouping-1:opaque-a",
                "catalog-grouping-remove:opaque-old",
            ] {
                let item = reveal(&app, id);
                let p = item.absolute_position();
                let s = item.size();
                assert!(s.height >= 44.0);
                assert!(
                    p.x >= 0.0 && p.x + s.width <= width as f32 + 1.0,
                    "{id} overflows at {width}"
                );
            }
            let unavailable = by_id(&app, "catalog-grouping-0:opaque-old").unwrap();
            assert_eq!(unavailable.accessible_enabled(), Some(true));
            assert_eq!(
                by_id(&app, "catalog-grouping-remove:opaque-old")
                    .unwrap()
                    .accessible_enabled(),
                Some(true)
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let captured = calls.clone();
    let changed = model.clone();
    app.on_catalog_grouping_override_changed(move |id, surface, mode| {
        captured.borrow_mut().push((id.to_string(), surface, mode));
        if id != "opaque-a" {
            return;
        }
        let mut row = changed.row_data(0).unwrap();
        if surface == 0 {
            row.home_mode = mode + 1;
        } else {
            row.discover_mode = mode + 1;
        }
        changed.set_row_data(0, row);
    });
    let settings = app.global::<nova::SettingsBridge>();
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    settings.set_kb_active(true);
    settings.set_kb_zone(6);
    content.set_kb_dr(11);
    key(&app, slint::platform::Key::RightArrow);
    assert_eq!(calls.borrow().last(), Some(&("opaque-a".into(), 0, 1)));
    content.set_kb_dr(12);
    key(&app, slint::platform::Key::LeftArrow);
    assert_eq!(calls.borrow().last(), Some(&("opaque-a".into(), 1, 1)));
    content.set_kb_dr(14);
    key(&app, slint::platform::Key::RightArrow);
    assert_eq!(calls.borrow().last(), Some(&("opaque-old".into(), 0, 2)));
    let removed = Rc::new(RefCell::new(Vec::new()));
    let captured = removed.clone();
    app.on_catalog_grouping_override_removed(move |id| captured.borrow_mut().push(id.to_string()));
    tap(&app, &reveal(&app, "catalog-grouping-remove:opaque-old"));
    assert_eq!(&*removed.borrow(), &["opaque-old"]);
    content.set_kb_dr(10);
    key(&app, slint::platform::Key::Return);
    assert!(content.get_input_focused());
    key(&app, slint::platform::Key::Back);
    assert!(!content.get_input_focused());
    assert!(app.get_settings_detail_open(), "Back first blurs search");
}
#[test]
fn inferred_series_toggle_routes_touch_keyboard_and_scope_at_phone_and_desktop_widths() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::inferred_series_toggle_routes_touch_keyboard_and_scope_at_phone_and_desktop_widths"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(3);
    app.set_settings_detail_open(true);
    let changes = Rc::new(RefCell::new(Vec::new()));
    let recorded = changes.clone();
    app.on_catalog_grouping_infer_series_changed(move |enabled| {
        recorded.borrow_mut().push(enabled)
    });
    let bridge = app.global::<nova::SettingsBridge>();
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    app.show().unwrap();
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, 900.0),
        });
        settle(&app);
        app.set_catalog_grouping_infer_series(false);
        tap(&app, &reveal(&app, "catalog-grouping-infer-series"));
        settle(&app);
        assert!(app.get_catalog_grouping_infer_series());
        assert_eq!(changes.borrow().last(), Some(&true));

        bridge.set_kb_active(true);
        bridge.set_kb_zone(6);
        content.set_kb_dr(9);
        key(&app, slint::platform::Key::Return);
        assert!(!app.get_catalog_grouping_infer_series());
        assert_eq!(changes.borrow().last(), Some(&false));
        key(&app, slint::platform::Key::RightArrow);
        assert!(app.get_catalog_grouping_infer_series());
        key(&app, slint::platform::Key::LeftArrow);
        assert!(!app.get_catalog_grouping_infer_series());

        content.set_kb_dr(8);
        key(&app, slint::platform::Key::DownArrow);
        assert_eq!(
            content.get_kb_dr(),
            9,
            "default grouping leads to inference"
        );
        key(&app, slint::platform::Key::DownArrow);
        assert_eq!(content.get_kb_dr(), 10, "inference leads to search");
        key(&app, slint::platform::Key::UpArrow);
        assert_eq!(content.get_kb_dr(), 9);
    }
    assert_eq!(
        &*changes.borrow(),
        &[true, false, true, false, true, false, true, false]
    );
}

#[test]
fn source_chooser_keeps_exit_image_inactive_cancels_reopen_and_uses_native_keys() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::source_chooser_keeps_exit_image_inactive_cancels_reopen_and_uses_native_keys"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    let bridge = app.global::<nova::CatalogGroupingBridge>();
    let anim = app.global::<nova::Anim>();
    anim.set_enabled(true);
    anim.set_transitions(true);
    let members = || {
        Rc::new(VecModel::from(vec![
            nova::CatalogGroupMemberRow {
                key: "native-a".into(),
                title: "Season 1 · Original addon".into(),
                subtitle: "Confirmed native source".into(),
            },
            nova::CatalogGroupMemberRow {
                key: "native-b".into(),
                title: "Season 2 · Another addon".into(),
                subtitle: "Another native source".into(),
            },
        ]))
        .into()
    };
    app.set_catalog_grouping_chooser_members(members());
    app.set_catalog_grouping_chooser_title("One show, several native sources".into());
    let picks = Rc::new(RefCell::new(Vec::new()));
    let captured = picks.clone();
    app.on_catalog_grouping_member_picked(move |key| captured.borrow_mut().push(key.to_string()));
    let weak = app.as_weak();
    app.on_catalog_grouping_chooser_closed(move || {
        let app = weak.upgrade().unwrap();
        app.set_catalog_grouping_chooser_open(false);
        app.set_catalog_grouping_chooser_members(Default::default());
        app.set_catalog_grouping_chooser_title("".into());
    });
    app.show().unwrap();
    app.set_catalog_grouping_chooser_open(true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    let card = || {
        ElementHandle::find_by_element_id(&app, "CatalogSourceChooser::chooser_card")
            .next()
            .unwrap()
    };
    let incoming_y = card().absolute_position().y;
    for _ in 0..20 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
    assert!(
        incoming_y > card().absolute_position().y + 4.0,
        "opening card must animate into place"
    );
    let row = by_id(&app, "catalog-member:native-b").unwrap();
    tap(&app, &row);
    assert_eq!(&*picks.borrow(), &["native-b"]);
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(
        picks.borrow().last().map(String::as_str),
        Some("native-b"),
        "arrow selection and native focus must agree"
    );
    key(&app, slint::platform::Key::Back);
    assert!(!bridge.get_chooser_open());
    assert!(bridge.get_chooser_visual_retained());
    assert!(
        by_id(&app, "catalog-member:native-b").is_some(),
        "rendered snapshot survives backend clear"
    );
    let count = picks.borrow().len();
    tap(&app, &row);
    assert_eq!(
        picks.borrow().len(),
        count,
        "outgoing choices cannot dispatch"
    );
    app.set_catalog_grouping_chooser_members(members());
    app.set_catalog_grouping_chooser_open(true);
    settle(&app);
    for _ in 0..20 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
    assert!(
        bridge.get_chooser_open() && bridge.get_chooser_visual_retained(),
        "old exit cannot remove reopened chooser"
    );
    key(&app, slint::platform::Key::Escape);
    anim.set_enabled(false);
    settle(&app);
    assert!(!bridge.get_chooser_visual_retained());
    assert!(by_id(&app, "catalog-member:native-b").is_none());
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.set_catalog_grouping_chooser_members(members());
        app.set_catalog_grouping_chooser_open(true);
        settle(&app);
        let item = by_id(&app, "catalog-member:native-a").unwrap();
        assert!(item.absolute_position().x >= 0.0);
        assert!(item.absolute_position().x + item.size().width <= width as f32);
        assert!(item.size().height >= 44.0);
        key(&app, slint::platform::Key::Escape);
        assert!(!bridge.get_chooser_visual_retained());
    }
}
#[test]
fn paused_discover_requires_explicit_continue_and_detail_scope_has_all_seasons_fallback() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::paused_discover_requires_explicit_continue_and_detail_scope_has_all_seasons_fallback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.set_catalog(
        Rc::new(VecModel::from(vec![nova::MediaCard {
            id: "native".into(),
            title: "Grouped show".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_discover_pagination_paused(true);
    app.set_discover_pagination_hint("More source pages are available.".into());
    app.set_can_load_more(true);
    let automatic = Rc::new(Cell::new(0));
    let captured = automatic.clone();
    app.on_load_more(move || captured.set(captured.get() + 1));
    let manual = Rc::new(Cell::new(0));
    let captured = manual.clone();
    app.on_discover_continue_pagination(move || captured.set(captured.get() + 1));
    app.show().unwrap();
    settle(&app);
    let discover = app.global::<nova::DiscoverBridge>();
    discover.set_kb_active(true);
    discover.set_kb_zone(3);
    discover.set_kb_idx(0);
    key(&app, slint::platform::Key::DownArrow);
    assert_eq!(automatic.get(), 0);
    assert_eq!(discover.get_kb_zone(), 4);
    key(&app, slint::platform::Key::Return);
    assert_eq!(manual.get(), 1);
    let cards = (0..60)
        .map(|index| nova::MediaCard {
            id: format!("native-{index}").into(),
            title: format!("Item {index}").into(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    app.set_catalog(Rc::new(VecModel::from(cards.clone())).into());
    discover.set_kb_zone(2);
    settle(&app);
    let card_y = |label: &str| {
        ElementHandle::find_by_accessible_label(&app, label)
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
            .unwrap()
            .absolute_position()
            .y
    };
    let stride = card_y("Item 2") - card_y("Item 0");
    assert!(stride > 100.0);
    app.set_discover_scroll_y(-1200.0);
    settle(&app);
    let before = app.get_discover_scroll_y();
    app.set_catalog(
        Rc::new(VecModel::from(
            cards.into_iter().skip(2).collect::<Vec<_>>(),
        ))
        .into(),
    );
    discover.set_kb_idx(6);
    discover.invoke_remap_catalog_anchor(8, 6);
    settle(&app);
    assert!(
        (app.get_discover_scroll_y() - before - stride).abs() < 2.0,
        "reprojection preserves the anchor's viewport offset"
    );
    assert_eq!(
        automatic.get(),
        0,
        "anchor restoration cannot restart paused automatic loading"
    );
    app.set_modal_visible(true);
    app.set_detail_is_movie(false);
    app.set_detail_tab(3);
    app.set_modal_episodes(false);
    app.set_catalog_grouping_detail_scope_active(true);
    app.set_catalog_grouping_detail_season_label("Season 2".into());
    app.set_catalog_grouping_detail_scope_notice("This mapping needs confirmation.".into());
    let cleared = Rc::new(Cell::new(0));
    let captured = cleared.clone();
    app.on_catalog_grouping_detail_all_seasons(move || captured.set(captured.get() + 1));
    settle(&app);
    let all = ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::all_seasons")
        .next()
        .expect("invalid scope keeps explicit All seasons available");
    tap(&app, &all);
    assert_eq!(cleared.get(), 1);
}

//! Casting retention choices stay accessible, synced and independent of local playback backend.
#![cfg(not(feature = "live-preview"))]
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
    }
}
fn control(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_id(app, "SettingsPlayerModule::remux_buffer").next()
}
fn reveal(app: &nova::AppWindow) -> ElementHandle {
    for attempt in 0..80 {
        settle(app);
        if let Some(item) = control(app) {
            let y = item.absolute_position().y;
            app.set_settings_scroll_y((app.get_settings_scroll_y() - y + 300.0).min(0.0));
            settle(app);
            return control(app).unwrap();
        }
        app.set_settings_scroll_y(-80.0 * (attempt + 1) as f32);
    }
    panic!("missing casting remux buffer choice");
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}

#[test]
fn casting_remux_buffer_choices_fit_translate_and_route_keyboard_and_sync_scope() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::casting_remux_buffer_choices_fit_translate_and_route_keyboard_and_sync_scope"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::CastBridge>().set_available(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.set_sync_enabled(true);
    app.global::<nova::SettingsSync>().set_rows(
        Rc::new(VecModel::from(vec![nova::SettingSyncInfo {
            key: "casting_remux_buffer".into(),
            status: 1,
            can_override: true,
            local_value: "64 MiB".into(),
            shared_value: "64 MiB".into(),
            has_shared: true,
            shared_supported: true,
        }]))
        .into(),
    );
    app.global::<nova::SettingsSync>()
        .on_lookup(|key| if key == "casting_remux_buffer" { 0 } else { -1 });
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for (locale, label) in [
        ("en", "Casting remux buffer"),
        ("hr", "Međuspremnik za remuksiranje pri emitiranju"),
    ] {
        slint::select_bundled_translation(locale).unwrap();
        for android in [false, true] {
            app.set_is_android(android);
            for backend in [0, 1] {
                app.set_player_backend_index(backend);
                for width in [360, 1280] {
                    app.window().set_size(slint::PhysicalSize::new(width, 900));
                    app.set_settings_scroll_y(0.0);
                    let item = reveal(&app);
                    assert_eq!(item.accessible_role(), Some(AccessibleRole::Combobox));
                    assert_eq!(item.accessible_label().as_deref(), Some(label));
                    assert!(item.size().height >= 44.0);
                    assert!(item.absolute_position().x >= 0.0);
                    assert!(item.absolute_position().x + item.size().width <= width as f32 + 1.0);
                    for (index, size) in ["32 MiB", "64 MiB", "128 MiB", "256 MiB"]
                        .into_iter()
                        .enumerate()
                    {
                        control(&app)
                            .unwrap()
                            .mock_single_click(slint::platform::PointerEventButton::Left);
                        settle(&app);
                        ElementHandle::find_by_accessible_label(&app, size)
                            .last()
                            .expect(size)
                            .mock_single_click(slint::platform::PointerEventButton::Left);
                        settle(&app);
                        assert_eq!(app.get_casting_remux_buffer_index(), index as i32);
                        assert_eq!(
                            control(&app).unwrap().accessible_value().as_deref(),
                            Some(size)
                        );
                    }
                    key(&app, slint::platform::Key::LeftArrow);
                    assert_eq!(app.get_casting_remux_buffer_index(), 2);
                    key(&app, slint::platform::Key::Return);
                    assert_eq!(app.get_casting_remux_buffer_index(), 3);
                    key(&app, slint::platform::Key::DownArrow);
                    key(&app, slint::platform::Key::LeftArrow);
                    assert!(
                        !app.global::<nova::StreamFiltersBridge>().get_enabled(),
                        "next row remains stream rules for either backend"
                    );
                    reveal(&app);
                    let scope = ElementQuery::from_root(&app)
                        .match_predicate(|item| {
                            item.accessible_id().as_deref()
                                == Some("setting-scope:casting_remux_buffer")
                        })
                        .find_first()
                        .expect("normal synced setting scope badge");
                    assert!(scope.size().height >= 24.0);
                }
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    app.set_is_android(true);
    app.set_player_backend_index(1);
    app.set_casting_remux_buffer_index(0);
    app.set_settings_detail_open(false);
    app.set_settings_search_query("remux buffer".into());
    settle(&app);
    ElementQuery::from_root(&app)
        .match_predicate(|item| item.accessible_id().as_deref() == Some("settings:player"))
        .find_first()
        .expect("search finds Player for the remux buffer")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    reveal(&app);
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    assert_eq!(
        content.get_kb_pr(),
        content.invoke_kb_player_stream_filters_row() - 1,
        "search selects the precise buffer keyboard row"
    );
    key(&app, slint::platform::Key::RightArrow);
    assert_eq!(app.get_casting_remux_buffer_index(), 1);
    assert!(
        edits
            .borrow()
            .iter()
            .any(|field| field == "casting_remux_buffer")
    );
}

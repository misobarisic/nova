//! Declarative-only providers use the ordinary gear at both layout sizes.
#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}
fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    assert!(p.x >= 0.0 && p.y >= 0.0 && s.width > 0.0 && s.height > 0.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle();
}
fn label(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label)
}

fn toggle(app: &nova::AppWindow, label: &str) -> ElementHandle {
    // The underlying Addons capability filters use the same translated labels.
    // Target the sheet's checkbox, rather than the earlier filter button.
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
        .expect(label)
}

#[test]
fn schema_only_configuration_saves_and_confirms_global_removal() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_addon_rows(
        Rc::new(VecModel::from(vec![nova::AddonRow {
            label: "Example provider".into(),
            url: "nova-provider://example".into(),
            config_url: "nova-provider://example".into(),
            enabled: false,
            ..Default::default()
        }]))
        .into(),
    );
    let weak = app.as_weak();
    app.on_addon_configure(move |url| {
        let app = weak.upgrade().unwrap();
        app.set_addon_config_url(url);
        app.set_addon_config_label("Example provider".into());
        app.set_addon_config_capabilities(false);
        app.set_plugin_setting_rows(
            Rc::new(VecModel::from(vec![
                nova::PluginSettingRow {
                    key: "source.enabled".into(),
                    label: "Enable source".into(),
                    kind: "boolean".into(),
                    checked: false,
                    ..Default::default()
                },
                nova::PluginSettingRow {
                    key: "account.token".into(),
                    label: "Account token".into(),
                    kind: "secret".into(),
                    value: "private token".into(),
                    ..Default::default()
                },
            ]))
            .into(),
        );
        app.set_addon_config_open(true);
    });
    let changed = Rc::new(RefCell::new(Vec::new()));
    let capture = changed.clone();
    app.on_plugin_setting_changed(move |url, key, value| {
        capture
            .borrow_mut()
            .push((url.to_string(), key.to_string(), value.to_string()))
    });
    let removed = Rc::new(RefCell::new(Vec::new()));
    let capture = removed.clone();
    app.on_plugin_settings_clear(move |url| capture.borrow_mut().push(url.to_string()));
    let cache = Rc::new(RefCell::new(Vec::new()));
    let capture = cache.clone();
    app.on_plugin_cache_clear(move |url| capture.borrow_mut().push(url.to_string()));
    app.window().set_size(slint::PhysicalSize::new(390, 1000));
    app.window().show().unwrap();
    settle();
    click(
        &app,
        destinations::find(&app, "settings:addons").next().unwrap(),
    );
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        settle();
        click(&app, label(&app, "Configure"));
        assert!(app.get_addon_config_open());
        assert!(!app.get_addon_config_capabilities());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Metadata")
                .all(|e| e.accessible_role() != Some(AccessibleRole::Checkbox))
        );
        click(&app, toggle(&app, "Enable source"));
        assert_eq!(changed.borrow().last().unwrap().1, "source.enabled");
        click(&app, label(&app, "Save"));
        assert_eq!(
            changed.borrow().last().unwrap().1,
            "account.token",
            "touch Save must target the visible token field"
        );
        assert_eq!(changed.borrow().last().unwrap().2, "private token");
        click(
            &app,
            ElementHandle::find_by_accessible_label(&app, "Account token")
                .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
                .expect("the token editor must be accessible"),
        );
        let enter: SharedString = slint::platform::Key::Return.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: enter.clone(),
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: enter });
        settle();
        assert_eq!(changed.borrow().last().unwrap().2, "private token");
        click(&app, label(&app, "Clear plugin cache"));
        assert_eq!(cache.borrow().len(), if width == 390 { 1 } else { 2 });
        click(&app, label(&app, "Remove configuration everywhere"));
        let before = removed.borrow().len();
        click(&app, label(&app, "Cancel"));
        assert_eq!(removed.borrow().len(), before);
        click(&app, label(&app, "Remove configuration everywhere"));
        click(&app, label(&app, "Remove configuration"));
        assert_eq!(removed.borrow().len(), before + 1);
        let key: SharedString = slint::platform::Key::Escape.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
        settle();
        assert!(!app.get_addon_config_open());
        assert!(app.get_show_settings());
    }
    // Exercise the shipped eight-field schema, whose controls require scrolling
    // at phone widths, with the actual Croatian catalog and backend labels.
    let weak = app.as_weak();
    app.on_addon_configure(move |url| {
        let app = weak.upgrade().unwrap();
        let schema = nova_providers::provider_settings_schema("nova-provider://supernova").unwrap();
        assert_eq!(schema.len(), 8);
        let rows = schema
            .into_iter()
            .map(|field| nova::PluginSettingRow {
                key: field.key.into(),
                label: nova_ui::backend_text::tr(&field.label).into(),
                kind: field.kind.into(),
                checked: field.default.as_bool().unwrap_or(false),
                value: field
                    .default
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| field.default.to_string())
                    .into(),
                selected: field
                    .options
                    .iter()
                    .position(|o| o.value == field.default)
                    .map(|i| i as i32)
                    .unwrap_or(-1),
                options: Rc::new(VecModel::from(
                    field
                        .options
                        .iter()
                        .map(|o| SharedString::from(o.value.to_string()))
                        .collect::<Vec<_>>(),
                ))
                .into(),
                labels: Rc::new(VecModel::from(
                    field
                        .options
                        .iter()
                        .map(|o| SharedString::from(nova_ui::backend_text::tr(&o.label)))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            })
            .collect::<Vec<_>>();
        app.set_addon_config_url(url);
        app.set_addon_config_capabilities(false);
        app.set_plugin_setting_rows(Rc::new(VecModel::from(rows)).into());
        app.set_addon_config_open(true);
    });
    for (language, backend_language, gear, cache_label, remove_label, confirm_label) in [
        (
            "en",
            nova_config::Language::English,
            "Configure",
            "Clear plugin cache",
            "Remove configuration everywhere",
            "Remove configuration",
        ),
        (
            "hr",
            nova_config::Language::Croatian,
            "Konfigurirajte",
            "Obriši predmemoriju dodatka",
            "Ukloni postavke na svim uređajima",
            "Ukloni postavke",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        nova_ui::backend_text::set_language(backend_language);
        for width in [320, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1000));
            settle();
            click(&app, label(&app, gear));
            let keypress = |key: slint::platform::Key| {
                let text: SharedString = key.into();
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: text.clone(),
                    });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
                settle();
            };
            // The sheet owns focus initially: Return activates its first field.
            let before = changed.borrow().len();
            keypress(slint::platform::Key::Return);
            assert_eq!(changed.borrow().len(), before + 1);
            // Tab visits all eight schema rows and reveals the bottom actions.
            for _ in 0..8 {
                keypress(slint::platform::Key::Tab);
            }
            for caption in [cache_label, remove_label] {
                let item = label(&app, caption);
                let pos = item.absolute_position();
                let size = item.size();
                assert!(
                    pos.x >= 0.0 && pos.x + size.width <= width as f32,
                    "{language}/{width}: {caption} overflows horizontally"
                );
                assert!(
                    pos.y >= 0.0 && pos.y + size.height <= 1000.0,
                    "{language}/{width}: {caption} was not scrolled into view"
                );
            }
            let cache_pos = label(&app, cache_label).absolute_position();
            // A horizontal wheel/pan cannot move this vertical-only sheet.
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: LogicalPosition::new(cache_pos.x + 20.0, cache_pos.y + 20.0),
                    delta_x: -300.0,
                    delta_y: 0.0,
                });
            settle();
            assert_eq!(label(&app, cache_label).absolute_position().x, cache_pos.x);
            click(&app, label(&app, remove_label));
            let confirm = label(&app, confirm_label);
            let pos = confirm.absolute_position();
            let size = confirm.size();
            assert!(pos.x >= 0.0 && pos.x + size.width <= width as f32);
            assert!(pos.y >= 0.0 && pos.y + size.height <= 1000.0);
            click(&app, confirm);
            keypress(slint::platform::Key::Escape);
            assert!(!app.get_addon_config_open());
        }
    }
    nova_ui::backend_text::set_language(nova_config::Language::English);
    slint::select_bundled_translation("en").unwrap();
}

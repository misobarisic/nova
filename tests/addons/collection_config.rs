//! Collection controls stay usable without exposing unrelated or closed editors.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle();
}
fn label(app: &nova::AppWindow, text: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, text)
        .next()
        .expect(text)
}
fn scroll(app: &nova::AppWindow, delta_y: f32) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: LogicalPosition::new(app.window().size().width as f32 / 2.0, 550.0),
            delta_x: 0.0,
            delta_y,
        });
    settle();
}
fn reveal_label(app: &nova::AppWindow, text: &str, delta_y: f32) -> ElementHandle {
    for _ in 0..20 {
        if let Some(item) = ElementHandle::find_by_accessible_label(app, text).next() {
            return item;
        }
        scroll(app, delta_y);
    }
    panic!("unreachable control: {text}");
}
fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
fn row(key: &str, label: &str, kind: &str, collection: &str, role: &str) -> nova::PluginSettingRow {
    nova::PluginSettingRow {
        key: key.into(),
        label: label.into(),
        kind: kind.into(),
        collection: collection.into(),
        collection_label: collection.into(),
        item: key.into(),
        item_role: role.into(),
        ..Default::default()
    }
}
fn fixture(source: bool, account: bool) -> Vec<nova::PluginSettingRow> {
    let mut header = row("", "Sources", "collection-header", "Sources", "");
    header.collection_label = "Sources".into();
    header.item_role = "member".into();
    header.configured = true;
    let mut source_row = row(
        "source.enabled",
        "A very long predefined source label that wraps on phones",
        "boolean",
        "Sources",
        "member",
    );
    source_row.configured = source;
    source_row.checked = source;
    source_row.has_editor = true;
    let mut source_picker = row("", "Add source", "collection-picker", "Sources", "");
    source_picker.item_role = "member".into();
    source_picker.configured = true;
    source_picker.options =
        Rc::new(VecModel::from(vec![SharedString::from("source.enabled")])).into();
    source_picker.labels = Rc::new(VecModel::from(vec![SharedString::from(
        "Predefined source",
    )]))
    .into();
    let mut url = row("source.url", "Custom URL", "string", "Sources", "url");
    url.item_controller = "source.enabled".into();
    url.value = "https://example.test/custom".into();
    let mut account_row = row("account.enabled", "TorBox", "boolean", "Debrid", "enabled");
    account_row.configured = account;
    account_row.has_editor = true;
    // An account with a retained credential is configured even while disabled.
    account_row.checked = false;
    account_row.options = Rc::new(VecModel::from(vec![SharedString::from("account.key")])).into();
    let mut account_picker = row("", "Add provider", "collection-picker", "Debrid", "");
    account_picker.options =
        Rc::new(VecModel::from(vec![SharedString::from("account.enabled")])).into();
    account_picker.labels = Rc::new(VecModel::from(vec![SharedString::from("TorBox")])).into();
    let mut secret = row("account.key", "API key", "secret", "Debrid", "secret");
    secret.item_controller = "account.enabled".into();
    secret.value = "retained-private-key".into();
    let mut result = vec![header, source_row];
    if !source {
        result.push(source_picker);
    }
    result.push(url);
    let mut debrid = row("", "Debrid", "collection-header", "Debrid", "");
    debrid.selected = 1;
    result.push(debrid);
    result.push(account_row);
    if !account {
        result.push(account_picker);
    }
    result.push(secret);
    result
}

fn full_configuration() -> Vec<nova::PluginSettingRow> {
    let mut rows = fixture(true, true);
    rows[1].label = "StremThru".into();
    for (index, name) in ["Nyaa", "AnimeTosho", "The Pirate Bay / APIBay", "Knaben"]
        .into_iter()
        .enumerate()
    {
        let mut source = row(name, name, "boolean", "Sources", "member");
        source.configured = true;
        source.checked = true;
        rows.insert(index + 1, source);
    }
    let mut preferences = row("", "", "preferences-header", "", "");
    preferences.selected = 2;
    rows.push(preferences);
    let mut sort = row("sort.order", "Sort order", "sort-header", "", "");
    sort.summary = "Cached availability, Resolution".into();
    sort.labels = Rc::new(VecModel::from(vec![
        SharedString::from("Cached availability"),
        SharedString::from("Resolution"),
    ]))
    .into();
    rows.push(sort);
    for (index, name) in ["Cached availability", "Resolution"]
        .into_iter()
        .enumerate()
    {
        let mut criterion = row("sort.order", name, "sort-criterion", "", "");
        criterion.selected = index as i32;
        criterion.checked = true;
        criterion.labels = Rc::new(VecModel::from(vec![
            SharedString::from("a"),
            SharedString::from("b"),
        ]))
        .into();
        criterion.options = Rc::new(VecModel::from(vec![
            SharedString::from("up"),
            SharedString::from("down"),
            SharedString::from("toggle"),
        ]))
        .into();
        rows.push(criterion);
    }
    for name in ["Preferred quality", "Playback mode"] {
        let mut choice = row(name, name, "choice", "", "");
        choice.selected = 0;
        choice.summary = "Selected value".into();
        choice.options = Rc::new(VecModel::from(vec![SharedString::from("value")])).into();
        choice.labels = Rc::new(VecModel::from(vec![SharedString::from("Selected value")])).into();
        rows.push(choice);
    }
    let mut minimum = row("seeders.minimum", "Minimum seeders", "integer", "", "");
    minimum.value = "0".into();
    minimum.summary = "0".into();
    rows.push(minimum);
    rows
}

#[test]
fn predefined_collections_support_keyboard_edit_save_and_explicit_removal() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "predefined_collections_support_keyboard_edit_save_and_explicit_removal"
    )) {
        return;
    }
    if std::env::var_os("NOVA_ADDON_CONFIG_PREVIEW_DIR").is_some() {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_addon_config_capabilities(false);
    app.set_addon_config_url("nova-provider://fixture".into());
    let changes = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let capture = changes.clone();
    app.on_plugin_setting_changed(move |_, key, value| {
        capture
            .borrow_mut()
            .push((key.to_string(), value.to_string()));
    });
    app.window().show().unwrap();
    for (language, edit, save, remove) in [
        ("en", "Edit", "Save", "Remove"),
        ("hr", "Uredi", "Spremi", "Uklonite"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [320, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1000));
            app.set_plugin_setting_rows(Rc::new(VecModel::from(fixture(false, false))).into());
            app.set_addon_config_open(true);
            settle();
            let before = changes.borrow().len();
            // Even malformed inherited metadata cannot turn structural rows into items.
            for structural_label in ["Sources", "Add source"] {
                assert!(
                    !ElementHandle::find_by_accessible_label(&app, structural_label)
                        .any(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
                );
            }
            assert!(
                ElementHandle::find_by_accessible_label(&app, remove)
                    .next()
                    .is_none()
            );
            // Tab skips the closed URL/secret editors and unconfigured controllers.
            key(&app, slint::platform::Key::Tab);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                changes.borrow().last().unwrap(),
                &("account.enabled".into(), "true".into())
            );
            key(&app, slint::platform::Key::Escape);
            // Initial focus skips the header and the unconfigured source row.
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                changes.borrow().last().unwrap(),
                &("source.enabled".into(), "true".into())
            );
            assert_eq!(changes.borrow().len(), before + 2);
            app.set_plugin_setting_rows(Rc::new(VecModel::from(fixture(true, true))).into());
            settle();
            // Collapse the source editor opened by Add before inspecting the list.
            key(&app, slint::platform::Key::Escape);
            let account_edit_label = format!("{edit} TorBox");
            for caption in [account_edit_label.as_str()] {
                for item in ElementHandle::find_by_accessible_label(&app, caption) {
                    let pos = item.absolute_position();
                    let size = item.size();
                    assert!(
                        pos.x >= 0.0 && pos.x + size.width <= width as f32,
                        "{language}/{width}: {caption} overflows"
                    );
                }
            }
            // A disabled account remains available because it retains a key.
            let account_edit = ElementHandle::find_by_accessible_label(&app, &account_edit_label)
                .next()
                .unwrap();
            click(&app, account_edit);
            let secret = ElementHandle::find_by_accessible_label(&app, "API key")
                .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
                .unwrap();
            let pos = secret.absolute_position();
            let size = secret.size();
            assert!(pos.x >= 0.0 && pos.x + size.width <= width as f32);
            click(&app, label(&app, save));
            assert_eq!(
                changes.borrow().last().unwrap(),
                &("account.key".into(), "retained-private-key".into())
            );
            let account_remove = reveal_label(&app, remove, 120.0);
            click(&app, account_remove);
            let change_snapshot = changes.borrow();
            assert_eq!(
                &change_snapshot[change_snapshot.len() - 2..],
                &[
                    ("account.key".into(), "".into()),
                    ("account.enabled".into(), "false".into())
                ]
            );
            drop(change_snapshot);
            let source_edit_label =
                format!("{edit} A very long predefined source label that wraps on phones");
            click(&app, label(&app, &source_edit_label));
            click(&app, label(&app, remove));
            assert_eq!(
                changes.borrow().last().unwrap(),
                &("source.enabled".into(), "false".into())
            );
            assert!(
                !changes
                    .borrow()
                    .iter()
                    .any(|(key, value)| key == "source.url" && value.is_empty())
            );
            key(&app, slint::platform::Key::Escape);
            // The first Escape can close an editor; the second closes the sheet.
            if app.get_addon_config_open() {
                key(&app, slint::platform::Key::Escape);
            }
            assert!(!app.get_addon_config_open());
        }
    }
    slint::select_bundled_translation("en").unwrap();
    for width in [320, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        app.set_plugin_setting_rows(Rc::new(VecModel::from(full_configuration())).into());
        app.set_addon_config_open(true);
        settle();
        let sources = label(&app, "Sources");
        let sources_position = sources.absolute_position();
        if width >= 1000 {
            let preferences_position = label(&app, "Stream preferences").absolute_position();
            assert!(
                preferences_position.x > sources_position.x + 200.0,
                "desktop preferences must sit in the right column"
            );
            assert!(
                (preferences_position.y - sources_position.y).abs() < 32.0,
                "desktop columns must begin on the same row: sources={sources_position:?}, preferences={preferences_position:?}"
            );
        }
        for caption in ["Sources", "Stream preferences", "Edit TorBox"] {
            for item in ElementHandle::find_by_accessible_label(&app, caption) {
                let p = item.absolute_position();
                assert!(
                    p.x >= 0.0 && p.x + item.size().width <= width as f32,
                    "{width}: {caption} must fit the panel"
                );
            }
        }
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Up")
                .next()
                .is_none(),
            "priority editors stay closed until requested"
        );
        let header_x = sources_position.x;
        let source_text = ElementHandle::find_by_accessible_label(&app, "Nyaa")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Text))
            .unwrap();
        assert!(
            source_text.absolute_position().x <= header_x + 12.0,
            "source labels must align with the section heading"
        );
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            for caption in [
                "TorBox",
                "Sort order",
                "Preferred quality",
                "Playback mode",
                "Minimum seeders",
                "Clear plugin cache",
            ] {
                if ElementHandle::find_by_accessible_label(&app, caption)
                    .next()
                    .is_some()
                {
                    seen.insert(caption);
                }
            }
            scroll(&app, -160.0);
        }
        assert_eq!(
            seen.len(),
            6,
            "{width}: every section below five configured sources must remain reachable: {seen:?}"
        );
        key(&app, slint::platform::Key::Escape);
    }
    // Center within the safe rectangle, rather than the whole window: phone
    // cutouts and unequal system bars must protect both the panel and header.
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
            i_slint_core::lengths::LogicalEdges::new(62.0, 14.0, 30.0, 6.0),
        );
        app.set_addon_config_label("Safe-area addon".into());
        app.set_addon_config_open(true);
        settle();
        let panel = ElementHandle::find_by_element_id(&app, "SettingsPage::addon_config_panel")
            .next()
            .expect("configuration panel");
        let p = panel.absolute_position();
        let size = panel.size();
        assert!(
            p.x >= 30.0 && p.x + size.width <= width as f32 - 6.0,
            "{width}: panel must stay within horizontal safe area: {p:?} {size:?}"
        );
        assert!(
            p.y >= 62.0 && p.y + size.height <= 986.0,
            "{width}: panel must stay within vertical safe area: {p:?} {size:?}"
        );
        for caption in ["Safe-area addon", "Back", "Close"] {
            let item = ElementHandle::find_by_accessible_label(&app, caption)
                .find(|item| {
                    let p = item.absolute_position();
                    p.x >= panel.absolute_position().x && p.y >= panel.absolute_position().y
                })
                .expect(caption);
            let p = item.absolute_position();
            assert!(p.x >= 30.0 && p.x + item.size().width <= width as f32 - 6.0);
            assert!(p.y >= 62.0 && p.y + item.size().height <= 986.0);
        }
        click(
            &app,
            ElementHandle::find_by_accessible_label(&app, "Close")
                .find(|item| {
                    item.accessible_role() == Some(AccessibleRole::Button)
                        && item.absolute_position().y < 200.0
                })
                .expect("header Close"),
        );
        assert!(!app.get_addon_config_open());
    }
    i_slint_core::window::WindowInner::from_pub(app.window())
        .set_window_item_safe_area(i_slint_core::lengths::LogicalEdges::default());
}

// Opt-in rendering runs on the actual display backend (for example Xvfb with
// SLINT_BACKEND=winit-software), keeping ordinary geometry tests deterministic.
#[test]
fn render_configuration_reference_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "render_configuration_reference_previews"
    )) {
        return;
    }
    let Some(dir) = std::env::var_os("NOVA_ADDON_CONFIG_PREVIEW_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_addon_config_capabilities(false);
    app.set_addon_config_label("Example addon".into());
    app.set_plugin_setting_rows(Rc::new(VecModel::from(full_configuration())).into());
    app.set_addon_config_open(true);
    app.window().set_size(slint::PhysicalSize::new(390, 1100));
    app.show().unwrap();
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    let step = Rc::new(RefCell::new(0));
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let snapshot = app.window().take_snapshot().unwrap();
            let pixels = snapshot
                .as_slice()
                .iter()
                .flat_map(|p| [p.r, p.g, p.b, p.a])
                .collect::<Vec<_>>();
            let width = app.window().size().width;
            image::save_buffer(
                dir.join(format!("addon-config-{width}.png")),
                &pixels,
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
            if *step.borrow() == 0 {
                *step.borrow_mut() = 1;
                app.window().set_size(slint::PhysicalSize::new(1280, 1000));
            } else {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
}

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
    result.push(row("", "Debrid", "collection-header", "Debrid", ""));
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
    let mut sort = row("sort.order", "Sort order", "sort-header", "", "");
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
        choice.options = Rc::new(VecModel::from(vec![SharedString::from("value")])).into();
        choice.labels = Rc::new(VecModel::from(vec![SharedString::from("Selected value")])).into();
        rows.push(choice);
    }
    let mut minimum = row("seeders.minimum", "Minimum seeders", "integer", "", "");
    minimum.value = "0".into();
    rows.push(minimum);
    rows
}

#[test]
fn predefined_collections_support_keyboard_edit_save_and_explicit_removal() {
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
        let header_x = label(&app, "Sources").absolute_position().x;
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
}

//! Manifest reorder rows expose all three actions on narrow and wide layouts.
use slint::{ComponentHandle, SharedString, VecModel};
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
#[test]
fn generic_sort_rows_have_keyboard_actions_and_fit_phone_and_desktop() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_addon_config_capabilities(false);
    app.set_addon_config_url("nova-provider://generic".into());
    let captured = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let changes = captured.clone();
    app.on_plugin_setting_changed(move |_, key, value| {
        changes
            .borrow_mut()
            .push((key.to_string(), value.to_string()))
    });
    app.window().show().unwrap();
    for (language, up, down, direction) in [
        ("en", "Up", "Down", "High first"),
        ("hr", "Gore", "Dolje", "Više prvo"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [320, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1000));
            let mut rows = vec![
                nova::PluginSettingRow {
                    kind: "preferences-header".into(),
                    ..Default::default()
                },
                nova::PluginSettingRow {
                    key: "sort.order".into(),
                    kind: "sort-header".into(),
                    label: "Sort order".into(),
                    ..Default::default()
                },
            ];
            for (index, label) in ["Cached availability", "Resolution", "Reported seeders"]
                .into_iter()
                .enumerate()
            {
                rows.push(nova::PluginSettingRow {
                    key: "sort.order".into(),
                    kind: "sort-criterion".into(),
                    label: label.into(),
                    item: label.into(),
                    selected: index as i32,
                    checked: true,
                    labels: Rc::new(VecModel::from(vec![
                        SharedString::from("a"),
                        SharedString::from("b"),
                        SharedString::from("c"),
                    ]))
                    .into(),
                    options: Rc::new(VecModel::from(vec![
                        SharedString::from("up-action"),
                        SharedString::from("down-action"),
                        SharedString::from("toggle-action"),
                    ]))
                    .into(),
                    ..Default::default()
                });
            }
            app.set_plugin_setting_rows(Rc::new(VecModel::from(rows)).into());
            app.set_addon_config_open(true);
            settle();
            key(&app, slint::platform::Key::Return);
            for caption in [up, down, direction] {
                for item in
                    i_slint_backend_testing::ElementHandle::find_by_accessible_label(&app, caption)
                {
                    let position = item.absolute_position();
                    let size = item.size();
                    assert!(
                        position.x >= 0.0 && position.x + size.width <= width as f32,
                        "{language}/{width}: {caption} overflows"
                    );
                }
            }
            let before = captured.borrow().len();
            // Enter the first criterion; Up is bounded at the top.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(captured.borrow().len(), before);
            key(&app, slint::platform::Key::Tab);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("sort.order".into(), "down-action".into())
            );
            key(&app, slint::platform::Key::Tab);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("sort.order".into(), "toggle-action".into())
            );
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("sort.order".into(), "up-action".into())
            );
            key(&app, slint::platform::Key::Escape);
            assert!(app.get_addon_config_open());
            key(&app, slint::platform::Key::Escape);
            assert!(!app.get_addon_config_open());

            // Ordered preferences reuse the same compact card and navigation,
            // with removal instead of a sort-direction action.
            let mut rows = vec![
                nova::PluginSettingRow {
                    kind: "preferences-header".into(),
                    ..Default::default()
                },
                nova::PluginSettingRow {
                    key: "codec.prefs".into(),
                    kind: "priority-header".into(),
                    label: "Preferred video codecs".into(),
                    ..Default::default()
                },
            ];
            for (index, caption) in ["AVC / H.264", "HEVC / H.265"].into_iter().enumerate() {
                rows.push(nova::PluginSettingRow {
                    key: "codec.prefs".into(),
                    kind: "priority-criterion".into(),
                    label: caption.into(),
                    selected: index as i32,
                    labels: Rc::new(VecModel::from(vec![
                        SharedString::from("AVC"),
                        SharedString::from("HEVC"),
                    ]))
                    .into(),
                    options: Rc::new(VecModel::from(vec![
                        SharedString::from("move-up"),
                        SharedString::from("move-down"),
                        SharedString::from("remove-choice"),
                    ]))
                    .into(),
                    ..Default::default()
                });
            }
            rows.push(nova::PluginSettingRow {
                key: "codec.prefs".into(),
                kind: "priority-picker".into(),
                label: "Add preference".into(),
                options: Rc::new(VecModel::from(vec![SharedString::from("add-choice")])).into(),
                labels: Rc::new(VecModel::from(vec![SharedString::from("AV1")])).into(),
                ..Default::default()
            });
            app.set_plugin_setting_rows(Rc::new(VecModel::from(rows)).into());
            app.set_addon_config_open(true);
            settle();
            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Tab);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("codec.prefs".into(), "move-down".into())
            );
            key(&app, slint::platform::Key::Tab);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("codec.prefs".into(), "remove-choice".into())
            );
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                captured.borrow().last().unwrap(),
                &("codec.prefs".into(), "add-choice".into())
            );
            app.set_system_back_request(app.get_system_back_request() + 1);
            settle();
            assert!(
                app.get_addon_config_open(),
                "system Back closes the nested preference first"
            );
            app.set_system_back_request(app.get_system_back_request() + 1);
            settle();
            assert!(!app.get_addon_config_open());
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

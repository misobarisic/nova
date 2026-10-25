//! Universal stream rules live in Settings → Player, using ordinary Settings
//! controls and the same profile-owned bridge/scope policy as other defaults.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle(app: &nova::AppWindow) {
    let mut viewport: Option<ElementHandle> = None;
    for _ in 0..5 {
        if viewport.as_ref().is_none_or(|item| !item.is_valid()) {
            viewport = ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll").next();
        }
        if let Some(viewport) = &viewport {
            let _ = (viewport.size(), viewport.absolute_position());
        }
        // Let the factory and the picker's 80ms focus request run across
        // several frames. Target queries in reveal()/in_form() then force the
        // relevant leaf geometry, without walking every hidden setting row.
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn form(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll")
        .next()
        .expect("Settings detail viewport")
}
fn scroll_by(app: &nova::AppWindow, delta_y: f32) {
    let scroll = form(app);
    let p = scroll.absolute_position();
    let s = scroll.size();
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0),
        delta_x: 0.0,
        delta_y,
    });
}
fn in_form(app: &nova::AppWindow, control: &ElementHandle) -> bool {
    let scroll = form(app);
    let p = control.absolute_position();
    let s = control.size();
    let viewport = scroll.absolute_position();
    let bounds = scroll.size();
    let content = (**app).global::<nova_ui_shared::SettingsContent>();
    let top =
        (content.get_detail_sticky_top() + content.get_detail_header_height()).max(viewport.y);
    p.x >= viewport.x - 0.5
        && p.x + s.width <= viewport.x + bounds.width + 0.5
        && p.y >= top - 0.5
        && p.y + s.height <= viewport.y + bounds.height - 4.0
}
fn control(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .filter(|item| {
            matches!(
                item.accessible_role(),
                Some(
                    AccessibleRole::Checkbox
                        | AccessibleRole::Combobox
                        | AccessibleRole::TextInput
                        | AccessibleRole::Button
                )
            )
        })
        .last()
}
fn reveal(app: &nova::AppWindow, label: &str) -> ElementHandle {
    for attempt in 0..55 {
        if let Some(target) = control(app, label) {
            if in_form(app, &target) {
                return target;
            }
            let p = target.absolute_position();
            let viewport = form(app);
            let top = viewport.absolute_position().y + 150.0;
            app.set_settings_scroll_y((app.get_settings_scroll_y() + top - p.y).min(0.0));
        } else if attempt == 0 {
            app.set_settings_scroll_y(0.0);
        } else {
            scroll_by(app, -180.0);
        }
        settle(app);
    }
    panic!(
        "missing/revealed Settings filter control {label}, width={} scroll={}",
        app.window().size().width,
        app.get_settings_scroll_y()
    );
}
fn click(app: &nova::AppWindow, label: &str) {
    reveal(app, label).mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}
fn reveal_scope(app: &nova::AppWindow, key: &str) -> ElementHandle {
    app.set_settings_scroll_y(0.0);
    for _ in 0..100 {
        settle(app);
        let expected = format!("setting-scope:{key}");
        let badge = ElementQuery::from_root(app)
            .match_predicate(move |element| {
                element.accessible_id().as_deref() == Some(expected.as_str())
            })
            .find_first();
        if let Some(badge) = badge {
            if in_form(app, &badge) {
                return badge;
            }
            let top = form(app).absolute_position().y + 150.0;
            app.set_settings_scroll_y(
                (app.get_settings_scroll_y() + top - badge.absolute_position().y).min(0.0),
            );
        } else {
            scroll_by(app, -180.0);
        }
    }
    let visible = ElementQuery::from_root(app)
        .match_predicate(|element| {
            element
                .accessible_id()
                .is_some_and(|id| id.starts_with("setting-scope:"))
        })
        .find_all()
        .into_iter()
        .map(|element| (element.accessible_id(), element.absolute_position()))
        .collect::<Vec<_>>();
    panic!(
        "missing logical scope {key}, scroll={}, visible={visible:?}",
        app.get_settings_scroll_y()
    );
}
fn type_text(app: &nova::AppWindow, value: &str) {
    for character in value.chars() {
        let text: slint::SharedString = character.to_string().into();
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    }
    settle(app);
}
fn seed_sort(app: &nova::AppWindow) {
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_sort_order(
        Rc::new(VecModel::from(
            [
                "audio",
                "codec",
                "cached",
                "resolution",
                "size",
                "season_pack",
                "seeders",
                "subtitle",
            ]
            .map(Into::into)
            .to_vec(),
        ))
        .into(),
    );
    filters.set_sort_ascending(
        Rc::new(VecModel::from(vec![
            true, true, false, false, true, false, false, true,
        ]))
        .into(),
    );
}
fn fixture(app: &nova::AppWindow) {
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_resolution_options(
        Rc::new(VecModel::from(vec![
            240, 360, 480, 576, 720, 1080, 1440, 2160, 4320, 8640,
        ]))
        .into(),
    );
    filters.set_resolution_option_names(
        Rc::new(VecModel::from(vec![
            "240p".into(),
            "360p".into(),
            "480p".into(),
            "576p".into(),
            "720p".into(),
            "1080p".into(),
            "1440p".into(),
            "2160p".into(),
            "4320p".into(),
            "8640p".into(),
        ]))
        .into(),
    );
    filters.set_max_resolution_index(3);
    filters.set_max_size_index(4);
    filters.set_audio_languages("ja, en".into());
    filters.set_allow_hevc(true);
    filters.set_retain_unknown(true);
    seed_sort(app);
    filters.set_use_preferred_audio_languages(true);
    filters.set_preferred_audio_languages("Japanese (ja) → English (en)".into());
    filters.set_language_codes(
        Rc::new(VecModel::from(vec!["en".into(), "ja".into(), "hr".into()])).into(),
    );
    filters.set_language_names(
        Rc::new(VecModel::from(vec![
            "English (en)".into(),
            "Japanese (ja)".into(),
            "Croatian (hr)".into(),
        ]))
        .into(),
    );
    filters.set_selected_language_codes(
        Rc::new(VecModel::from(vec!["ja".into(), "en".into()])).into(),
    );
    filters.set_selected_language_names(
        Rc::new(VecModel::from(vec![
            "Japanese (ja)".into(),
            "English (en)".into(),
        ]))
        .into(),
    );
}
fn wire_edits(app: &nova::AppWindow) -> Rc<RefCell<Vec<(i32, String)>>> {
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_edited(move |field, value| {
            recorded.borrow_mut().push((field, value.to_string()));
            let app = weak.upgrade().unwrap();
            let f = app.global::<nova::StreamFiltersBridge>();
            match field {
                0 => f.set_enabled(value == "true"),
                1 => f.set_max_resolution_index(value.parse().unwrap()),
                2 => f.set_max_size_index(value.parse().unwrap()),
                3 => f.set_allow_avc(value == "true"),
                4 => f.set_allow_hevc(value == "true"),
                5 => f.set_allow_av1(value == "true"),
                6 => f.set_allow_vp9(value == "true"),
                7 => f.set_audio_languages(value),
                8 => f.set_require_audio(value == "true"),
                9 => f.set_retain_unknown(value == "true"),
                10 => f.set_allow_other(value == "true"),
                11 => f.set_block_avc(value == "true"),
                12 => f.set_block_hevc(value == "true"),
                13 => f.set_block_av1(value == "true"),
                14 => f.set_block_vp9(value == "true"),
                15 => f.set_block_other(value == "true"),
                16 => {
                    let (action, codec) = value.split_once(':').unwrap();
                    let codec = match codec {
                        "avc" => 0,
                        "hevc" => 1,
                        "av1" => 2,
                        "vp9" => 3,
                        _ => 4,
                    };
                    let mut order = f.get_codec_priority_order().iter().collect::<Vec<_>>();
                    match action {
                        "add" if !order.contains(&codec) => order.push(codec),
                        "remove" => order.retain(|item| *item != codec),
                        "up" | "down" => {
                            if let Some(index) = order.iter().position(|item| *item == codec) {
                                let target = if action == "up" {
                                    index.saturating_sub(1)
                                } else {
                                    (index + 1).min(order.len() - 1)
                                };
                                order.swap(index, target);
                            }
                        }
                        _ => {}
                    }
                    f.set_codec_priority_order(Rc::new(VecModel::from(order)).into());
                }
                17 => f.set_use_preferred_audio_languages(value == "true"),
                18 => f.set_require_preferred_codec(value == "true"),
                19 | 20 => {
                    let (action, height) = value.split_once(':').unwrap();
                    let height = height.parse::<i32>().unwrap();
                    let mut order = if field == 19 {
                        f.get_excluded_resolutions()
                    } else {
                        f.get_resolution_priority_order()
                    }
                    .iter()
                    .collect::<Vec<_>>();
                    match action {
                        "add" if !order.contains(&height) => order.push(height),
                        "remove" => order.retain(|item| *item != height),
                        "up" | "down" => {
                            if let Some(index) = order.iter().position(|item| *item == height) {
                                let target = if action == "up" {
                                    index.saturating_sub(1)
                                } else {
                                    (index + 1).min(order.len() - 1)
                                };
                                order.swap(index, target);
                            }
                        }
                        _ => {}
                    }
                    if field == 19 {
                        f.set_excluded_resolutions(Rc::new(VecModel::from(order)).into());
                    } else {
                        f.set_resolution_priority_order(Rc::new(VecModel::from(order)).into());
                    }
                }
                21 => f.set_require_preferred_resolution(value == "true"),
                _ => panic!("unexpected stream filter field"),
            }
        });
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_language_edited(move |code, action| {
            recorded.borrow_mut().push((100 + action, code.to_string()));
            let app = weak.upgrade().unwrap();
            let filters = app.global::<nova::StreamFiltersBridge>();
            let mut selected = filters
                .get_selected_language_codes()
                .iter()
                .collect::<Vec<_>>();
            match action {
                0 if !selected.contains(&code) => selected.push(code.clone()),
                1 => selected.retain(|item| item != &code),
                2 | 3 => {
                    if let Some(index) = selected.iter().position(|item| item == &code) {
                        let target = if action == 2 {
                            index.saturating_sub(1)
                        } else {
                            (index + 1).min(selected.len() - 1)
                        };
                        selected.swap(index, target);
                    }
                }
                _ => {}
            }
            let names = selected
                .iter()
                .map(|code| nova_config::playback_preferences::language_label(code).into())
                .collect::<Vec<_>>();
            filters.set_selected_language_codes(Rc::new(VecModel::from(selected)).into());
            filters.set_selected_language_names(Rc::new(VecModel::from(names)).into());
        });
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_sort_edited(move |criterion, action| {
            recorded
                .borrow_mut()
                .push((200 + action, criterion.to_string()));
            let app = weak.upgrade().unwrap();
            let filters = app.global::<nova::StreamFiltersBridge>();
            let mut order = filters.get_sort_order().iter().collect::<Vec<_>>();
            let mut ascending = filters.get_sort_ascending().iter().collect::<Vec<_>>();
            let index = order.iter().position(|item| item == &criterion).unwrap();
            if action == 2 {
                ascending[index] = !ascending[index];
            } else {
                let target = if action == 0 {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(order.len() - 1)
                };
                order.swap(index, target);
                ascending.swap(index, target);
            }
            filters.set_sort_order(Rc::new(VecModel::from(order)).into());
            filters.set_sort_ascending(Rc::new(VecModel::from(ascending)).into());
        });
    edits
}
fn select_filter_row(app: &nova::AppWindow, row: i32) {
    let settings = app.global::<nova_ui_shared::SettingsBridge>();
    let content = (**app).global::<nova_ui_shared::SettingsContent>();
    app.set_kb_active(true);
    app.global::<nova_ui_shared::SettingsStyle>()
        .set_keyboard_mode(true);
    settings.set_kb_zone(9);
    settings.invoke_focus_shell();
    content.set_kb_pr(content.invoke_kb_player_stream_filters_row() + row);
    settle(app);
}

#[test]
fn universal_filters_fit_320_and_390_phones_and_desktop_with_keyboard_and_touch() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::universal_filters_fit_320_and_390_phones_and_desktop_with_keyboard_and_touch"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    let edits = wire_edits(&app);
    let resets = Rc::new(Cell::new(0));
    let count = resets.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>().on_reset(move || {
        count.set(count.get() + 1);
        weak.upgrade()
            .unwrap()
            .global::<nova::StreamFiltersBridge>()
            .set_audio_languages("".into());
    });
    app.show().unwrap();
    for (width, height) in [(320, 740), (390, 844), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(width < 700);
        settle(&app);
        for label in [
            "Use stream rules",
            "Maximum resolution",
            "Maximum size",
            "Exclude resolution",
            "Add resolution preference",
            "Only preferred resolutions",
            "H.264 / AVC",
            "H.265 / HEVC",
            "AV1",
            "VP9",
            "Other video codecs",
            "Exclude H.264 / AVC",
            "Exclude H.265 / HEVC",
            "Exclude AV1",
            "Exclude VP9",
            "Exclude other video codecs",
            "Add codec preference",
            "Only preferred video codecs",
            "Use preferred audio languages",
            "Require one of these audio languages",
            "Keep streams with unknown details",
            "Reset",
        ] {
            let target = reveal(&app, label);
            assert!(
                target.size().height >= 44.0,
                "{width}: {label} needs a touch target"
            );
            assert!(
                in_form(&app, &target),
                "{width}: {label} overflows its Settings viewport"
            );
        }
        click(&app, "Use stream rules");
        assert_eq!(edits.borrow().last().unwrap().0, 0);
        select_filter_row(&app, 1);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(edits.borrow().last().unwrap(), &(1, "4".into()));
        click(&app, "Maximum resolution");
        ElementHandle::find_by_element_id(&app, "Dropdown::item_touch")
            .nth(2)
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap(), &(1, "2".into()));
        app.global::<nova::StreamFiltersBridge>()
            .set_max_resolution_index(3);
        let content = (*app).global::<nova_ui_shared::SettingsContent>();
        select_filter_row(
            &app,
            content.invoke_kb_stream_filter_resolution_exclude_picker(),
        );
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(19, "add:240".into()));
        click(&app, "Remove excluded resolution: 240p");
        assert_eq!(edits.borrow().last().unwrap(), &(19, "remove:240".into()));
        content.set_stream_filter_resolution_choice(5);
        select_filter_row(&app, content.invoke_kb_stream_filter_resolution_picker());
        key(&app, slint::platform::Key::Return);
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        click(&app, "Move up: 2160p");
        assert_eq!(edits.borrow().last().unwrap(), &(20, "up:2160".into()));
        assert_eq!(
            app.global::<nova::StreamFiltersBridge>()
                .get_resolution_priority_order()
                .iter()
                .collect::<Vec<_>>(),
            vec![2160, 1080]
        );
        select_filter_row(&app, content.invoke_kb_stream_filter_require_resolution());
        key(&app, slint::platform::Key::Return);
        assert!(
            app.global::<nova::StreamFiltersBridge>()
                .get_require_preferred_resolution()
        );
        click(&app, "Remove: 2160p");
        click(&app, "Remove: 1080p");
        app.global::<nova::StreamFiltersBridge>()
            .set_require_preferred_resolution(false);
        click(&app, "Exclude AV1");
        assert_eq!(edits.borrow().last().unwrap().0, 13);
        select_filter_row(
            &app,
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .invoke_kb_stream_filter_codec_picker(),
        );
        key(&app, slint::platform::Key::Return);
        select_filter_row(
            &app,
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .invoke_kb_stream_filter_codec_picker(),
        );
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        click(&app, "Move up: H.265 / HEVC");
        assert_eq!(edits.borrow().last().unwrap(), &(16, "up:hevc".into()));
        assert_eq!(
            app.global::<nova::StreamFiltersBridge>()
                .get_codec_priority_order()
                .iter()
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        let content = (*app).global::<nova_ui_shared::SettingsContent>();
        select_filter_row(&app, content.invoke_kb_stream_filter_require_codec());
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap().0, 18);
        if app
            .global::<nova::StreamFiltersBridge>()
            .get_use_preferred_audio_languages()
        {
            click(&app, "Use preferred audio languages");
        }
        select_filter_row(&app, content.invoke_kb_stream_filter_audio_picker());
        key(&app, slint::platform::Key::Return);
        let input = reveal(&app, "Search languages");
        assert!(content.get_player_language_input_focused());
        type_text(&app, "Croatian");
        assert_eq!(input.accessible_value().as_deref(), Some("Croatian"));
        key(&app, slint::platform::Key::Return);
        assert!(!content.get_player_language_input_focused());
        click(&app, "Move up: English (en)");
        assert_eq!(edits.borrow().last().unwrap(), &(102, "en".into()));
        select_filter_row(&app, content.invoke_kb_stream_filter_require());
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap().0, 8);
        click(&app, "Keep streams with unknown details");
        assert_eq!(edits.borrow().last().unwrap().0, 9);
        for (criterion, label) in [
            ("audio", "Audio language"),
            ("codec", "Video codec"),
            ("cached", "Cached availability"),
            ("resolution", "Resolution"),
            ("size", "Size"),
            ("season_pack", "Season pack"),
            ("seeders", "Reported seeders"),
            ("subtitle", "Subtitle language"),
        ] {
            let order = app.global::<nova::StreamFiltersBridge>().get_sort_order();
            let index = order
                .iter()
                .position(|item| item.as_str() == criterion)
                .unwrap();
            let label = format!(
                "Move {}: {label}",
                if index + 1 == order.row_count() {
                    "up"
                } else {
                    "down"
                }
            );
            click(&app, &label);
            let expected_action = if index + 1 == order.row_count() {
                200
            } else {
                201
            };
            assert_eq!(
                edits.borrow().last().unwrap(),
                &(expected_action, criterion.into())
            );
            let direction = reveal(
                &app,
                &format!(
                    "Sort direction: {}",
                    content.invoke_stream_filter_sort_label(criterion.into())
                ),
            );
            assert!(direction.size().height >= 44.0 && in_form(&app, &direction));
        }
        seed_sort(&app);
        select_filter_row(&app, content.invoke_kb_stream_filter_sort_start() + 1);
        content.set_stream_filter_sort_column(0);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(200, "codec".into()));
        assert_eq!(
            app.global::<nova::StreamFiltersBridge>()
                .get_sort_order()
                .row_data(0)
                .as_deref(),
            Some("codec")
        );
        content.set_stream_filter_sort_column(2);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(202, "codec".into()));
        assert_eq!(
            app.global::<nova::StreamFiltersBridge>()
                .get_sort_ascending()
                .row_data(0),
            Some(false)
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Reverse preference order")
                .next()
                .is_some()
        );
        let before = resets.get();
        select_filter_row(&app, content.invoke_kb_stream_filter_reset());
        key(&app, slint::platform::Key::Return);
        assert_eq!(resets.get(), before + 1);
        // Keep every width's fixture independent of edits above.
        // Leaving custom mode unmounts its local search field; a resize alone
        // intentionally retains that query and would append the next input.
        app.global::<nova::StreamFiltersBridge>()
            .set_use_preferred_audio_languages(true);
        app.global::<nova::StreamFiltersBridge>()
            .set_codec_priority_order(Default::default());
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .set_stream_filter_codec_choice(0);
        app.global::<nova::StreamFiltersBridge>()
            .set_selected_language_codes(
                Rc::new(VecModel::from(vec!["ja".into(), "en".into()])).into(),
            );
        app.global::<nova::StreamFiltersBridge>()
            .set_selected_language_names(
                Rc::new(VecModel::from(vec![
                    "Japanese (ja)".into(),
                    "English (en)".into(),
                ]))
                .into(),
            );
    }
}

#[test]
fn filter_scope_uses_standard_settings_comparison_and_device_override() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::filter_scope_uses_standard_settings_comparison_and_device_override"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    let keys = [
        "stream_filters_enabled",
        "stream_filters_resolution",
        "stream_filters_size",
        "stream_filters_excluded_resolutions",
        "stream_filters_resolution_priority",
        "stream_filters_require_resolution",
        "stream_filters_allowed_codecs",
        "stream_filters_excluded_codecs",
        "stream_filters_codec_priority",
        "stream_filters_require_codec",
        "stream_filters_audio",
        "stream_filters_require_audio",
        "stream_filters_retain_unknown",
        "stream_filters_sort_order",
    ];
    let rows = Rc::new(VecModel::from(
        keys.iter()
            .map(|key| nova::SettingSyncInfo {
                key: (*key).into(),
                status: 1,
                can_override: true,
                has_shared: true,
                shared_supported: true,
                local_value: "Account preference".into(),
                shared_value: "Account preference".into(),
            })
            .collect::<Vec<_>>(),
    ));
    let sync = app.global::<nova::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(rows.clone().into());
    sync.on_lookup(move |key| {
        keys.iter()
            .position(|item| *item == key.as_str())
            .unwrap_or(0) as i32
    });
    let overrides = Rc::new(RefCell::new(Vec::new()));
    let recorded = overrides.clone();
    let changed = rows.clone();
    sync.on_override_changed(move |key, enabled| {
        recorded.borrow_mut().push((key.to_string(), enabled));
        let index = keys.iter().position(|item| *item == key.as_str()).unwrap();
        let mut row = changed.row_data(index).unwrap();
        row.status = if enabled { 2 } else { 1 };
        changed.set_row_data(index, row);
    });
    app.show().unwrap();
    settle(&app);
    for key in keys {
        reveal_scope(&app, key).mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(sync.get_open());
        assert_eq!(sync.get_selected_key(), key);
        ElementHandle::find_by_accessible_label(&app, "Use device override")
            .last()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(overrides.borrow().last(), Some(&(key.into(), true)));
        assert!(!sync.get_open());
        // The longer device-override badge changes its row's layout. Reveal
        // the same stable key again rather than assuming its old viewport.
        reveal_scope(&app, key).mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        ElementHandle::find_by_accessible_label(&app, "Use synced value")
            .last()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(overrides.borrow().last(), Some(&(key.into(), false)));
    }
}

#[test]
fn settings_filter_editor_back_blurs_and_remounts_from_effective_profile_value() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::settings_filter_editor_back_blurs_and_remounts_from_effective_profile_value"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    wire_edits(&app);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    click(&app, "Use preferred audio languages");
    let content = (*app).global::<nova_ui_shared::SettingsContent>();
    select_filter_row(&app, content.invoke_kb_stream_filter_audio_picker());
    key(&app, slint::platform::Key::Return);
    let input = reveal(&app, "Search languages");
    assert!(in_form(&app, &input));
    assert!(content.get_player_language_input_focused());
    type_text(&app, "Croatian");
    key(&app, slint::platform::Key::Back);
    assert!(!content.get_player_language_input_focused());
    assert!(app.get_settings_detail_open());
    assert_eq!(app.get_settings_selected_id(), 5);
    // Factory teardown drops only the local search query; the next profile's
    // selected model is authoritative when its picker is mounted again.
    app.set_show_settings(false);
    settle(&app);
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_selected_language_codes(
        Rc::new(VecModel::from(vec!["de".into(), "fr".into()])).into(),
    );
    filters.set_selected_language_names(
        Rc::new(VecModel::from(vec![
            "German (de)".into(),
            "French (fr)".into(),
        ]))
        .into(),
    );
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    settle(&app);
    assert_eq!(
        reveal(&app, "Search languages")
            .accessible_value()
            .as_deref(),
        Some("")
    );
    assert!(reveal(&app, "Move up: French (fr)").size().height >= 44.0);
    assert!(!content.get_player_language_input_focused());
}

#[test]
#[cfg(feature = "desktop")]
#[ignore = "requires a display; optional native Settings stream-filter preview"]
fn render_universal_filter_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_universal_filter_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    fixture(&app);
    app.show().unwrap();
    let phase = Rc::new(Cell::new(0usize));
    let step = phase.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(650),
        move || {
            let app = weak.upgrade().unwrap();
            let scenes = [
                (390, 844, 0, "filters-settings-phone"),
                (390, 844, 16, "filters-settings-phone-audio"),
                (1280, 900, 0, "filters-settings-desktop"),
                (390, 844, 99, "filters-settings-phone-sort"),
                (1280, 900, 99, "filters-settings-desktop-sort"),
            ];
            if step.get().is_multiple_of(2) {
                let (w, h, row, _) = scenes[step.get() / 2];
                app.set_is_android(w < 700);
                app.window().set_size(slint::PhysicalSize::new(w, h));
                let settings = app.global::<nova_ui_shared::SettingsBridge>();
                let content = (*app).global::<nova_ui_shared::SettingsContent>();
                app.set_kb_active(true);
                app.global::<nova_ui_shared::SettingsStyle>()
                    .set_keyboard_mode(true);
                settings.set_kb_zone(9);
                app.global::<nova::StreamFiltersBridge>()
                    .set_use_preferred_audio_languages(row != 16);
                let row = if row == 99 {
                    content.set_stream_filter_sort_column(2);
                    content.invoke_kb_stream_filter_sort_start()
                } else {
                    row
                };
                content.set_kb_pr(content.invoke_kb_player_stream_filters_row() + row);
            } else {
                let (_, _, _, name) = scenes[step.get() / 2];
                let pixels = app.window().take_snapshot().unwrap();
                if let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") {
                    std::fs::create_dir_all(&dir).unwrap();
                    image::save_buffer(
                        std::path::PathBuf::from(dir).join(format!("{name}.png")),
                        pixels.as_bytes(),
                        pixels.width(),
                        pixels.height(),
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
                if step.get() + 1 == scenes.len() * 2 {
                    slint::quit_event_loop().unwrap();
                }
            }
            step.set(step.get() + 1);
        },
    );
    app.run().unwrap();
}

fn selector_fixture(app: &nova::AppWindow) {
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_selected_title("The Quiet Sea".into());
    app.set_selected_year("2026".into());
    app.set_stream_total(1);
    app.set_streams(
        Rc::new(slint::VecModel::from(vec![nova::StreamRow {
            id: "release".into(),
            title: "The.Quiet.Sea.1080p.WEB-DL.HEVC".into(),
            metadata: "Aster · 1080p".into(),
            description: "Audio: Japanese / English".into(),
            size: "4.5 GiB".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let sync = app.global::<nova_ui_shared::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(
        Rc::new(slint::VecModel::from(vec![
            nova_ui_shared::SettingSyncInfo {
                key: "stream_filters".into(),
                status: 1,
                can_override: true,
                has_shared: true,
                shared_supported: true,
                local_value: "1080p · 8 GiB · HEVC · ja, en · retain unknown".into(),
                shared_value: "1080p · 8 GiB · HEVC · ja, en · retain unknown".into(),
            },
        ]))
        .into(),
    );
    sync.on_lookup(|_| 0);
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_resolution_options(
        Rc::new(VecModel::from(vec![
            240, 360, 480, 576, 720, 1080, 1440, 2160, 4320, 8640,
        ]))
        .into(),
    );
    filters.set_resolution_option_names(
        Rc::new(VecModel::from(vec![
            "240p".into(),
            "360p".into(),
            "480p".into(),
            "576p".into(),
            "720p".into(),
            "1080p".into(),
            "1440p".into(),
            "2160p".into(),
            "4320p".into(),
            "8640p".into(),
        ]))
        .into(),
    );
    filters.set_max_resolution_index(3);
    filters.set_max_resolution_label("1080p".into());
    filters.set_max_size_index(4);
    filters.set_max_size_label("8 GiB".into());
    filters.set_allow_hevc(true);
    filters.set_audio_languages("ja, en".into());
    filters.set_retain_unknown(true);
}

#[test]
#[cfg(feature = "desktop")]
#[ignore = "requires a display; native stream selector opening timings"]
fn measure_native_stream_selector_opening() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::measure_native_stream_selector_opening"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    selector_fixture(&app);
    app.set_stream_selector_open(false);
    app.set_streams(
        Rc::new(slint::VecModel::from(
            (0..25)
                .map(|index| nova::StreamRow {
                    id: format!("timing-{index}").into(),
                    title: format!("The.Quiet.Sea.2026.1080p.WEB-DL.HEVC.Release.{index}").into(),
                    metadata: "Example addon · 1080p · HEVC".into(),
                    description: "Audio: Japanese / English\nSubtitles: English / Croatian".into(),
                    size: "4.5 GiB".into(),
                    lines: 3,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_stream_total(25);
    app.set_stream_addons(
        Rc::new(slint::VecModel::from(vec![
            "Example addon".into(),
            "Second addon".into(),
        ]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    let mut phase = 0;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let app = weak.upgrade().unwrap();
            let round = phase / 2;
            if phase % 2 == 0 {
                let start = std::time::Instant::now();
                app.set_stream_selector_open(true);
                let image = app.window().take_snapshot().unwrap();
                let elapsed = start.elapsed();
                let tree = ElementQuery::from_root(&app)
                    .match_predicate(|_| true)
                    .find_all();
                // Accessibility traversal excludes invisible components, so
                // this count is diagnostic rather than proof of lazy mounting.
                println!(
                    "STREAM_SELECTOR_OPEN size={}x{} round={} elapsed_us={} visible_elements={}",
                    image.width(),
                    image.height(),
                    round % 8,
                    elapsed.as_micros(),
                    tree.len(),
                );
            } else {
                app.set_stream_selector_open(false);
                let _ = app.window().take_snapshot().unwrap();
                if round == 7 {
                    app.window().set_size(slint::PhysicalSize::new(1280, 800));
                } else if round == 15 {
                    slint::quit_event_loop().unwrap();
                }
            }
            phase += 1;
        },
    );
    slint::run_event_loop().unwrap();
}

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
    for _ in 0..10 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
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
    ElementHandle::find_by_accessible_label(app, label).find(|item| {
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
fn fixture(app: &nova::AppWindow) {
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_max_resolution_index(3);
    filters.set_max_size_index(4);
    filters.set_audio_languages("ja, en".into());
    filters.set_allow_hevc(true);
    filters.set_retain_unknown(true);
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
                _ => panic!("unexpected stream filter field"),
            }
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
            "H.264 / AVC",
            "H.265 / HEVC",
            "AV1",
            "VP9",
            "Other video codecs",
            "Audio language codes",
            "Apply languages",
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
        // Real Dropdown options use the same bridge, including a smaller cap.
        click(&app, "Maximum resolution");
        ElementHandle::find_by_element_id(&app, "Dropdown::item_touch")
            .nth(2)
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap(), &(1, "2".into()));
        app.global::<nova::StreamFiltersBridge>()
            .set_max_resolution_index(3);
        app.global::<nova::StreamFiltersBridge>()
            .set_audio_languages("".into());
        select_filter_row(&app, 8);
        key(&app, slint::platform::Key::Return);
        let input = reveal(&app, "Audio language codes");
        assert!(
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_player_language_input_focused(),
            "keyboard Enter must focus the ordinary Settings input"
        );
        type_text(&app, "ja, en, hr");
        assert_eq!(input.accessible_value().as_deref(), Some("ja, en, hr"));
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(7, "ja, en, hr".into()));
        assert!(
            !(*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_player_language_input_focused()
        );
        select_filter_row(&app, 10);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap().0, 8);
        click(&app, "Keep streams with unknown details");
        assert_eq!(edits.borrow().last().unwrap().0, 9);
        let before = resets.get();
        select_filter_row(&app, 12);
        key(&app, slint::platform::Key::Return);
        assert_eq!(resets.get(), before + 1);
        assert_eq!(
            (*app)
                .global::<nova_ui_shared::SettingsContent>()
                .get_stream_filter_language_draft(),
            ""
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
    let rows = Rc::new(VecModel::from(vec![nova::SettingSyncInfo {
        key: "stream_filters".into(),
        status: 1,
        can_override: true,
        has_shared: true,
        shared_supported: true,
        local_value: "1080p · 8 GiB · ja, en".into(),
        shared_value: "1080p · 8 GiB · ja, en".into(),
    }]));
    let sync = app.global::<nova::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(rows.clone().into());
    sync.on_lookup(|_| 0);
    let overrides = Rc::new(RefCell::new(Vec::new()));
    let recorded = overrides.clone();
    let changed = rows.clone();
    sync.on_override_changed(move |key, enabled| {
        recorded.borrow_mut().push((key.to_string(), enabled));
        let mut row = changed.row_data(0).unwrap();
        row.status = if enabled { 2 } else { 1 };
        changed.set_row_data(0, row);
    });
    app.show().unwrap();
    settle(&app);
    click(&app, "Use stream rules"); // also proves enabling is an ordinary Settings action
    reveal(&app, "Use stream rules");
    let badge = || {
        ElementQuery::from_root(&app)
            .match_predicate(|e| {
                e.accessible_id().as_deref() == Some("setting-scope:stream_filters")
            })
            .find_first()
            .expect("stream_filters scope badge")
    };
    badge().mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(sync.get_open());
    assert_eq!(sync.get_selected_key(), "stream_filters");
    ElementHandle::find_by_accessible_label(&app, "Use device override")
        .last()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(&*overrides.borrow(), &[("stream_filters".into(), true)]);
    assert!(!sync.get_open());
    badge().mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    ElementHandle::find_by_accessible_label(&app, "Use synced value")
        .last()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(
        overrides.borrow().last(),
        Some(&("stream_filters".into(), false))
    );
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
    select_filter_row(&app, 8);
    key(&app, slint::platform::Key::Return);
    let input = reveal(&app, "Audio language codes");
    assert!(in_form(&app, &input));
    assert!(
        (*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_player_language_input_focused()
    );
    type_text(&app, ", hr");
    key(&app, slint::platform::Key::Back);
    assert!(
        !(*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_player_language_input_focused()
    );
    assert!(app.get_settings_detail_open());
    assert_eq!(app.get_settings_selected_id(), 5);
    // Closing the Settings page releases the leaf; a new profile's effective
    // mirror must seed the next editor instead of resurrecting the old draft.
    app.set_show_settings(false);
    settle(&app);
    app.global::<nova::StreamFiltersBridge>()
        .set_audio_languages("de, fr".into());
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    settle(&app);
    let input = reveal(&app, "Audio language codes");
    assert_eq!(input.accessible_value().as_deref(), Some("de, fr"));
    assert!(
        !(*app)
            .global::<nova_ui_shared::SettingsContent>()
            .get_player_language_input_focused()
    );
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
    let phase = Rc::new(Cell::new(0));
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
                (390, 844, 8, "filters-settings-phone-audio"),
                (1280, 900, 0, "filters-settings-desktop"),
            ];
            if step.get() % 2 == 0 {
                let (w, h, row, _) = scenes[step.get() / 2];
                app.set_is_android(w < 700);
                app.window().set_size(slint::PhysicalSize::new(w, h));
                let settings = app.global::<nova_ui_shared::SettingsBridge>();
                let content = (*app).global::<nova_ui_shared::SettingsContent>();
                app.set_kb_active(true);
                app.global::<nova_ui_shared::SettingsStyle>()
                    .set_keyboard_mode(true);
                settings.set_kb_zone(9);
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
                if step.get() == 5 {
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

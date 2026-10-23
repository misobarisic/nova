//! Playback preference reset rows and responsive Player settings UI (headless).

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(Duration::from_millis(ms), body);
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
}

fn has_text(app: &nova::AppWindow, expected: &str) -> bool {
    ElementHandle::find_by_element_type_name(app, "Text")
        .any(|text| text.accessible_label().as_deref() == Some(expected))
}

fn setting_scope(app: &nova::AppWindow, key: &str) -> Option<ElementHandle> {
    let id = format!("setting-scope:{key}");
    ElementQuery::from_root(app)
        .match_predicate(move |element| element.accessible_id().as_deref() == Some(id.as_str()))
        .find_all()
        .into_iter()
        .next()
}

fn title_badge_do_not_overlap(app: &nova::AppWindow, title: &str, badge: &ElementHandle) -> bool {
    let badge_position = badge.absolute_position();
    let badge_size = badge.size();
    ElementHandle::find_by_accessible_label(app, title).any(|title| {
        let position = title.absolute_position();
        let size = title.size();
        position.x + size.width <= badge_position.x - 7.5
            || position.y + size.height <= badge_position.y - 2.5
            || badge_position.y + badge_size.height <= position.y - 2.5
    })
}

fn max_visible_right_edge(app: &nova::AppWindow) -> f32 {
    let width = app.window().size().width as f32;
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|element| {
            let position = element.absolute_position();
            let size = element.size();
            (position.x >= 0.0 && position.x < width && size.width > 0.0)
                .then_some(position.x + size.width)
        })
        .fold(0.0, f32::max)
}

#[test]
fn playback_defaults_reset_and_player_settings_fit() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "playback_defaults_reset_and_player_settings_fit"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(800, 600));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_audio_preference_saved(true);
    app.set_subtitle_preference_saved(true);
    app.set_speed_preference_saved(true);
    app.set_audio_tracks(
        Rc::new(VecModel::from(vec![nova::TrackRow {
            index: 7,
            lang: "en".into(),
            title: "Main".into(),
            codec: "AAC".into(),
            selected: false,
        }]))
        .into(),
    );
    app.set_sub_tracks(
        Rc::new(VecModel::from(vec![nova::TrackRow {
            index: 42,
            lang: "en".into(),
            title: "English".into(),
            codec: "SRT".into(),
            selected: false,
        }]))
        .into(),
    );
    let scope_rows = Rc::new(VecModel::from(vec![
        nova::SettingSyncInfo {
            key: "default_audio_language".into(),
            status: 1,
            can_override: true,
            local_value: "English".into(),
            shared_value: "English".into(),
            has_shared: true,
            shared_supported: true,
        },
        nova::SettingSyncInfo {
            key: "default_subtitle_language".into(),
            status: 2,
            can_override: true,
            local_value: "Croatian".into(),
            shared_value: "English".into(),
            has_shared: true,
            shared_supported: true,
        },
    ]));
    app.global::<nova::SettingsSync>()
        .set_rows(scope_rows.clone().into());
    let audio_codes = Rc::new(VecModel::from(vec!["en".into(), "ja".into()]));
    let audio_names = Rc::new(VecModel::from(vec![
        "English (en)".into(),
        "Japanese (ja)".into(),
    ]));
    app.set_audio_language_selected_codes(audio_codes.into());
    app.set_audio_language_selected_names(audio_names.into());
    app.set_subtitle_language_selected_codes(
        Rc::new(VecModel::from(vec!["hr".into(), "en".into()])).into(),
    );
    app.set_subtitle_language_selected_names(
        Rc::new(VecModel::from(vec![
            "Croatian (hr)".into(),
            "English (en)".into(),
        ]))
        .into(),
    );
    let language_edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = language_edits.clone();
    app.on_playback_language_edited(move |code, action, subtitles| {
        recorded
            .borrow_mut()
            .push((code.to_string(), action, subtitles))
    });
    app.set_sync_enabled(true);
    let indexed_scope_rows = scope_rows.clone();
    app.global::<nova::SettingsSync>().on_lookup(move |key| {
        indexed_scope_rows
            .iter()
            .position(|row| row.key == key)
            .map(|index| index as i32)
            .unwrap_or(-1)
    });

    let restored = Rc::new(RefCell::new(Vec::<i32>::new()));
    let audio_picks = Rc::new(RefCell::new(Vec::<i32>::new()));
    let subtitle_picks = Rc::new(RefCell::new(Vec::<i32>::new()));
    let steps = Rc::new(RefCell::new(Vec::<i32>::new()));
    {
        let restored = restored.clone();
        let app = app.as_weak();
        app.upgrade()
            .unwrap()
            .on_restore_playback_default(move |control| {
                restored.borrow_mut().push(control);
                let app = app.upgrade().unwrap();
                match control {
                    0 => app.set_audio_preference_saved(false),
                    1 => app.set_subtitle_preference_saved(false),
                    2 => app.set_speed_preference_saved(false),
                    _ => {}
                }
            });
    }
    {
        let steps = steps.clone();
        app.on_playback_speed_stepped(move |direction| steps.borrow_mut().push(direction));
    }
    {
        let picks = audio_picks.clone();
        app.on_pick_audio_track(move |index| picks.borrow_mut().push(index));
    }
    {
        let picks = subtitle_picks.clone();
        app.on_pick_sub_track(move |index| picks.borrow_mut().push(index));
    }

    let app_weak = app.as_weak();
    after(350, move || {
        slint::spawn_local(async move {
            let app = app_weak.upgrade().unwrap();
            // Desktop player controls are indexed close, pause, volume, seek
            // back/forward, subtitles, audio, gear. Open the gear by keyboard.
            for _ in 0..6 {
                key(&app, slint::platform::Key::Tab);
            }
            key(&app, slint::platform::Key::Return);
            settings_support::settle().await;
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "the gear key opens the player settings panel"
            );

            // Two subtitle-tool shortcuts precede reset, Off and track rows.
            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring subtitles leaves the panel open"
            );
            assert!(has_text(&app, "Using global default"));
            // Off and the first track follow the selected reset row.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(subtitle_picks.borrow().as_slice(), [42]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                0,
                "choosing a subtitle track closes the panel"
            );

            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert!(has_text(&app, "Saved for this title"));
            // Audio reset is row 0, tracks begin at row 1.
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1, 0]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring audio leaves the panel open"
            );
            // Audio tracks begin at row 1 after the reset item.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(audio_picks.borrow().as_slice(), [7]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                0,
                "choosing an audio track closes the panel"
            );

            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert!(has_text(&app, "Saved for this title"));
            // Speed reset is row 0 and its slider is row 1. Up/Down move
            // focus; Left/Right step only while the slider has focus.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::RightArrow);
            key(&app, slint::platform::Key::LeftArrow);
            assert_eq!(steps.borrow().as_slice(), [1, -1]);
            key(&app, slint::platform::Key::UpArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1, 0, 2]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring speed leaves the panel open"
            );

            // The reset entries share MenuRow styling and touch sizing with
            // the existing player menu; the 44px minimum touch target holds.
            assert!(
                ElementHandle::find_by_element_type_name(&app, "MenuRow")
                    .any(|row| row.size().height >= 44.0)
            );

            app.set_player_open(false);
            app.set_show_settings(true);
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            slint::select_bundled_translation("en").unwrap();

            for (audio_status, subtitle_status) in [(1, 2), (2, 1)] {
                let mut audio_scope = scope_rows.row_data(0).unwrap();
                audio_scope.status = audio_status;
                scope_rows.set_row_data(0, audio_scope);
                let mut subtitle_scope = scope_rows.row_data(1).unwrap();
                subtitle_scope.status = subtitle_status;
                scope_rows.set_row_data(1, subtitle_scope);
                for (width, height) in [(360, 800), (800, 360)] {
                    app.window()
                        .set_size(slint::PhysicalSize::new(width, height));
                    let mut found_audio = false;
                    let mut found_subtitles = false;
                    let mut found_speed = false;
                    let mut checked_priority_control = false;
                    let mut found_audio_scope = false;
                    let mut found_subtitle_scope = false;
                    let mut checked_audio_caption = false;
                    let mut checked_subtitle_caption = false;
                    for scroll in [0.0, -120.0, -240.0, -360.0, -480.0, -600.0, -720.0, -840.0, -960.0, -1080.0] {
                        app.set_settings_scroll_y(scroll);
                        settings_support::settle().await;
                        found_audio |= has_text(&app, "Preferred audio languages");
                        found_subtitles |= has_text(&app, "Preferred subtitle languages");
                        found_speed |= ElementHandle::find_by_element_type_name(&app, "SpeedControl")
                            .next()
                            .is_some();
                        if !checked_priority_control
                            && let Some(button) = ElementHandle::find_by_accessible_label(&app, "Move down: English (en)").find(|button| {
                                let position = button.absolute_position();
                                position.y >= 170.0 && position.y + button.size().height <= height as f32 - 16.0
                            }) {
                            let before = language_edits.borrow().len();
                            let position = button.absolute_position();
                            let size = button.size();
                            let position = slint::LogicalPosition::new(position.x + size.width / 2.0, position.y + size.height / 2.0);
                            app.window().dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button: slint::platform::PointerEventButton::Left });
                            app.window().dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button: slint::platform::PointerEventButton::Left });
                            assert_eq!(language_edits.borrow().len(), before + 1);
                            assert_eq!(language_edits.borrow().last(), Some(&("en".into(), 3, false)));
                            checked_priority_control = true;
                        }
                        let expected_audio = if audio_status == 1 {
                            "Synced"
                        } else {
                            "This device (overridden)"
                        };
                        if let Some(badge) = setting_scope(&app, "default_audio_language") {
                            found_audio_scope = true;
                            assert_eq!(badge.accessible_label().as_deref(), Some(expected_audio));
                            // Element queries omit clipped titles: a stacked badge
                            // can remain visible after its title scrolls offscreen.
                            if has_text(&app, "Preferred audio languages") {
                                checked_audio_caption = true;
                                assert!(title_badge_do_not_overlap(
                                    &app,
                                    "Preferred audio languages",
                                    &badge
                                ), "audio title/badge overlap at {width}x{height}, status {audio_status}");
                            }
                        }
                        let expected_subtitle = if subtitle_status == 1 {
                            "Synced"
                        } else {
                            "This device (overridden)"
                        };
                        if let Some(badge) = setting_scope(&app, "default_subtitle_language") {
                            found_subtitle_scope = true;
                            assert_eq!(badge.accessible_label().as_deref(), Some(expected_subtitle));
                            if has_text(&app, "Preferred subtitle languages") {
                                checked_subtitle_caption = true;
                                assert!(title_badge_do_not_overlap(
                                    &app,
                                    "Preferred subtitle languages",
                                    &badge
                                ), "subtitle title/badge overlap at {width}x{height}, status {subtitle_status}");
                            }
                        }
                        let edge = max_visible_right_edge(&app);
                        assert!(
                            edge <= width as f32 + 1.0,
                            "Settings → Player overflows {width}x{height} with language scopes: right edge {edge:.1}"
                        );
                    }
                    assert!(checked_priority_control, "language priority control must be usable at {width}x{height}");
                    assert!(checked_audio_caption && checked_subtitle_caption,
                        "both language captions must be checked at {width}x{height}");
                    assert!(
                        found_audio,
                        "audio language control visible at {width}x{height}"
                    );
                    assert!(
                        found_subtitles,
                        "subtitle language control visible at {width}x{height}"
                    );
                    assert!(
                        found_speed,
                        "default speed control visible at {width}x{height}"
                    );
                    assert!(
                        found_audio_scope,
                        "audio language scope visible at {width}x{height}"
                    );
                    assert!(
                        found_subtitle_scope,
                        "subtitle language scope visible at {width}x{height}"
                    );
                }
            }

            slint::quit_event_loop().unwrap();
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();
}

use crate::settings_support;

#[test]
fn android_pip_setting_is_local_translated_and_keyboard_accessible() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::android_pip_setting_is_local_translated_and_keyboard_accessible"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 1400));
    app.show().unwrap();
    let settle = || {
        for _ in 0..30 {
            for item in ElementQuery::from_root(&app)
                .match_predicate(|_| true)
                .find_all()
            {
                let _ = item.size();
            }
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
        }
    };
    settle();
    assert!(!has_text(&app, "Picture in picture"));
    assert!(!has_text(&app, "Auto-rotate for playback"));
    app.set_is_android(true);
    settle();
    assert!(app.get_android_pip_enabled());
    let edits = Rc::new(RefCell::new(Vec::new()));
    let captured = edits.clone();
    app.on_settings_edited(move |field| captured.borrow_mut().push(field.to_string()));
    let toggle = || {
        ElementHandle::find_by_accessible_label(&app, "Picture in picture")
            .find(|item| {
                item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Checkbox)
            })
            .expect("Android PiP toggle")
    };
    assert_eq!(toggle().accessible_checked(), Some(true));
    toggle().mock_single_click(slint::platform::PointerEventButton::Left);
    assert!(!app.get_android_pip_enabled());
    assert!(
        edits
            .borrow()
            .iter()
            .any(|field| field == "android_pip_enabled")
    );
    // Left/Right reaches this added row without shifting the decoder or
    // return-behavior controls; Enter toggles it through the shell as well.
    key(&app, slint::platform::Key::RightArrow);
    assert!(app.get_android_pip_enabled());
    key(&app, slint::platform::Key::LeftArrow);
    assert!(!app.get_android_pip_enabled());
    key(&app, slint::platform::Key::Return);
    assert!(app.get_android_pip_enabled());
    assert!(app.get_android_auto_rotate());
    let rotation = ElementHandle::find_by_accessible_label(&app, "Auto-rotate for playback")
        .find(|item| {
            item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Checkbox)
        })
        .expect("Android auto-rotation toggle");
    assert_eq!(rotation.accessible_checked(), Some(true));
    rotation.mock_single_click(slint::platform::PointerEventButton::Left);
    assert!(!app.get_android_auto_rotate());
    assert_eq!(
        edits.borrow().last().map(String::as_str),
        Some("android_auto_rotate")
    );
    key(&app, slint::platform::Key::RightArrow);
    assert!(app.get_android_auto_rotate());
    key(&app, slint::platform::Key::LeftArrow);
    assert!(!app.get_android_auto_rotate());
    key(&app, slint::platform::Key::Return);
    assert!(app.get_android_auto_rotate());
    slint::select_bundled_translation("hr").unwrap();
    settle();
    assert!(has_text(&app, "Slika u slici"));
    assert!(has_text(&app, "Automatsko zakretanje pri reprodukciji"));
    app.set_is_android(false);
    settle();
    assert!(!has_text(&app, "Slika u slici"));
    assert!(!has_text(&app, "Automatsko zakretanje pri reprodukciji"));
}

#[test]
fn external_player_choice_is_android_only() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::external_player_choice_is_android_only"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 1000));
    app.show().unwrap();
    for android in [false, true] {
        app.set_is_android(android);
        app.set_player_backend_index(0);
        app.set_settings_scroll_y(0.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(250));
        let button = |label: &str| {
            ElementHandle::find_by_accessible_label(&app, label)
                .find(|item| {
                    item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
                })
                .unwrap_or_else(|| panic!("missing button {label}"))
        };
        assert_eq!(button("External").accessible_enabled(), Some(android));
        button("External").mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(app.get_player_backend_index(), i32::from(android));
        button("Built-in").mock_single_click(slint::platform::PointerEventButton::Left);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_player_backend_index(), i32::from(android));
    }
}

#[test]
fn desktop_decoder_default_and_session_picker_are_separate_and_keyboard_accessible() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::desktop_decoder_default_and_session_picker_are_separate_and_keyboard_accessible"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    app.set_desktop_decoder_available(true);
    app.set_desktop_hwdec_index(nova_config::DesktopHwdec::default().index());
    app.window().set_size(slint::PhysicalSize::new(1280, 1000));
    app.show().unwrap();
    let settle = || i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(250));
    settle();
    let click = |label: &str| {
        ElementHandle::find_by_accessible_label(&app, label)
            .find(|item| {
                item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
            })
            .unwrap_or_else(|| panic!("missing button {label}"))
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle();
    };
    assert_eq!(
        app.get_desktop_hwdec_index(),
        nova_config::DesktopHwdec::default().index()
    );
    assert!(has_text(&app, "Video decoder"));
    click("HW+");
    assert_eq!(app.get_desktop_hwdec_index(), 0);
    key(&app, slint::platform::Key::RightArrow);
    settle();
    assert_eq!(app.get_desktop_hwdec_index(), 1);
    assert_eq!(app.get_android_hwdec_index(), 0);

    let picks = Rc::new(RefCell::new(Vec::new()));
    let captured = picks.clone();
    app.on_pick_decoder(move |index| captured.borrow_mut().push(index));
    app.set_show_settings(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_osd_visible(true);
    app.set_decode_label("HW".into());
    app.set_decoder_selected(1);
    settle();
    let open_menu = || {
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
            .next()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle();
        click("Hardware decoding");
        assert!(has_text(
            &app,
            "Applies to this stream only. The next stream uses Settings → Player."
        ));
    };
    open_menu();
    click("SW");
    assert_eq!(*picks.borrow(), [2]);
    assert_eq!(
        app.get_desktop_hwdec_index(),
        1,
        "OSD pick must not edit the saved preference"
    );
    open_menu();
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::Return);
    settle();
    assert_eq!(*picks.borrow(), [2, 1]);
    assert_eq!(app.get_desktop_hwdec_index(), 1);

    // Windows exposes the same choices, but HW+ is disabled in both controls.
    // Exercise the mount input on every host so Linux CI covers Windows UI.
    app.set_player_open(false);
    app.set_show_settings(true);
    app.set_hw_plus_available(false);
    settle();
    let hw_plus = || {
        ElementHandle::find_by_accessible_label(&app, "HW+")
            .find(|item| {
                item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
            })
            .unwrap()
    };
    assert_eq!(hw_plus().accessible_enabled(), Some(false));
    click("HW+");
    assert_eq!(app.get_desktop_hwdec_index(), 1);
    click("HW");
    key(&app, slint::platform::Key::RightArrow);
    settle();
    assert_eq!(app.get_desktop_hwdec_index(), 2);
    key(&app, slint::platform::Key::LeftArrow);
    key(&app, slint::platform::Key::LeftArrow);
    settle();
    assert_eq!(app.get_desktop_hwdec_index(), 1);

    app.set_show_settings(false);
    app.set_player_open(true);
    app.set_osd_visible(true);
    settle();
    open_menu();
    assert_eq!(hw_plus().accessible_enabled(), Some(false));
    click("HW+");
    assert_eq!(*picks.borrow(), [2, 1]);
    key(&app, slint::platform::Key::UpArrow);
    key(&app, slint::platform::Key::Return);
    settle();
    assert_eq!(*picks.borrow(), [2, 1, 1]);
    open_menu();
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::Return);
    settle();
    assert_eq!(*picks.borrow(), [2, 1, 1, 2]);
    open_menu();
    click("HW");
    assert_eq!(*picks.borrow(), [2, 1, 1, 2, 1]);
    assert_eq!(app.get_desktop_hwdec_index(), 1);
    assert_eq!(app.get_android_hwdec_index(), 0);

    app.set_player_open(false);
    app.set_desktop_decoder_available(false);
    settle();
    app.set_player_open(true);
    app.set_osd_visible(true);
    settle();
    ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
    assert!(
        !has_text(&app, "Hardware decoding"),
        "Other desktop targets keep their existing menu"
    );
}

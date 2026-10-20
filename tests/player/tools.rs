//! Playback tools share the responsive OSD popup and its keyboard ownership.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        slint::platform::update_timers_and_animations();
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}
fn open_page(app: &nova::AppWindow, row: usize) {
    app.set_osd_visible(true);
    ElementHandle::find_by_element_id(app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
    for _ in 0..row {
        key(app, slint::platform::Key::DownArrow);
    }
    key(app, slint::platform::Key::Return);
}

#[test]
fn bookmarks_second_language_and_source_retry_work_at_phone_landscape_and_desktop_sizes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::bookmarks_second_language_and_source_retry_work_at_phone_landscape_and_desktop_sizes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, height, android) in [(390, 844, true), (900, 360, true), (1280, 800, false)] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        app.set_desktop_decoder_available(true);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_duration(600.0);
        app.set_position(80.0);
        app.set_scene_bookmarks_available(true);
        app.set_try_another_stream_available(true);
        app.set_scene_bookmarks(
            Rc::new(slint::VecModel::from(vec![nova::SceneBookmarkRow {
                id: "scene-one".into(),
                position: 45.0,
                note: "The reveal".into(),
            }]))
            .into(),
        );
        app.set_secondary_sub_tracks(
            Rc::new(slint::VecModel::from(vec![nova::TrackRow {
                index: 11,
                lang: "hr".into(),
                ..Default::default()
            }]))
            .into(),
        );
        let secondary = Rc::new(Cell::new(-2));
        let recorded = secondary.clone();
        app.on_pick_secondary_sub_track(move |id| recorded.set(id));
        let placement = Rc::new(Cell::new(-1));
        let recorded = placement.clone();
        app.on_secondary_subtitle_position_changed(move |index| recorded.set(index));
        let notes = Rc::new(RefCell::new(Vec::new()));
        let recorded = notes.clone();
        app.on_add_scene_bookmark(move |note| {
            recorded.borrow_mut().push(note.to_string());
            true
        });
        let deleted = Rc::new(RefCell::new(String::new()));
        let recorded = deleted.clone();
        app.on_delete_scene_bookmark(move |id| {
            *recorded.borrow_mut() = id.to_string();
            true
        });
        let edits = Rc::new(RefCell::new(Vec::new()));
        let recorded = edits.clone();
        app.on_edit_scene_bookmark(move |id, note| {
            recorded
                .borrow_mut()
                .push((id.to_string(), note.to_string()));
            true
        });
        let retries = Rc::new(Cell::new(0));
        let recorded = retries.clone();
        app.on_try_another_stream(move || recorded.set(recorded.get() + 1));
        app.show().unwrap();
        settle(&app);
        // Secondary subtitles are reached only from the normal subtitle picker.
        open_page(&app, 0);
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        let _ = button(&app, "Off");
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(secondary.get(), 11);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(placement.get(), 1);
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        open_page(&app, 5);
        let _ = button(&app, "The reveal");
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(&*notes.borrow(), &[String::new()]);
        for _ in 0..3 {
            key(&app, slint::platform::Key::DownArrow);
        }
        key(&app, slint::platform::Key::Return);
        assert_eq!(
            &*edits.borrow(),
            &[("scene-one".to_string(), String::new())]
        );
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(deleted.borrow().as_str(), "scene-one");
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        open_page(&app, 6);
        assert_eq!(retries.get(), 1);
    }
}

#[test]
fn bookmark_editor_keeps_note_and_stable_id_until_a_save_succeeds() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::bookmark_editor_keeps_note_and_stable_id_until_a_save_succeeds"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_scene_bookmarks_available(true);
    app.set_duration(600.0);
    app.set_scene_bookmarks(
        Rc::new(slint::VecModel::from(vec![nova::SceneBookmarkRow {
            id: "stable-scene".into(),
            position: 45.0,
            note: "Keep this note".into(),
        }]))
        .into(),
    );
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.on_edit_scene_bookmark(move |id, note| {
        recorded
            .borrow_mut()
            .push((id.to_string(), note.to_string()));
        let success = recorded.borrow().len() > 1;
        weak.upgrade().unwrap().set_scene_bookmark_notice(
            if success {
                ""
            } else {
                "Could not save your scene bookmark. Your note has been kept."
            }
            .into(),
        );
        success
    });
    let adds = Rc::new(Cell::new(0));
    let recorded = adds.clone();
    app.on_add_scene_bookmark(move |_| {
        recorded.set(recorded.get() + 1);
        false
    });
    app.show().unwrap();
    settle(&app);
    open_page(&app, 5);
    for _ in 0..3 {
        key(&app, slint::platform::Key::DownArrow);
    }
    key(&app, slint::platform::Key::Return); // Edit the saved note.
    key(&app, slint::platform::Key::Return); // Storage failure.
    assert_eq!(
        &*edits.borrow(),
        &[("stable-scene".to_string(), "Keep this note".to_string())]
    );
    let _ = button(&app, "Save note");
    assert!(!app.get_scene_bookmark_notice().is_empty());
    key(&app, slint::platform::Key::Return); // Retry keeps the same ID and draft.
    assert_eq!(edits.borrow().len(), 2);
    assert_eq!(edits.borrow()[0], edits.borrow()[1]);
    let _ = button(&app, "Bookmark this scene");
    assert!(app.get_scene_bookmark_notice().is_empty());
    assert_eq!(
        adds.get(),
        0,
        "retrying an edit must never create another bookmark"
    );
}

#[test]
#[cfg(feature = "desktop")]
#[ignore = "requires a display; optional native player settings icon preview"]
fn render_player_settings_icon_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_player_settings_icon_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    app.set_desktop_decoder_available(true);
    app.set_decode_label("HW+".into());
    app.set_decoder_selected(0);
    app.set_playback_speed(1.25);
    app.set_player_show_title("The Quiet Sea".into());
    app.set_player_episode_title("S1E3: The crossing".into());
    app.set_position(137.0);
    app.set_duration(2700.0);
    app.set_audio_tracks(
        Rc::new(slint::VecModel::from(vec![nova::TrackRow {
            index: 1,
            lang: "ja".into(),
            title: "Japanese · Original".into(),
            selected: true,
            ..Default::default()
        }]))
        .into(),
    );
    app.set_sub_tracks(
        Rc::new(slint::VecModel::from(vec![nova::TrackRow {
            index: 2,
            lang: "en".into(),
            title: "English".into(),
            selected: true,
            ..Default::default()
        }]))
        .into(),
    );
    // Explicit sample data for the real current-episode player controls.
    // The fixture does not load a movie or fabricate rendered captions.
    app.set_secondary_sub_tracks(
        Rc::new(slint::VecModel::from(vec![
            nova::TrackRow {
                index: 3,
                lang: "hr".into(),
                title: "Croatian".into(),
                codec: "SRT".into(),
                selected: true,
                ..Default::default()
            },
            nova::TrackRow {
                index: 4,
                lang: "ja".into(),
                title: "Japanese".into(),
                codec: "ASS".into(),
                ..Default::default()
            },
        ]))
        .into(),
    );
    app.set_secondary_sub_track_label("Croatian".into());
    app.set_secondary_subtitle_position_index(0);
    app.set_scene_bookmarks_available(true);
    app.set_try_another_stream_available(true);
    app.set_scene_bookmarks(
        Rc::new(slint::VecModel::from(vec![
            nova::SceneBookmarkRow {
                id: "preview-radio".into(),
                position: 42.0,
                note: "The radio goes quiet".into(),
            },
            nova::SceneBookmarkRow {
                id: "preview-island".into(),
                position: 137.0,
                note: "First sight of the island".into(),
            },
        ]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 844));
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
            app.set_osd_visible(true);
            let click = |control: ElementHandle| {
                let origin = control.absolute_position();
                let size = control.size();
                let position = slint::LogicalPosition::new(
                    origin.x + size.width / 2.0,
                    origin.y + size.height / 2.0,
                );
                app.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
                app.window().dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
            };
            let back = || {
                ElementHandle::find_by_element_id(&app, "SettingsMenu::back_touch")
                    .next()
                    .expect("submenu back control")
            };
            match step.get() {
                0 => {
                    click(
                        ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
                            .next()
                            .expect("mounted settings gear"),
                    );
                    step.set(1);
                    return;
                }
                3 | 8 => {
                    click(button(&app, "Scene bookmarks"));
                    step.set(step.get() + 1);
                    return;
                }
                4 | 9 => {
                    click(button(&app, "Edit note"));
                    step.set(step.get() + 1);
                    return;
                }
                16 | 17 => {
                    click(back());
                    step.set(if step.get() == 16 { 3 } else { 8 });
                    return;
                }
                14 | 15 => {
                    click(button(&app, "Second subtitles"));
                    step.set(if step.get() == 14 { 2 } else { 7 });
                    return;
                }
                11 => {
                    click(button(&app, "Subtitles"));
                    step.set(12);
                    return;
                }
                _ => {}
            }
            let (width, height, name) = match step.get() {
                1 => (390, 844, "player-settings-icons-phone"),
                2 => (390, 844, "player-second-subtitles-phone"),
                5 => (390, 844, "player-scene-bookmarks-phone"),
                6 => (1280, 800, "player-settings-icons-desktop"),
                7 => (1280, 800, "player-second-subtitles-desktop"),
                10 => (1280, 800, "player-scene-bookmarks-desktop"),
                12 => (1280, 800, "player-subtitles-desktop"),
                13 => (390, 844, "player-subtitles-phone"),
                _ => unreachable!(),
            };
            if matches!(step.get(), 1 | 6) {
                for label in [
                    "Subtitles",
                    "Audio",
                    "Hardware decoding",
                    "Playback speed",
                    "Intro skipping",
                    "Scene bookmarks",
                    "Try another stream",
                ] {
                    let row = button(&app, label);
                    assert!(
                        row.query_descendants()
                            .match_type_name("Image")
                            .find_all()
                            .iter()
                            .any(|icon| {
                                let size = icon.size();
                                (size.width - 24.0).abs() < 0.5 && (size.height - 24.0).abs() < 0.5
                            }),
                        "{label} must render its 24px Lucide icon"
                    );
                }
            } else if matches!(step.get(), 2 | 7) {
                for label in [
                    "hr — Croatian",
                    "ja — Japanese",
                    "Automatic (best effort)",
                    "Top",
                    "Bottom",
                ] {
                    let _ = button(&app, label);
                }
            } else if matches!(step.get(), 12 | 13) {
                for label in ["Appearance & timing", "Second subtitles", "en — English"] {
                    let _ = button(&app, label);
                }
            } else {
                for label in [
                    "Save note",
                    "The radio goes quiet",
                    "Edit note",
                    "Clear note",
                    "Delete bookmark",
                ] {
                    let _ = button(&app, label);
                }
                let note = ElementQuery::from_root(&app)
                    .match_predicate(|item| {
                        item.accessible_role() == Some(AccessibleRole::TextInput)
                    })
                    .find_first()
                    .expect("bookmark note editor");
                assert_eq!(
                    note.accessible_value().as_deref(),
                    Some("The radio goes quiet")
                );
            }
            let pixels = app.window().take_snapshot().unwrap();
            assert_eq!((pixels.width(), pixels.height()), (width, height));
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
            match step.get() {
                1 | 6 => {
                    click(button(&app, "Subtitles"));
                    step.set(if step.get() == 1 { 14 } else { 15 });
                }
                2 | 7 => {
                    click(back());
                    step.set(if step.get() == 2 { 16 } else { 17 });
                }
                5 => {
                    click(back());
                    app.set_is_android(false);
                    app.window().set_size(slint::PhysicalSize::new(1280, 800));
                    step.set(6);
                }
                10 => {
                    click(back());
                    step.set(11);
                }
                12 => {
                    app.set_is_android(true);
                    app.window().set_size(slint::PhysicalSize::new(390, 844));
                    step.set(13);
                }
                13 => {
                    slint::quit_event_loop().unwrap();
                }
                _ => unreachable!(),
            }
        },
    );
    app.run().unwrap();
}

#[test]
fn automatic_stream_recovery_is_separate_from_immediate_manual_settings_action() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::automatic_stream_recovery_is_separate_from_immediate_manual_settings_action"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.set_player_open(true);
    app.set_desktop_decoder_available(true);
    app.set_player_status("Loading stream…".into());
    app.set_try_another_stream_available(true);
    let calls = Rc::new(Cell::new(0));
    let recorded = calls.clone();
    app.on_try_another_stream(move || recorded.set(recorded.get() + 1));
    app.show().unwrap();
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Try another stream")
            .next()
            .is_none(),
        "initial loading must not immediately suggest recovery"
    );
    // The setting stays usable before the automatic wait has elapsed.
    open_page(&app, 6);
    assert_eq!(calls.get(), 1);
    app.set_automatic_stream_recovery_visible(true);
    settle(&app);
    button(&app, "Try another stream").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(calls.get(), 2);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_player_status("".into());
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Playback is taking longer than expected.")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Try another stream")
            .next()
            .is_some()
    );
    app.set_is_paused(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Try another stream")
            .next()
            .is_none(),
        "pausing must immediately hide a stalled-playback suggestion"
    );
    app.set_is_paused(false);
    app.set_automatic_stream_recovery_visible(false);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Try another stream")
            .next()
            .is_none(),
        "healthy playback removes the automatic suggestion"
    );
}

#[test]
fn saved_scene_actions_fit_beside_note_and_timestamp_and_support_horizontal_keys() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::saved_scene_actions_fit_beside_note_and_timestamp_and_support_horizontal_keys"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, height, tv) in [
        (320, 844, false),
        (390, 844, false),
        (900, 360, false),
        (1280, 800, false),
        (960, 540, true),
    ] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_is_paused(false);
        app.set_desktop_decoder_available(true);
        app.set_duration(600.0);
        app.set_scene_bookmarks_available(true);
        app.set_scene_bookmarks(
            Rc::new(slint::VecModel::from(vec![nova::SceneBookmarkRow {
                id: "scene".into(),
                position: 14.0,
                note: "impossible".into(),
            }]))
            .into(),
        );
        let cleared = Rc::new(RefCell::new(Vec::new()));
        let recorded = cleared.clone();
        app.on_edit_scene_bookmark(move |id, note| {
            recorded
                .borrow_mut()
                .push((id.to_string(), note.to_string()));
            true
        });
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.show().unwrap();
        app.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });
        settle(&app);
        open_page(&app, 5);
        key(&app, slint::platform::Key::DownArrow); // Save.
        key(&app, slint::platform::Key::DownArrow); // First saved scene.
        let note = button(&app, "impossible");
        let actions = [
            button(&app, "Edit note"),
            button(&app, "Clear note"),
            button(&app, "Delete bookmark"),
        ];
        let card = ElementHandle::find_by_element_id(&app, "SettingsMenu::card")
            .next()
            .unwrap();
        let mut right = note.absolute_position().x + note.size().width;
        for action in &actions {
            assert!(action.size().width >= 44.0 && action.size().height >= 44.0);
            assert!(
                action.absolute_position().x >= right,
                "{width}x{height}: saved-scene actions must share a row without overlapping the note"
            );
            assert!(
                (action.absolute_position().y + action.size().height / 2.0
                    - note.absolute_position().y
                    - note.size().height / 2.0)
                    .abs()
                    < 1.0
            );
            assert!(
                action
                    .query_descendants()
                    .match_type_name("Image")
                    .find_all()
                    .iter()
                    .any(|image| image.size().width == 24.0 && image.size().height == 24.0)
            );
            right = action.absolute_position().x + action.size().width;
        }
        assert!(right <= card.absolute_position().x + card.size().width - 15.0);
        assert!(
            card.absolute_position().x >= 0.0
                && card.absolute_position().x + card.size().width <= width as f32
        );
        key(&app, slint::platform::Key::RightArrow); // Edit.
        key(&app, slint::platform::Key::RightArrow); // Clear.
        key(&app, slint::platform::Key::LeftArrow); // Edit; stays in this page.
        assert!(
            ElementHandle::find_by_accessible_label(&app, "impossible")
                .next()
                .is_some()
        );
        key(&app, slint::platform::Key::RightArrow); // Clear.
        key(&app, slint::platform::Key::Return);
        assert_eq!(
            cleared.borrow().as_slice(),
            &[("scene".to_string(), String::new())]
        );
        key(&app, slint::platform::Key::Escape); // Nearest page, then the popup.
        // Back restores the root's first row. Later rows can be outside the
        // visible popup on short TV windows, so assert the visible root entry.
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Subtitles")
                .next()
                .is_some()
        );
        key(&app, slint::platform::Key::Escape);
        assert!(app.get_player_open());
    }
}

#[test]
fn subtitle_shortcuts_fit_and_return_to_the_picker_that_opened_them() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::subtitle_shortcuts_fit_and_return_to_the_picker_that_opened_them"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, height, android) in [(320, 900, true), (390, 844, true), (1280, 800, false)] {
        let app = nova::AppWindow::new().unwrap();
        app.set_animations(false);
        app.global::<nova::Anim>().set_enabled(false);
        app.set_show_home(false);
        app.set_is_android(android);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_duration(600.0);
        app.set_sub_tracks(
            Rc::new(slint::VecModel::from(vec![nova::TrackRow {
                index: 11,
                lang: "en".into(),
                title: "Full dialogue".into(),
                selected: true,
                ..Default::default()
            }]))
            .into(),
        );
        app.set_secondary_sub_tracks(
            Rc::new(slint::VecModel::from(vec![nova::TrackRow {
                index: 17,
                lang: "hr".into(),
                title: "Second language".into(),
                ..Default::default()
            }]))
            .into(),
        );
        let primary_picks = Rc::new(RefCell::new(Vec::new()));
        let recorded = primary_picks.clone();
        app.on_pick_sub_track(move |id| recorded.borrow_mut().push(id));
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.show().unwrap();
        settle(&app);
        open_page(&app, 0);
        let appearance = button(&app, "Appearance & timing");
        let secondary = button(&app, "Second subtitles");
        let first_track = button(&app, "en — Full dialogue");
        assert!((appearance.absolute_position().y - secondary.absolute_position().y).abs() < 0.5);
        assert!((appearance.size().height - secondary.size().height).abs() < 0.5);
        assert!((appearance.size().width - secondary.size().width).abs() < 0.5);
        assert!(appearance.size().height >= 44.0);
        assert!(
            (secondary.absolute_position().x
                - appearance.absolute_position().x
                - appearance.size().width
                - 8.0)
                .abs()
                < 0.5
        );
        assert!(secondary.absolute_position().x + secondary.size().width <= width as f32);
        assert!(
            first_track.absolute_position().y
                >= appearance.absolute_position().y + appearance.size().height
        );
        appearance.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Subtitle timing (seconds)")
                .next()
                .is_some()
        );
        ElementHandle::find_by_element_id(&app, "SettingsMenu::back_touch")
            .next()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        // Right reaches the second shortcut without changing subtitle timing.
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Automatic (best effort)")
                .next()
                .is_some()
        );
        key(&app, slint::platform::Key::Escape);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Appearance & timing")
                .next()
                .is_some()
        );
        assert!(
            primary_picks.borrow().is_empty(),
            "opening tools must not select a track"
        );
        // Two new shortcuts precede the unchanged global/Off/track choices.
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(primary_picks.borrow().as_slice(), &[11]);
        app.set_osd_visible(true);
        if !android {
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::subtitle_button")
                .next()
                .unwrap()
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert!(
                ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .is_some()
            );
            button(&app, "Appearance & timing")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            key(&app, slint::platform::Key::Escape);
            assert!(
                ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .is_some(),
                "Back returns to the desktop captions picker"
            );
            key(&app, slint::platform::Key::RightArrow);
            key(&app, slint::platform::Key::Return);
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Automatic (best effort)")
                    .next()
                    .is_some()
            );
            key(&app, slint::platform::Key::Escape);
            assert!(
                ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .is_some()
            );
        }
    }
}

#[test]
fn direct_subtitle_tools_animate_forward_and_back_and_respect_reduced_motion() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::direct_subtitle_tools_animate_forward_and_back_and_respect_reduced_motion"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 800));
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_osd_visible(true);
    app.show().unwrap();
    settle(&app);
    let element = |id: &str| ElementHandle::find_by_element_id(&app, id).next().unwrap();
    element("PlayerOverlay::settings_gear")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    for label in ["Subtitle appearance and timing", "Second subtitles"] {
        assert!(
            ElementHandle::find_by_accessible_label(&app, label)
                .next()
                .is_none(),
            "duplicate root row: {label}"
        );
    }
    key(&app, slint::platform::Key::Escape);
    for motion in [true, false] {
        app.set_animations(motion);
        app.global::<nova::Anim>().set_enabled(motion);
        app.global::<nova::Anim>().set_player(motion);
        element("PlayerOverlay::subtitle_button")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        for label in ["Appearance & timing", "Second subtitles"] {
            button(&app, label).mock_single_click(slint::platform::PointerEventButton::Left);
            let incoming_x = element("SettingsMenu::card_col").absolute_position().x
                - element("SettingsMenu::card").absolute_position().x;
            if motion {
                assert!(
                    incoming_x > 20.0,
                    "direct tool must enter from the right: {incoming_x}"
                );
            } else {
                assert!(incoming_x.abs() < 0.5);
            }
            assert!(
                ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .is_none(),
                "only the incoming menu may own input"
            );
            settle(&app);
            settle(&app);
            let settled_x = element("SettingsMenu::card_col").absolute_position().x
                - element("SettingsMenu::card").absolute_position().x;
            assert!(settled_x.abs() < 0.5);
            element("SettingsMenu::back_touch")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            let returning_x = element("TrackMenu::menu_scroll").absolute_position().x
                - ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .unwrap()
                    .absolute_position()
                    .x;
            if motion {
                assert!(
                    returning_x < -20.0,
                    "Back must reveal the captions picker from the left: {returning_x}"
                );
            } else {
                assert!(returning_x.abs() < 0.5);
            }
            assert!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
                    .next()
                    .is_none()
            );
            settle(&app);
            settle(&app);
            let settled_x = element("TrackMenu::menu_scroll").absolute_position().x
                - ElementHandle::find_by_element_type_name(&app, "TrackMenu")
                    .next()
                    .unwrap()
                    .absolute_position()
                    .x;
            assert!(settled_x.abs() < 0.5);
        }
        key(&app, slint::platform::Key::Escape);
    }
}

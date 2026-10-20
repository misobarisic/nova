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
        // The first six categories keep their established indices.
        open_page(&app, 6);
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
        open_page(&app, 7);
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
        open_page(&app, 8);
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
    open_page(&app, 7);
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
                _ => {}
            }
            let (width, height, name) = match step.get() {
                1 => (390, 844, "player-settings-icons-phone"),
                2 => (390, 844, "player-second-subtitles-phone"),
                5 => (390, 844, "player-scene-bookmarks-phone"),
                6 => (1280, 800, "player-settings-icons-desktop"),
                7 => (1280, 800, "player-second-subtitles-desktop"),
                10 => (1280, 800, "player-scene-bookmarks-desktop"),
                _ => unreachable!(),
            };
            if matches!(step.get(), 1 | 6) {
                for label in [
                    "Subtitles",
                    "Audio",
                    "Hardware decoding",
                    "Playback speed",
                    "Subtitle appearance and timing",
                    "Intro skipping",
                    "Second subtitles",
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
                    click(button(&app, "Second subtitles"));
                    step.set(step.get() + 1);
                }
                2 | 7 => {
                    click(back());
                    step.set(step.get() + 1);
                }
                5 => {
                    click(back());
                    app.set_is_android(false);
                    app.window().set_size(slint::PhysicalSize::new(1280, 800));
                    step.set(6);
                }
                10 => {
                    slint::quit_event_loop().unwrap();
                }
                _ => unreachable!(),
            }
        },
    );
    app.run().unwrap();
}

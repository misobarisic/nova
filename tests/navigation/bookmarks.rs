//! The bookmark archive retains stable scene actions and nested Back behavior.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_ui_shared::{BookmarkSceneRow, BookmarkTitleRow, BookmarksBridge};
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova_ui::AppWindow) {
    for _ in 0..12 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing bookmark action {label}"))
}

#[test]
fn archive_preserves_scene_identity_clear_note_and_nested_back_at_phone_widths() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::archive_preserves_scene_identity_clear_note_and_nested_back_at_phone_widths"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let global = app.global::<BookmarksBridge>();
    global.set_open(true);
    global.set_titles(
        Rc::new(VecModel::from(vec![BookmarkTitleRow {
            id: "typed-title".into(),
            title: "A saved show".into(),
            count: "2 bookmarks".into(),
            context: "Series".into(),
            added: "Added date unknown".into(),
            ..Default::default()
        }]))
        .into(),
    );
    global.set_scenes(
        Rc::new(VecModel::from(vec![BookmarkSceneRow {
            id: "stable-scene-42".into(),
            title: "Episode 1".into(),
            position: "0:00:42".into(),
            note: "A scene worth revisiting".into(),
            added: "Added date unknown".into(),
        }]))
        .into(),
    );
    let weak = app.as_weak();
    global.on_select_title(move |id| {
        let app = weak.upgrade().unwrap();
        app.global::<BookmarksBridge>().set_title_id(id);
        app.global::<BookmarksBridge>()
            .set_title_caption("A saved show".into());
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    global.on_edit(move |id, note| {
        recorded
            .borrow_mut()
            .push((id.to_string(), note.to_string()));
        true
    });
    let plays = Rc::new(RefCell::new(Vec::new()));
    let recorded = plays.clone();
    global.on_play(move |id| recorded.borrow_mut().push(id.to_string()));
    app.window().set_size(slint::PhysicalSize::new(390, 1000));
    app.show().unwrap();
    settle(&app);
    button(&app, "A saved show · 2 bookmarks")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(global.get_title_id(), "typed-title");
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        settle(&app);
        for label in ["Play scene", "Edit note", "Delete"] {
            let control = button(&app, label);
            let pos = control.absolute_position();
            assert!(pos.x >= 0.0 && pos.x + control.size().width <= width as f32 + 0.5);
            assert!(control.size().height >= 44.0);
        }
    }
    button(&app, "Play scene").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(*plays.borrow(), ["stable-scene-42"]);
    button(&app, "Edit note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    button(&app, "Clear note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(
        *edits.borrow(),
        [("stable-scene-42".to_owned(), String::new())]
    );
    button(&app, "Edit note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert_eq!(
        global.get_title_id(),
        "typed-title",
        "Back closes the note editor first"
    );
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(global.get_title_id().is_empty());
    assert!(global.get_open());
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!global.get_open());
    assert!(app.get_more_open());
    assert!(!app.get_show_home());
}

#[test]
#[ignore = "requires a display; optional native design preview"]
fn render_bookmarks_previews() {
    if !crate::isolation::run(concat!(module_path!(), "::render_bookmarks_previews")) {
        return;
    }
    let app = nova_ui::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let global = app.global::<BookmarksBridge>();
    let artwork = |color: [u8; 3]| {
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(120, 180);
        for (index, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
            let fraction = (index / 120) as f32 / 180.0;
            *pixel = slint::Rgba8Pixel {
                r: (color[0] as f32 * (1.0 - fraction * 0.65)) as u8,
                g: (color[1] as f32 * (1.0 - fraction * 0.65)) as u8,
                b: (color[2] as f32 * (1.0 - fraction * 0.65)) as u8,
                a: 255,
            };
        }
        slint::Image::from_rgba8(pixels)
    };
    global.set_titles(
        Rc::new(VecModel::from(vec![
            BookmarkTitleRow {
                id: "sea".into(),
                title: "The Quiet Sea".into(),
                context: "Series".into(),
                added: "2026-10-20 19:42 UTC".into(),
                count: "3 bookmarks".into(),
                poster: artwork([43, 112, 151]),
                has_poster: true,
            },
            BookmarkTitleRow {
                id: "city".into(),
                title: "City of Amber".into(),
                context: "Movie".into(),
                added: "2026-10-18 21:16 UTC".into(),
                count: "2 bookmarks".into(),
                poster: artwork([191, 124, 51]),
                has_poster: true,
            },
            BookmarkTitleRow {
                id: "orchard".into(),
                title: "The Last Orchard".into(),
                context: "Movie".into(),
                added: "Added date unknown".into(),
                count: "1 bookmark".into(),
                poster: artwork([87, 127, 83]),
                has_poster: true,
            },
        ]))
        .into(),
    );
    global.set_scenes(
        Rc::new(VecModel::from(vec![
            BookmarkSceneRow {
                id: "scene-1".into(),
                title: "S2 E4 · A signal in the dark".into(),
                position: "0:18:42".into(),
                note: "The lighthouse conversation — a beautiful quiet moment.".into(),
                added: "2026-10-20 19:42 UTC".into(),
            },
            BookmarkSceneRow {
                id: "scene-2".into(),
                title: "S2 E4 · A signal in the dark".into(),
                position: "0:36:08".into(),
                note: "Look for the reflection in the window.".into(),
                added: "2026-10-19 20:11 UTC".into(),
            },
            BookmarkSceneRow {
                id: "scene-3".into(),
                title: "S1 E8 · The crossing".into(),
                position: "0:42:03".into(),
                note: String::new().into(),
                added: "Added date unknown".into(),
            },
        ]))
        .into(),
    );
    let weak = app.as_weak();
    global.on_select_title(move |id| {
        let app = weak.upgrade().unwrap();
        app.global::<BookmarksBridge>().set_title_id(id);
        app.global::<BookmarksBridge>()
            .set_title_caption("The Quiet Sea".into());
    });
    global.set_open(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let phase = Rc::new(std::cell::Cell::new(0usize));
    let step = phase.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let names = [
                "bookmarks-titles-desktop",
                "bookmarks-scenes-desktop",
                "bookmarks-note-desktop",
                "bookmarks-titles-phone",
                "bookmarks-scenes-phone",
                "bookmarks-note-phone",
                "bookmarks-note-error-phone",
            ];
            let pixels = app.window().take_snapshot().unwrap();
            if let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    std::path::PathBuf::from(dir).join(format!("{}.png", names[step.get()])),
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            step.set(step.get() + 1);
            match step.get() {
                1 | 4 => {
                    app.global::<BookmarksBridge>()
                        .set_title_caption("The Quiet Sea".into());
                    app.global::<BookmarksBridge>().set_title_id("sea".into());
                }
                2 | 5 => {
                    let control = button(&app, "Edit note");
                    let origin = control.absolute_position();
                    let size = control.size();
                    let position = slint::LogicalPosition::new(
                        origin.x + size.width / 2.0,
                        origin.y + size.height / 2.0,
                    );
                    // Mock clicks advance testing timers; dispatch real input
                    // here because this fixture already runs inside a timer.
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                }
                3 => {
                    app.global::<BookmarksBridge>().set_title_id("".into());
                    app.window().set_size(slint::PhysicalSize::new(390, 844));
                }
                6 => {
                    app.global::<BookmarksBridge>()
                        .set_notice("Bookmarks could not be saved. Please try again.".into());
                }
                _ => {
                    slint::quit_event_loop().unwrap();
                }
            }
        },
    );
    app.run().unwrap();
}

#[test]
fn failed_note_save_and_clear_preserve_editor_draft_until_success() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::failed_note_save_and_clear_preserve_editor_draft_until_success"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let global = app.global::<BookmarksBridge>();
    global.set_open(true);
    global.set_title_id("selected-show".into());
    global.set_title_caption("Saved show".into());
    global.set_scenes(
        Rc::new(VecModel::from(vec![BookmarkSceneRow {
            id: "stable-scene".into(),
            title: "Episode 2".into(),
            position: "0:05:20".into(),
            note: "Saved note".into(),
            added: "Added date unknown".into(),
        }]))
        .into(),
    );
    let successful = Rc::new(std::cell::Cell::new(false));
    let allow = successful.clone();
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    global.on_edit(move |id, note| {
        recorded
            .borrow_mut()
            .push((id.to_string(), note.to_string()));
        let app = weak.upgrade().unwrap();
        app.global::<BookmarksBridge>().set_notice(if allow.get() {
            "".into()
        } else {
            "Bookmarks could not be saved. Please try again.".into()
        });
        allow.get()
    });
    app.window().set_size(slint::PhysicalSize::new(390, 1100));
    app.show().unwrap();
    settle(&app);
    button(&app, "Edit note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let text: slint::SharedString = " + unsaved draft".into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(&app);
    let input = || {
        ElementQuery::from_root(&app)
            .match_predicate(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .find_first()
            .expect("note editor remains mounted")
    };
    let draft = input().accessible_value().unwrap();
    assert!(
        draft.contains("unsaved draft"),
        "fixture must enter an unsaved draft"
    );
    button(&app, "Save note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(input().accessible_value().as_deref(), Some(draft.as_str()));
    assert!(!global.get_notice().is_empty());
    assert_eq!(
        edits.borrow().last().unwrap(),
        &("stable-scene".into(), draft.to_string())
    );
    button(&app, "Clear note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(input().accessible_value().as_deref(), Some(draft.as_str()));
    assert_eq!(
        edits.borrow().last().unwrap(),
        &("stable-scene".into(), String::new())
    );
    successful.set(true);
    button(&app, "Clear note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(global.get_notice().is_empty());
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Edit note")
            .next()
            .is_some(),
        "successful retry closes editor"
    );
}

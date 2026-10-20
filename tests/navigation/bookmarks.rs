//! The bookmark archive retains stable scene actions and nested Back behavior.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_ui_shared::{BookmarkSceneRow, BookmarkTitleRow, BookmarksBridge};
use slint::{ComponentHandle, Model, VecModel};
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
// Standalone UI fixtures do not run Bridge::apply_animations,
// which publishes persisted switches into the animation global in production.
fn set_motion(app: &nova_ui::AppWindow, enabled: bool, transitions: bool) {
    app.set_animations(enabled);
    app.set_anim_transitions(transitions);
    app.global::<nova_ui::Anim>().set_enabled(enabled);
    app.global::<nova_ui::Anim>().set_transitions(transitions);
}

fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing bookmark action {label}"))
}

fn pointer_click(app: &nova_ui::AppWindow, element: &ElementHandle) {
    let origin = element.absolute_position();
    let size = element.size();
    let position =
        slint::LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
    for pressed in [true, false] {
        app.window().dispatch_event(if pressed {
            slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            }
        } else {
            slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            }
        });
    }
}

fn assert_compact_actions(
    app: &nova_ui::AppWindow,
    first: &ElementHandle,
    second: &ElementHandle,
    third: &ElementHandle,
    wrapped: bool,
) {
    let card = ElementHandle::find_by_element_id(app, "BookmarksPage::scene_card")
        .next()
        .expect("scene card");
    let left = card.absolute_position().x + 16.0;
    let right = card.absolute_position().x + card.size().width - 16.0;
    assert!(
        (first.absolute_position().x - left).abs() < 0.5,
        "first action starts at card padding"
    );
    let mut previous = first;
    let mut row_y = first.absolute_position().y;
    let mut row_height = first.size().height;
    for action in [first, second, third] {
        let position = action.absolute_position();
        let size = action.size();
        assert!(size.width >= 44.0 && size.height >= 44.0);
        if card.size().width > 700.0 {
            assert!(
                size.width < 300.0,
                "desktop actions keep their natural label widths"
            );
        }
        assert!(
            position.x >= left - 0.5 && position.x + size.width <= right + 0.5,
            "labeled action fits card padding: {position:?} {size:?}, bounds={left}..{right}"
        );
        if !std::ptr::eq(action, first) {
            if (position.y - row_y).abs() < 0.5 {
                let gap = position.x - previous.absolute_position().x - previous.size().width;
                assert!((gap - 8.0).abs() < 0.5, "inline action gap: {gap}");
                row_height = row_height.max(size.height);
            } else {
                assert!(
                    (position.x - left).abs() < 0.5,
                    "wrapped row stays left aligned"
                );
                let gap = position.y - row_y - row_height;
                assert!((gap - 8.0).abs() < 0.5, "wrapped row gap: {gap}");
                row_y = position.y;
                row_height = size.height;
            }
        }
        let label = action.accessible_label().expect("action accessible label");
        let visible_label = ElementHandle::find_by_element_id(app, "SceneAction::action_label")
            .find(|text| {
                text.accessible_label().as_deref() == Some(label.as_str())
                    && text.absolute_position().x >= position.x
                    && text.absolute_position().x < position.x + size.width
                    && text.absolute_position().y >= position.y
                    && text.absolute_position().y < position.y + size.height
            })
            .unwrap_or_else(|| panic!("visible label for action {label}"));
        assert!(visible_label.size().width > 1.0 && visible_label.size().height > 1.0);
        assert!(
            visible_label.absolute_position().x + visible_label.size().width
                <= position.x + size.width - 13.5
        );
        previous = action;
    }
    assert_eq!(
        third.absolute_position().y > first.absolute_position().y + 0.5,
        wrapped
    );
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
    set_motion(&app, false, false);
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
    for width in [320, 390, 1280, 1820] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        settle(&app);
        for label in ["Play scene", "Edit note", "Delete"] {
            let control = button(&app, label);
            let pos = control.absolute_position();
            assert!(pos.x >= 0.0 && pos.x + control.size().width <= width as f32 + 0.5);
            assert!(control.size().height >= 44.0);
        }
        let play = button(&app, "Play scene");
        let edit = button(&app, "Edit note");
        let delete = button(&app, "Delete");
        assert_compact_actions(&app, &play, &edit, &delete, width < 700);
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
    set_motion(&app, false, false);
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
                "bookmarks-scenes-wide-desktop",
                "bookmarks-scenes-small-phone",
                "bookmarks-note-small-phone",
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
                2 | 5 | 9 => {
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
                7 => {
                    app.set_system_back_request(app.get_system_back_request() + 1);
                    app.global::<BookmarksBridge>().set_notice("".into());
                    app.window().set_size(slint::PhysicalSize::new(1820, 1000));
                }
                8 => {
                    app.window().set_size(slint::PhysicalSize::new(320, 1000));
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
    set_motion(&app, false, false);
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
        ElementHandle::find_by_accessible_label(&app, "Bookmark note")
            .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .expect("note editor remains mounted")
    };
    let draft = input().accessible_value().unwrap();
    assert!(
        draft.contains("unsaved draft"),
        "fixture must enter an unsaved draft"
    );
    for width in [320, 390, 1280, 1820] {
        app.window().set_size(slint::PhysicalSize::new(width, 1100));
        settle(&app);
        let save = button(&app, "Save note");
        let clear = button(&app, "Clear note");
        let cancel = button(&app, "Cancel");
        assert_compact_actions(&app, &save, &clear, &cancel, width < 700);
    }
    app.window().set_size(slint::PhysicalSize::new(1280, 1100));
    app.global::<nova_ui_shared::InterfaceStyle>().set_tv(true);
    settle(&app);
    for label in ["Save note", "Clear note", "Cancel"] {
        assert!(button(&app, label).size().width >= 56.0);
        assert!(button(&app, label).size().height >= 56.0);
    }
    app.global::<nova_ui_shared::InterfaceStyle>().set_tv(false);
    // Below supported phone widths, labels can wrap onto their own rows
    // without spreading the controls or reducing the touch targets.
    app.window().set_size(slint::PhysicalSize::new(180, 1800));
    settle(&app);
    assert_compact_actions(
        &app,
        &button(&app, "Save note"),
        &button(&app, "Clear note"),
        &button(&app, "Cancel"),
        true,
    );
    app.window().set_size(slint::PhysicalSize::new(390, 1100));
    settle(&app);
    assert_eq!(input().accessible_value().as_deref(), Some(draft.as_str()));
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
    let attempts = edits.borrow().len();
    button(&app, "Cancel").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(
        edits.borrow().len(),
        attempts,
        "Cancel never persists a note"
    );
    button(&app, "Edit note").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
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

#[test]
fn archive_action_row_wraps_without_clipping_and_failed_delete_keeps_scene_identity() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::archive_action_row_wraps_without_clipping_and_failed_delete_keeps_scene_identity"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    set_motion(&app, false, false);
    app.set_show_home(false);
    app.set_more_open(true);
    let bridge = app.global::<BookmarksBridge>();
    bridge.set_open(true);
    bridge.set_title_id("saved-title".into());
    bridge.set_title_caption("Saved show".into());
    bridge.set_scenes(
        Rc::new(VecModel::from(
            vec!["first-scene", "second-scene"]
                .into_iter()
                .map(|id| BookmarkSceneRow {
                    id: id.into(),
                    title: "Episode 1".into(),
                    position: "0:02:42".into(),
                    note: "Saved note".into(),
                    added: "Added date unknown".into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let deletes = Rc::new(RefCell::new(Vec::new()));
    let recorded = deletes.clone();
    let weak = app.as_weak();
    bridge.on_remove(move |id| {
        recorded.borrow_mut().push(id.to_string());
        weak.upgrade()
            .unwrap()
            .global::<BookmarksBridge>()
            .set_notice("Bookmarks could not be saved. Please try again.".into());
    });
    app.window().set_size(slint::PhysicalSize::new(280, 1000));
    app.show().unwrap();
    settle(&app);
    // A narrow phone and longer translated labels deliberately exceed one
    // row. Every action must keep its text and wrap without clipping.
    slint::select_bundled_translation("hr").unwrap();
    settle(&app);
    let play = button(&app, "Pokrenite scenu");
    let edit = button(&app, "Uredite bilješku");
    let delete = button(&app, "Izbrišite");
    assert!(edit.absolute_position().y >= play.absolute_position().y + play.size().height + 7.5);
    assert_compact_actions(&app, &play, &edit, &delete, true);
    for item in [&play, &edit, &delete] {
        let p = item.absolute_position();
        let s = item.size();
        assert!(
            p.x >= 0.0 && p.x + s.width <= 280.5,
            "wrapped action must fit: {p:?} {s:?}"
        );
        assert!(s.width >= 44.0 && s.height >= 44.0);
    }
    // TV mode reserves space for its navigation rail, so exercise its larger
    // targets at an actual desktop TV width rather than an artificial phone.
    app.window().set_size(slint::PhysicalSize::new(1280, 1000));
    app.global::<nova_ui_shared::InterfaceStyle>().set_tv(true);
    settle(&app);
    let play = button(&app, "Pokrenite scenu");
    let edit = button(&app, "Uredite bilješku");
    let delete = button(&app, "Izbrišite");
    assert!((play.absolute_position().y - edit.absolute_position().y).abs() < 0.5);
    assert!((edit.absolute_position().y - delete.absolute_position().y).abs() < 0.5);
    assert_compact_actions(&app, &play, &edit, &delete, false);
    for item in [&play, &edit, &delete] {
        let p = item.absolute_position();
        let s = item.size();
        assert!(p.x >= 0.0 && p.x + s.width <= 1280.5);
        assert!(s.width >= 56.0 && s.height >= 56.0);
    }
    app.global::<nova_ui_shared::InterfaceStyle>().set_tv(false);
    slint::select_bundled_translation("en").unwrap();
    app.window().set_size(slint::PhysicalSize::new(320, 1000));
    settle(&app);
    ElementHandle::find_by_accessible_label(&app, "Delete")
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .nth(1)
        .expect("second scene Delete")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(&*deletes.borrow(), &["second-scene"]);
    assert!(!bridge.get_notice().is_empty());
    assert_eq!(
        bridge.get_scenes().row_count(),
        2,
        "failed persistence keeps both scene rows"
    );
    let key: slint::SharedString = slint::platform::Key::Delete.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressRepeated { text: key.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
    settle(&app);
    assert_eq!(
        &*deletes.borrow(),
        &["second-scene", "second-scene"],
        "keyboard retry stays on the failed scene and held Delete runs once"
    );
    assert_eq!(bridge.get_title_id(), "saved-title");
}

#[test]
fn title_scene_motion_retains_scroll_blocks_pop_input_and_cancels_stale_returns() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::title_scene_motion_retains_scroll_blocks_pop_input_and_cancels_stale_returns"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    set_motion(&app, false, false);
    app.set_show_home(false);
    app.set_more_open(true);
    let bridge = app.global::<BookmarksBridge>();
    bridge.set_open(true);
    bridge.set_query("Saved".into());
    bridge.set_sort(1);
    bridge.set_titles(
        Rc::new(VecModel::from(
            (0..16)
                .map(|index| BookmarkTitleRow {
                    id: format!("saved-{index}").into(),
                    title: format!("Saved title {index}").into(),
                    count: "1 bookmark".into(),
                    context: "Series".into(),
                    added: "Added date unknown".into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    bridge.set_scenes(
        Rc::new(VecModel::from(vec![BookmarkSceneRow {
            id: "stable-scene".into(),
            title: "Episode 1".into(),
            position: "0:02:42".into(),
            note: "Saved note".into(),
            added: "Added date unknown".into(),
        }]))
        .into(),
    );
    let selections = Rc::new(RefCell::new(Vec::new()));
    let recorded = selections.clone();
    let weak = app.as_weak();
    bridge.on_select_title(move |id| {
        recorded.borrow_mut().push(id.to_string());
        let app = weak.upgrade().unwrap();
        app.global::<BookmarksBridge>().set_title_id(id);
        app.global::<BookmarksBridge>()
            .set_title_caption("Saved show".into());
    });
    let plays = Rc::new(std::cell::Cell::new(0));
    let recorded = plays.clone();
    bridge.on_play(move |_| recorded.set(recorded.get() + 1));
    app.show().unwrap();
    for width in [390, 1280] {
        let returns_before = selections
            .borrow()
            .iter()
            .filter(|id| id.is_empty())
            .count();
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        settle(&app);
        let list = ElementHandle::find_by_element_id(&app, "BookmarksPage::title_scroll")
            .next()
            .unwrap();
        let viewport = list.absolute_position();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(
                    viewport.x + list.size().width / 2.0,
                    viewport.y + 350.0,
                ),
                delta_x: 0.0,
                delta_y: -320.0,
            });
        settle(&app);
        let title = ElementHandle::find_by_element_id(&app, "BookmarksPage::title_card")
            .find(|card| {
                let y = card.absolute_position().y;
                y > viewport.y + 180.0 && y + card.size().height < viewport.y + list.size().height
            })
            .expect("a fully visible title after scrolling");
        let label = title.accessible_label().unwrap();
        let title_y = title.absolute_position().y;
        set_motion(&app, true, true);
        pointer_click(&app, &title);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let layer = ElementHandle::find_by_element_id(&app, "BookmarksPage::scene_layer")
            .next()
            .unwrap();
        let _ = layer.absolute_position();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
        let offset = layer.absolute_position().x - (viewport.x - 18.0);
        assert!(
            offset > 0.5 && offset < width as f32 * 0.1,
            "scene entrance slides: {offset}"
        );
        settle(&app);
        settle(&app);
        pointer_click(&app, &button(&app, "Edit note"));
        settle(&app);
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle(&app);
        assert!(
            !bridge.get_title_id().is_empty(),
            "editor Back stays in scenes"
        );
        let play = button(&app, "Play scene");
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let _ = layer.absolute_position();
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
        assert!(
            !bridge.get_title_id().is_empty(),
            "outgoing scenes remain until pop ends"
        );
        let offset = layer.absolute_position().x - (viewport.x - 18.0);
        assert!(offset > 0.5, "scene exit slides toward the right");
        pointer_click(&app, &play);
        assert_eq!(plays.get(), 0, "returning scene actions cannot activate");
        settle(&app);
        settle(&app);
        assert!(bridge.get_title_id().is_empty());
        assert!(
            bridge.get_open(),
            "duplicate Back does not close the archive"
        );
        assert_eq!(
            selections
                .borrow()
                .iter()
                .filter(|id| id.is_empty())
                .count(),
            returns_before + 1
        );
        let returned = button(&app, label.as_str());
        assert!(
            (returned.absolute_position().y - title_y).abs() < 0.5,
            "title scroll survives return"
        );
        assert_eq!(bridge.get_query(), "Saved");
        assert_eq!(bridge.get_sort(), 1);

        // A different title/profile arriving during the pop invalidates its
        // delayed callback rather than clearing the new selection.
        pointer_click(&app, &returned);
        settle(&app);
        settle(&app);
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        bridge.set_title_id("replacement-title".into());
        settle(&app);
        settle(&app);
        assert_eq!(bridge.get_title_id(), "replacement-title");
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        app.set_anim_transitions(false);
        app.global::<nova_ui::Anim>().set_transitions(false);
        settle(&app);
        assert!(
            bridge.get_title_id().is_empty(),
            "disabling motion finishes an active return"
        );
        pointer_click(&app, &button(&app, label.as_str()));
        settle(&app);
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle(&app);
        assert!(
            bridge.get_title_id().is_empty(),
            "reduced-motion Back is immediate"
        );
        app.set_animations(false);
        app.global::<nova_ui::Anim>().set_enabled(false);
        pointer_click(&app, &button(&app, label.as_str()));
        settle(&app);
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle(&app);
        assert!(
            bridge.get_title_id().is_empty(),
            "master motion switch also disables return delay"
        );
        set_motion(&app, true, true);
        pointer_click(&app, &button(&app, label.as_str()));
        settle(&app);
        settle(&app);
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        bridge.set_open(false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        bridge.set_title_id("new-profile-title".into());
        bridge.set_open(true);
        settle(&app);
        settle(&app);
        assert_eq!(
            bridge.get_title_id(),
            "new-profile-title",
            "teardown cancels a pending pop before another profile reopens"
        );
        set_motion(&app, false, false);
        bridge.set_title_id("".into());
        settle(&app);
    }
}

#[test]
fn scene_keyboard_selection_reveals_the_whole_card_below_its_header() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::scene_keyboard_selection_reveals_the_whole_card_below_its_header"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    set_motion(&app, false, false);
    app.set_show_home(false);
    app.set_more_open(true);
    let bridge = app.global::<BookmarksBridge>();
    bridge.set_open(true);
    bridge.set_title_id("saved-show".into());
    bridge.set_title_caption("Saved show".into());
    bridge.set_scenes(
        Rc::new(VecModel::from(
            (1..=10)
                .map(|number| BookmarkSceneRow {
                    id: format!("scene-{number}").into(),
                    title: format!("Episode {number}").into(),
                    position: "0:00:42".into(),
                    note: "A saved scene with its own actions".into(),
                    added: "Added date unknown".into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.show().unwrap();
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 700));
        settle(&app);
        // Starting from either end also checks reverse traversal and makes
        // this independent of the selection retained across a resize.
        for key in [
            slint::platform::Key::UpArrow,
            slint::platform::Key::DownArrow,
        ] {
            let text: slint::SharedString = key.into();
            for _ in 0..10 {
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: text.clone(),
                    });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                        text: text.clone(),
                    });
                settle(&app);
            }
        }
        let caption = ElementHandle::find_by_accessible_label(&app, "Episode 10 · 0:00:42")
            .next()
            .expect("last scene caption");
        let top = caption.absolute_position().y;
        let card = ElementHandle::find_by_element_id(&app, "BookmarksPage::scene_card")
            .find(|card| {
                top >= card.absolute_position().y
                    && top < card.absolute_position().y + card.size().height
            })
            .expect("selected scene card");
        let viewport = ElementHandle::find_by_element_id(&app, "BookmarksPage::archive_scroll")
            .next()
            .unwrap();
        assert!(card.absolute_position().y >= viewport.absolute_position().y - 0.5);
        assert!(
            card.absolute_position().y + card.size().height
                <= viewport.absolute_position().y + viewport.size().height + 0.5,
            "selected card and its action row fit at {width}px: card={:?}/{:?}, viewport={:?}/{:?}",
            card.absolute_position(),
            card.size(),
            viewport.absolute_position(),
            viewport.size()
        );
    }
}

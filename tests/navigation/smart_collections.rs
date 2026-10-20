//! Collection rules remain editable on portrait phones and desktop, with local
//! Back handling and stable-ID selection independent of manual categories.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_ui_shared::{SmartCollectionRow, SmartCollectionsBridge};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova_ui::AppWindow) {
    for _ in 0..10 {
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
        .unwrap_or_else(|| panic!("missing collection control: {label}"))
}
fn scroll(app: &nova_ui::AppWindow, delta_y: f32) {
    let size = app.window().size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(
                size.width as f32 / 2.0,
                size.height as f32 / 2.0,
            ),
            delta_x: 0.0,
            delta_y,
        });
    settle(app);
}
fn reveal(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    scroll(app, 3000.0);
    for _ in 0..40 {
        let control = button(app, label);
        let pos = control.absolute_position();
        if pos.y >= 18.0 && pos.y + control.size().height < app.window().size().height as f32 - 18.0
        {
            return control;
        }
        scroll(app, -70.0);
    }
    panic!("collection action must be reachable: {label}");
}
fn fixture() -> nova_ui::AppWindow {
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_library(true);
    app.window().show().unwrap();
    settle(&app);
    app
}
fn key(app: &nova_ui::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn assert_editor_form_containment(app: &nova_ui::AppWindow) {
    let form = ElementHandle::find_by_element_id(app, "SmartCollectionsEditor::form")
        .next()
        .expect("mounted collection form");
    let origin = form.absolute_position();
    let size = form.size();
    for text in form.query_descendants().match_type_name("Text").find_all() {
        let pos = text.absolute_position();
        let text_size = text.size();
        assert!(
            pos.x >= origin.x + 15.0 && pos.x + text_size.width <= origin.x + size.width - 15.0,
            "collection form text stays inside horizontal padding: {pos:?} {text_size:?}, form {origin:?} {size:?}"
        );
        assert!(
            pos.y >= origin.y + 15.0 && pos.y + text_size.height <= origin.y + size.height - 15.0,
            "collection form text stays inside vertical padding: {pos:?} {text_size:?}, form {origin:?} {size:?}"
        );
    }
}
fn assert_collection_rail_rows_do_not_overlap(app: &nova_ui::AppWindow) {
    let header = ElementHandle::find_by_element_id(app, "SmartCollectionsRail::rail_header")
        .next()
        .expect("collection rail heading row");
    let chips = ElementHandle::find_by_element_id(app, "SmartCollectionsRail::rail")
        .next()
        .expect("collection chip viewport");
    let pos = header.absolute_position();
    let size = header.size();
    assert!(
        chips.absolute_position().y >= pos.y + size.height + 9.0,
        "heading and actions must finish before the chip row begins"
    );
    for text in header
        .query_descendants()
        .match_type_name("Text")
        .find_all()
    {
        let row = text.absolute_position();
        let row_size = text.size();
        assert!(
            row.y >= pos.y - 0.5 && row.y + row_size.height <= pos.y + size.height + 0.5,
            "rail header text stays inside its own row"
        );
    }
    for control in header
        .query_descendants()
        .match_predicate(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .find_all()
    {
        let row = control.absolute_position();
        let row_size = control.size();
        assert!(
            row.y >= pos.y - 0.5 && row.y + row_size.height <= pos.y + size.height + 0.5,
            "rail actions stay above the chips"
        );
    }
}

#[test]
fn portrait_and_desktop_editor_keeps_fields_and_save_reachable() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::portrait_and_desktop_editor_keeps_fields_and_save_reachable"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = fixture();
    let smart = app.global::<SmartCollectionsBridge>();
    let saved = Rc::new(RefCell::new(Vec::new()));
    let recorded = saved.clone();
    let weak = app.as_weak();
    smart.on_save(move || {
        let app = weak.upgrade().unwrap();
        let smart = app.global::<SmartCollectionsBridge>();
        recorded.borrow_mut().push((
            smart.get_name().to_string(),
            smart.get_media_type(),
            smart.get_watch_state(),
            smart.get_genre().to_string(),
            smart.get_max_runtime().to_string(),
        ));
        smart.set_editor_open(false);
    });
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        smart.set_name("Short sci-fi".into());
        smart.set_media_type(1);
        smart.set_watch_state(1);
        smart.set_genre("Sci-Fi".into());
        smart.set_max_runtime("120".into());
        smart.set_editing_id("".into());
        smart.set_editor_open(true);
        settle(&app);
        for label in [
            "Name your collection",
            "Any genre, or enter one like Sci-Fi",
            "No limit, or enter minutes",
        ] {
            let field = ElementHandle::find_by_accessible_label(&app, label)
                .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
                .unwrap();
            let pos = field.absolute_position();
            assert!(
                pos.x >= 0.0 && pos.x + field.size().width <= width as f32 + 0.5,
                "{label} stays within {width}px"
            );
            assert!(field.size().height >= 44.0);
        }
        assert_editor_form_containment(&app);
        let save = reveal(&app, "Save collection");
        assert_editor_form_containment(&app);
        let pos = save.absolute_position();
        assert!(pos.x >= 0.0 && pos.x + save.size().width <= width as f32 + 0.5);
        save.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(!smart.get_editor_open());
    }
    assert_eq!(saved.borrow().len(), 3);
    for recorded in saved.borrow().iter() {
        assert_eq!(
            recorded,
            &("Short sci-fi".into(), 1, 1, "Sci-Fi".into(), "120".into())
        );
    }
}

#[test]
fn selection_uses_rule_id_and_back_dismisses_editor_without_resetting_category() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::selection_uses_rule_id_and_back_dismisses_editor_without_resetting_category"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = fixture();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.set_library_filter_category("Weekend".into());
    let smart = app.global::<SmartCollectionsBridge>();
    smart.set_rows(
        Rc::new(slint::VecModel::from(vec![SmartCollectionRow {
            id: "stable-scifi".into(),
            name: "Short sci-fi".into(),
        }]))
        .into(),
    );
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    let weak = app.as_weak();
    smart.on_selected(move |id| {
        recorded.borrow_mut().push(id.to_string());
        weak.upgrade()
            .unwrap()
            .global::<SmartCollectionsBridge>()
            .set_selected_id(id);
    });
    let edited = Rc::new(RefCell::new(Vec::new()));
    let recorded = edited.clone();
    let weak = app.as_weak();
    smart.on_edit(move |id| {
        recorded.borrow_mut().push(id.to_string());
        let app = weak.upgrade().unwrap();
        let smart = app.global::<SmartCollectionsBridge>();
        smart.set_editing_id(id);
        smart.set_editor_open(true);
    });
    settle(&app);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        assert_collection_rail_rows_do_not_overlap(&app);
    }
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    settle(&app);
    button(&app, "Short sci-fi").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    button(&app, "Edit").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(smart.get_editor_open());
    key(&app, slint::platform::Key::Escape.into());
    assert!(!smart.get_editor_open());
    assert!(app.get_show_library() && !app.get_show_home());
    assert_eq!(app.get_library_filter_category(), "Weekend");
    assert_eq!(selected.borrow().as_slice(), &["stable-scifi"]);
    assert_eq!(edited.borrow().as_slice(), &["stable-scifi"]);
}

#[test]
fn sync_comparison_back_stays_inside_the_collection_editor() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::sync_comparison_back_stays_inside_the_collection_editor"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = fixture();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    let smart = app.global::<SmartCollectionsBridge>();
    smart.set_editor_open(true);
    settle(&app);
    let sync = app.global::<nova_ui_shared::SettingsSync>();
    sync.set_enabled(true);
    sync.set_selected_key("smart_collections".into());
    sync.set_selected_title("Smart collections".into());
    sync.set_open(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Sync status")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Escape.into());
    assert!(!sync.get_open());
    assert!(smart.get_editor_open());
    assert!(app.get_show_library());
    key(&app, slint::platform::Key::Escape.into());
    assert!(!smart.get_editor_open());
}

#[test]
#[ignore = "requires a display; optional native collection design preview"]
fn render_smart_collection_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_smart_collection_previews"
    )) {
        return;
    }
    let app = nova_ui::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_library(true);
    let categories = ["Plan to Watch", "Watching", "Completed", "Favorites"]
        .into_iter()
        .map(slint::SharedString::from)
        .collect::<Vec<_>>();
    app.set_library_category_names(Rc::new(slint::VecModel::from(categories.clone())).into());
    app.set_library_category_labels(Rc::new(slint::VecModel::from(categories)).into());
    app.set_library(
        Rc::new(slint::VecModel::from(
            [
                ("Arrival", "2016"),
                ("Moon", "2009"),
                ("The Vast of Night", "2019"),
            ]
            .into_iter()
            .map(|(title, year)| nova_ui::MediaCard {
                id: title.into(),
                title: title.into(),
                year: year.into(),
                media_type: "Movie".into(),
                status: "Plan to Watch".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let smart = app.global::<SmartCollectionsBridge>();
    smart.set_rows(
        Rc::new(slint::VecModel::from(vec![
            SmartCollectionRow {
                id: "scifi".into(),
                name: "Short sci-fi discoveries".into(),
            },
            SmartCollectionRow {
                id: "series".into(),
                name: "Weekend series".into(),
            },
        ]))
        .into(),
    );
    smart.set_selected_id("scifi".into());
    smart.set_selected_name("Short sci-fi discoveries".into());
    smart.set_name("Short sci-fi discoveries".into());
    smart.set_editing_id("scifi".into());
    smart.set_media_type(1);
    smart.set_watch_state(1);
    smart.set_genre("Sci-Fi".into());
    smart.set_max_runtime("120".into());
    let sync = app.global::<nova_ui_shared::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(
        Rc::new(slint::VecModel::from(vec![
            nova_ui_shared::SettingSyncInfo {
                key: "smart_collections".into(),
                status: 1,
                can_override: true,
                has_shared: true,
                shared_supported: true,
                local_value: "2 collections · Short sci-fi discoveries, Weekend series".into(),
                shared_value: "2 collections · Short sci-fi discoveries, Weekend series".into(),
            },
        ]))
        .into(),
    );
    sync.on_lookup(|_| 0);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let phase = Rc::new(std::cell::Cell::new(0));
    let step = phase.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let names = [
                "smart-library-desktop",
                "smart-editor-desktop",
                "smart-library-phone",
                "smart-editor-phone",
                "smart-editor-compact",
            ];
            let pixels = app.window().take_snapshot().unwrap();
            if app.global::<SmartCollectionsBridge>().get_editor_open() {
                assert_editor_form_containment(&app);
            } else {
                assert_collection_rail_rows_do_not_overlap(&app);
            }
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
                1 | 3 => {
                    app.global::<SmartCollectionsBridge>().set_editor_open(true);
                }
                2 => {
                    app.global::<SmartCollectionsBridge>()
                        .set_editor_open(false);
                    app.window().set_size(slint::PhysicalSize::new(390, 844));
                }
                4 => {
                    app.window().set_size(slint::PhysicalSize::new(320, 844));
                }
                _ => {
                    slint::quit_event_loop().unwrap();
                }
            }
        },
    );
    app.run().unwrap();
}

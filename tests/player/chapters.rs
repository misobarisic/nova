//! Embedded chapter UI follows preview-only scrubbing and existing menu seeking.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn chapters() -> slint::ModelRc<nova::ChapterRow> {
    Rc::new(slint::VecModel::from(vec![
        nova::ChapterRow {
            position: 0.0,
            title: "Opening".into(),
        },
        nova::ChapterRow {
            position: 40.0,
            title: "Act two".into(),
        },
        nova::ChapterRow {
            position: 75.0,
            title: "Finale".into(),
        },
    ]))
    .into()
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

#[test]
fn embedded_markers_and_titles_follow_hover_and_held_scrubbing() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::embedded_markers_and_titles_follow_hover_and_held_scrubbing"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(100.0);
    app.set_player_chapters(chapters());
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |value| recorded.borrow_mut().push(value));
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.show().unwrap();
    for (width, height, android) in [(1280, 720, false), (390, 844, true), (900, 360, true)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        app.set_position(10.0);
        app.set_osd_visible(true);
        settle(&app);
        assert_eq!(
            element(&app, "PlayerOverlay::current_chapter_title")
                .accessible_label()
                .as_deref(),
            Some("Opening")
        );
        let track = element(&app, "PlayerOverlay::seek_touch");
        let origin = track.absolute_position();
        let size = track.size();
        let markers: Vec<_> =
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::chapter_marker").collect();
        assert_eq!(markers.len(), 2);
        for (marker, fraction) in markers.iter().zip([0.4, 0.75]) {
            assert!(
                (marker.absolute_position().x - (origin.x + size.width * fraction - 1.5)).abs()
                    < 1.0
            );
        }
        let point = |fraction| {
            LogicalPosition::new(
                origin.x + size.width * fraction,
                origin.y + size.height / 2.0,
            )
        };
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: point(0.5),
        });
        settle(&app);
        assert_eq!(
            element(&app, "PlayerOverlay::seek_chapter_title")
                .accessible_label()
                .as_deref(),
            Some("Act two")
        );
        let before = seeks.borrow().len();
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: point(0.5),
            button: slint::platform::PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: point(0.8),
        });
        app.set_position(11.0); // a native progress tick cannot move the held title
        settle(&app);
        assert_eq!(
            element(&app, "PlayerOverlay::current_chapter_title")
                .accessible_label()
                .as_deref(),
            Some("Finale")
        );
        assert_eq!(
            element(&app, "PlayerOverlay::seek_chapter_title")
                .accessible_label()
                .as_deref(),
            Some("Finale")
        );
        assert_eq!(seeks.borrow().len(), before);
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: point(0.8),
            button: slint::platform::PointerEventButton::Left,
        });
        assert_eq!(seeks.borrow().len(), before + 1);
        assert!((seeks.borrow()[before] - 80.0).abs() < 0.1);
    }
    app.set_player_chapters(Default::default());
    settle(&app);
    assert_eq!(
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::chapter_marker").count(),
        0
    );
    assert_eq!(
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::current_chapter_title").count(),
        0
    );
}

#[test]
fn accessible_chapter_menu_supports_keyboard_selection_and_hides_without_metadata() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::accessible_chapter_menu_supports_keyboard_selection_and_hides_without_metadata"
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
    app.set_duration(100.0);
    app.set_osd_visible(true);
    app.set_player_chapters(chapters());
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |value| recorded.borrow_mut().push(value));
    app.show().unwrap();
    settle(&app);
    element(&app, "PlayerOverlay::settings_gear")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    button(&app, "Chapters").mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let _ = button(&app, "Act two");
    for key in [
        slint::platform::Key::DownArrow,
        slint::platform::Key::Return,
    ] {
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    assert_eq!(&*seeks.borrow(), &[40.0]);
    app.set_player_chapters(Default::default());
    app.set_osd_visible(true);
    settle(&app);
    element(&app, "PlayerOverlay::settings_gear")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        !ElementHandle::find_by_accessible_label(&app, "Chapters")
            .any(|item| item.accessible_role() == Some(AccessibleRole::Button))
    );
}

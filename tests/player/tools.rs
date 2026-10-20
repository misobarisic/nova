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
            Rc::new(slint::VecModel::from(vec![nova::ChapterRow {
                position: 45.0,
                title: "The reveal".into(),
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
        let notes = Rc::new(RefCell::new(Vec::new()));
        let recorded = notes.clone();
        app.on_add_scene_bookmark(move |note| recorded.borrow_mut().push(note.to_string()));
        let deleted = Rc::new(Cell::new(-1));
        let recorded = deleted.clone();
        app.on_delete_scene_bookmark(move |index| recorded.set(index));
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
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        open_page(&app, 7);
        let _ = button(&app, "The reveal");
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(&*notes.borrow(), &[String::new()]);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(deleted.get(), 0);
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        open_page(&app, 8);
        assert_eq!(retries.get(), 1);
    }
}

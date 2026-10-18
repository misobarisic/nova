//! The autoplay preview shares the terminal menu's modal/input ownership.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..4 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().unwrap()
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}

fn show_preview(app: &nova::AppWindow) {
    app.set_playback_end_menu_visible(false);
    app.set_auto_next_preview(true);
    settle(app);
}

#[test]
fn automatic_next_preview_fits_and_cancel_returns_to_existing_menu() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::automatic_next_preview_fits_and_cancel_returns_to_existing_menu"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_player_show_title("A long show title which must fit a narrow phone".into());
    app.set_player_episode_title("Season 10 · Episode 100".into());
    app.set_auto_next_preview_title("S10 E101 · The next episode".into());
    app.set_auto_next_preview_source(
        "Show.S10E101.1080p.WEB-DL.x265-A-Very-Long-Release-Group".into(),
    );
    app.set_auto_next_preview_metadata("Addon · 1080p · HEVC · WEB-DL".into());
    app.set_auto_next_seconds(8);
    app.set_playback_end_menu_has_next(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let captured = actions.clone();
    let weak = app.as_weak();
    app.on_auto_next_play(move || {
        captured.borrow_mut().push("play");
        weak.upgrade().unwrap().set_auto_next_preview(false);
    });
    let captured = actions.clone();
    app.on_playback_end_next(move || captured.borrow_mut().push("manual next"));
    let captured = actions.clone();
    let weak = app.as_weak();
    app.on_playback_end_stay(move || {
        let app = weak.upgrade().unwrap();
        if app.get_auto_next_preview() {
            captured.borrow_mut().push("cancel");
            app.set_auto_next_preview(false);
            app.set_playback_end_menu_visible(true);
        } else {
            captured.borrow_mut().push("stay");
            app.set_playback_end_menu_visible(false);
        }
    });
    app.on_close_player(|| panic!("preview Back must not close the player"));
    app.on_toggle_pause(|| panic!("preview Return must not toggle playback"));
    app.window().show().unwrap();

    for (width, height) in [(320, 568), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for language in ["en", "hr"] {
            slint::select_bundled_translation(language).unwrap();
            show_preview(&app);
            assert!(app.get_playback_end_menu_shown());
            let content = element(&app, "PlaybackEnded::ended_content");
            let position = content.absolute_position();
            let size = content.size();
            assert!(position.x >= 0.0 && position.y >= 0.0);
            assert!(position.x + size.width <= width as f32 + 1.0);
            assert!(position.y + size.height <= height as f32 + 1.0);
            let buttons = element(&app, "AppWindow::playback_ended")
                .query_descendants()
                .match_predicate(|item| item.accessible_role() == Some(AccessibleRole::Button))
                .find_all();
            assert_eq!(buttons.len(), 2, "preview offers Play now and Cancel");
            for button in &buttons {
                let position = button.absolute_position();
                let size = button.size();
                assert!(size.height >= 44.0 && size.width >= 44.0);
                assert!(position.y >= 0.0 && position.y + size.height <= height as f32 + 1.0);
            }
            key(&app, slint::platform::Key::Return);
            assert_eq!(actions.borrow().last(), Some(&"play"));
            show_preview(&app);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(actions.borrow().last(), Some(&"cancel"));
            assert!(!app.get_auto_next_preview());
            assert!(app.get_playback_end_menu_visible());
            assert!(app.get_player_open());
            let buttons = element(&app, "AppWindow::playback_ended")
                .query_descendants()
                .match_predicate(|item| item.accessible_role() == Some(AccessibleRole::Button))
                .find_all();
            assert_eq!(buttons.len(), 3, "cancel restores the original choices");
            show_preview(&app);
            app.set_system_back_request(app.get_system_back_request() + 1);
            settle(&app);
            assert_eq!(actions.borrow().last(), Some(&"cancel"));
            assert!(app.get_playback_end_menu_visible());
            show_preview(&app);
            key(&app, slint::platform::Key::Escape);
            assert_eq!(actions.borrow().last(), Some(&"cancel"));
            assert!(app.get_playback_end_menu_visible());
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

//! The delayed EOF menu is a modal UI; the backend owns its timing and session.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn advance(app: &nova::AppWindow, millis: u64) {
    for _ in 0..millis.div_ceil(10) {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (
                element.size(),
                element.absolute_position(),
                element.computed_opacity(),
            );
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    advance(app, 20);
}

fn setup() -> (nova::AppWindow, Rc<RefCell<Vec<&'static str>>>) {
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_player_show_title(
        "A long completed title with plenty of words to test the narrow layout".into(),
    );
    app.set_player_episode_title("Season 12 · Episode 100 · A long episode title".into());
    app.set_playback_end_menu_next_title("S12 E101 · The next episode title".into());
    app.set_player_poster(slint::Image::from_rgb8(slint::SharedPixelBuffer::new(4, 4)));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let captured = actions.clone();
    app.on_playback_end_next(move || captured.borrow_mut().push("next"));
    let captured = actions.clone();
    app.on_playback_end_details(move || captured.borrow_mut().push("details"));
    let captured = actions.clone();
    let weak = app.as_weak();
    app.on_playback_end_stay(move || {
        captured.borrow_mut().push("stay");
        weak.upgrade().unwrap().set_playback_end_menu_visible(false);
    });
    app.on_close_player(|| panic!("end menu must consume player Back"));
    app.on_toggle_pause(|| panic!("end menu must consume player Return"));
    app.window().show().unwrap();
    (app, actions)
}

#[test]
fn end_menu_fits_translations_and_routes_pointer_keyboard_and_system_back() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::end_menu_fits_translations_and_routes_pointer_keyboard_and_system_back"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let (app, actions) = setup();
    for (width, height) in [(320, 568), (390, 844), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for language in ["en", "hr"] {
            slint::select_bundled_translation(language).unwrap();
            for has_next in [false, true] {
                app.set_playback_end_menu_has_next(has_next);
                app.set_playback_end_menu_visible(true);
                advance(&app, 30);
                let content = element(&app, "PlaybackEnded::ended_content");
                let position = content.absolute_position();
                let size = content.size();
                assert!(position.x >= 0.0 && position.y >= 0.0);
                assert!(position.x + size.width <= width as f32 + 1.0);
                assert!(position.y + size.height <= height as f32 + 1.0);
                let artwork = element(&app, "PlaybackEnded::ended_artwork");
                assert!((artwork.size().width - width as f32).abs() < 0.5);
                assert!((artwork.size().height - height as f32).abs() < 0.5);
                let menu = element(&app, "AppWindow::playback_ended");
                let buttons = menu
                    .query_descendants()
                    .match_predicate(|item| item.accessible_role() == Some(AccessibleRole::Button))
                    .find_all();
                assert_eq!(buttons.len(), if has_next { 3 } else { 2 });
                for button in &buttons {
                    let point = button.absolute_position();
                    let size = button.size();
                    assert!(size.height >= 44.0 && size.width > 100.0);
                    assert!(point.y >= position.y && point.y + size.height <= height as f32);
                }
                key(&app, slint::platform::Key::Return);
                assert_eq!(
                    *actions.borrow().last().unwrap(),
                    if has_next { "next" } else { "stay" }
                );
                app.set_playback_end_menu_visible(true);
                advance(&app, 20);
                // Arrow navigation reaches Details for both movies and series.
                key(&app, slint::platform::Key::UpArrow);
                key(&app, slint::platform::Key::Return);
                assert_eq!(*actions.borrow().last().unwrap(), "details");
                buttons
                    .last()
                    .unwrap()
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                assert_eq!(*actions.borrow().last().unwrap(), "details");
                app.set_system_back_request(app.get_system_back_request() + 1);
                advance(&app, 20);
                assert_eq!(*actions.borrow().last().unwrap(), "stay");
                assert!(app.get_player_open());
                app.set_playback_end_menu_visible(true);
                advance(&app, 20);
                key(&app, slint::platform::Key::Escape);
                assert!(!app.get_playback_end_menu_visible());
                assert!(app.get_player_open());
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn end_menu_retains_fade_and_yields_to_higher_modals() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::end_menu_retains_fade_and_yields_to_higher_modals"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let (app, _) = setup();
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    app.global::<nova::Anim>().set_enabled(true);
    app.set_playback_end_menu_visible(true);
    advance(&app, 60);
    let menu = element(&app, "AppWindow::playback_ended");
    assert!(menu.computed_opacity() > 0.0 && menu.computed_opacity() < 1.0);
    advance(&app, 300);
    assert_eq!(menu.computed_opacity(), 1.0);
    app.set_playback_end_menu_visible(false);
    advance(&app, 60);
    assert!(menu.computed_opacity() > 0.0 && menu.computed_opacity() < 1.0);
    advance(&app, 300);
    assert_eq!(menu.computed_opacity(), 0.0);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_playback_end_menu_visible(true);
    advance(&app, 20);
    assert_eq!(menu.computed_opacity(), 1.0);
    app.set_resume_prompt_visible(true);
    advance(&app, 20);
    assert!(!app.get_playback_end_menu_shown());
    assert_eq!(menu.computed_opacity(), 0.0);
    app.set_resume_prompt_visible(false);
    advance(&app, 20);
    assert!(app.get_playback_end_menu_shown());
    // Completion ratings take over once playback closes. A pending end-menu
    // request must not take focus or paint over their higher modal lane.
    app.set_completion_prompt_open(true);
    app.set_player_open(false);
    advance(&app, 20);
    assert!(app.get_completion_prompt_visible());
    assert!(!app.get_playback_end_menu_shown());
    app.set_completion_prompt_open(false);
    app.set_player_open(true);
    advance(&app, 20);
    assert!(app.get_playback_end_menu_shown());
    app.set_party_page_open(true);
    advance(&app, 20);
    assert!(!app.get_playback_end_menu_shown());
}

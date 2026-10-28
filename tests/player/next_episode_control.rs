//! The footer action always opens a stream selector; end-of-episode offers are separate.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, platform::WindowEvent};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..4 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.absolute_position(), item.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn key(app: &nova::AppWindow, text: impl Into<SharedString>) {
    let text = text.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}

fn next(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_id(app, "PlayerOverlay::next_episode_button").next()
}

fn setup() -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_position(30.0);
    app.set_duration(3600.0);
    app.set_next_episode_button_available(true);
    app.set_next_episode_button_token("footer:session:2".into());
    app.set_next_episode_token("banner:session:2".into());
    app.set_next_episode_visible(false);
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.on_toggle_pause(|| panic!("Next must only open the selector"));
    app.on_seek(|_| panic!("Next must not seek"));
    app.on_seek_relative(|_| panic!("Next must not seek"));
    app.on_playback_speed_changed(|_| panic!("Next must not alter speed"));
    app.on_close_player(|| panic!("UI must leave the source transition to the controller"));
    app.on_next_episode_dismiss(|_| panic!("footer must not dismiss the banner"));
    app.window().show().unwrap();
    settle(&app);
    app
}

#[test]
fn footer_next_opens_current_selector_early_independently_of_banner_and_policy() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::footer_next_opens_current_selector_early_independently_of_banner_and_policy"
    )) {
        return;
    }
    let app = setup();
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    app.on_next_episode_streams(move |token| recorded.borrow_mut().push(token.to_string()));
    let action = next(&app).expect("next is available at 30 seconds, before any end offer");
    assert_eq!(action.accessible_role(), Some(AccessibleRole::Button));
    assert_eq!(action.accessible_label().as_deref(), Some("Next episode"));
    assert_eq!(action.accessible_enabled(), Some(true));
    action.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(&*selected.borrow(), &["footer:session:2"]);
    assert!(app.get_player_open());
    assert_eq!(app.get_position(), 30.0);
    app.set_next_episode_visible(true);
    settle(&app);
    app.set_next_episode_visible(false); // Dismissing/clearing an offer cannot hide the footer.
    app.set_next_episode_button_token("footer:session:3".into());
    settle(&app);
    next(&app)
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(
        &*selected.borrow(),
        &["footer:session:2", "footer:session:3"]
    );
    app.set_party_active(true);
    app.set_party_can_control(false);
    settle(&app);
    assert_eq!(next(&app).unwrap().accessible_enabled(), Some(false));
    next(&app)
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(selected.borrow().len(), 2);
    app.set_party_can_control(true);
    app.set_party_busy(true);
    settle(&app);
    assert_eq!(next(&app).unwrap().accessible_enabled(), Some(false));
    app.set_party_busy(false);
    settle(&app);
    assert_eq!(next(&app).unwrap().accessible_enabled(), Some(true));
    app.set_next_episode_button_token("".into());
    settle(&app);
    assert_eq!(next(&app).unwrap().accessible_enabled(), Some(false));
    app.set_next_episode_button_available(false); // No released successor/movie/finale.
    settle(&app);
    assert!(next(&app).is_none());
}

#[test]
fn footer_next_keyboard_tv_and_compact_geometry_preserve_other_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::footer_next_keyboard_tv_and_compact_geometry_preserve_other_controls"
    )) {
        return;
    }
    let app = setup();
    let selected = Rc::new(RefCell::new(Vec::new()));
    let recorded = selected.clone();
    app.on_next_episode_streams(move |token| recorded.borrow_mut().push(token.to_string()));
    // Next starts the right-hand group, before subtitles, audio and speed.
    // Desktop order reaches it after pause, rewind, forward and volume.
    for _ in 0..4 {
        key(&app, slint::platform::Key::Tab);
    }
    key(&app, slint::platform::Key::Return);
    assert_eq!(&*selected.borrow(), &["footer:session:2"]);
    app.set_party_active(true);
    app.set_party_can_control(false);
    settle(&app);
    key(&app, slint::platform::Key::Return); // Revocation moves focus to Playback.
    assert_eq!(selected.borrow().len(), 1);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Escape);
    app.set_party_active(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    settle(&app);
    // TV mode resets to Pause; volume is absent and speed is in Playback.
    for _ in 0..3 {
        key(&app, slint::platform::Key::RightArrow);
    }
    key(&app, slint::platform::Key::Return);
    assert_eq!(selected.borrow().len(), 2);
    app.global::<nova::InterfaceStyle>().set_tv(false);
    for (width, height, android) in [
        (320, 640, false),
        (360, 800, true),
        (740, 320, true),
        (1280, 720, false),
    ] {
        app.set_is_android(android);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_osd_visible(true);
        settle(&app);
        let action = next(&app).unwrap();
        let p = action.absolute_position();
        assert!(p.x >= 0.0 && p.y >= 0.0);
        assert!(p.x + action.size().width <= width as f32 + 0.5);
        assert!(p.y + action.size().height <= height as f32 + 0.5);
        if let Some(subtitles) =
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::subtitle_button").next()
        {
            assert!(
                p.x + action.size().width <= subtitles.absolute_position().x + 0.5,
                "Next must precede subtitles at {width}×{height}"
            );
        }
        let mut lane = Vec::new();
        for id in [
            "desktop_pause",
            "vol_group",
            "next_episode_button",
            "speed_button",
            "screenshot_button",
            "settings_gear",
            "fullscreen_button",
        ] {
            if let Some(control) =
                ElementHandle::find_by_element_id(&app, &format!("PlayerOverlay::{id}")).next()
            {
                let cp = control.absolute_position();
                assert!(
                    cp.x >= -0.5 && cp.x + control.size().width <= width as f32 + 0.5,
                    "{id} fits at {width}×{height}"
                );
                if (cp.y - p.y).abs() < 2.0 {
                    lane.push((cp.x, control.size().width, id));
                }
            }
        }
        lane.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in lane.windows(2) {
            assert!(
                pair[0].0 + pair[0].1 <= pair[1].0 + 0.5,
                "{} overlaps {} at {width}×{height}",
                pair[0].2,
                pair[1].2
            );
        }
        assert!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
                .next()
                .is_some()
        );
        assert!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::screenshot_button")
                .next()
                .is_some()
        );
        if width == 320 {
            assert!(
                ElementHandle::find_by_element_id(&app, "PlayerOverlay::speed_button")
                    .next()
                    .is_none(),
                "compact speed remains available through Playback"
            );
        }
    }
}

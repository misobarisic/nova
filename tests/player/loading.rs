//! Initial startup is a branding surface with one exit, not a transport OSD.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{
    ComponentHandle,
    platform::{Key, WindowEvent},
};
use std::{cell::Cell, rc::Rc, time::Duration};
fn settle(app: &nova::AppWindow, ms: u64) {
    for _ in 0..ms.div_ceil(20) {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (
                item.size(),
                item.absolute_position(),
                item.computed_opacity(),
            );
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}
fn key(app: &nova::AppWindow, key: impl Into<slint::SharedString>) {
    let text = key.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app, 40);
}

#[test]
fn initial_loading_has_only_back_and_title_fallback_and_keeps_errors_visible() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::initial_loading_has_only_back_and_title_fallback_and_keeps_errors_visible"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_player_status("Connecting…".into());
    app.set_player_show_title("A title without a logo".into());
    let transport = Rc::new(Cell::new(0));
    let calls = transport.clone();
    app.on_toggle_pause(move || calls.set(calls.get() + 1));
    let calls = transport.clone();
    app.on_seek_relative(move |_| calls.set(calls.get() + 1));
    let calls = transport.clone();
    app.on_volume_step(move |_| calls.set(calls.get() + 1));
    let calls = transport.clone();
    app.on_take_screenshot(move || calls.set(calls.get() + 1));
    let exits = Rc::new(Cell::new(0));
    let calls = exits.clone();
    app.on_close_player(move || calls.set(calls.get() + 1));
    app.show().unwrap();
    for (width, height, android, tv) in [
        (390, 844, true, false),
        (844, 390, true, false),
        (1280, 720, false, false),
        (1280, 720, false, true),
    ] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        app.set_interface_mode_index(if tv { 2 } else { 1 });
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.set_playback_started(false);
        app.set_player_logo(slint::Image::default());
        app.set_player_status("Connecting…".into());
        app.set_show_stream_loading_text(true);
        app.set_player_status_is_error(false);
        settle(&app, 100);
        let title = element(&app, "PlayerLoadingArtwork::loading_title");
        assert_eq!(
            title.accessible_label().as_deref(),
            Some("A title without a logo")
        );
        let status = element(&app, "PlayerOverlay::loading_status_text");
        assert!(status.computed_opacity() > 0.9);
        let status_y = status.absolute_position().y;
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(4, 2);
        pixels
            .make_mut_slice()
            .fill(slint::Rgba8Pixel::new(240, 240, 240, 180));
        app.set_player_logo(slint::Image::from_rgba8(pixels));
        settle(&app, 100);
        assert!(
            (element(&app, "PlayerOverlay::loading_status_text")
                .absolute_position()
                .y
                - status_y)
                .abs()
                < 0.5
        );
        let back = element(&app, "PlayerOverlay::close_button");
        assert!(back.absolute_position().x < 40.0 && back.absolute_position().y < 40.0);
        assert!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::osd")
                .next()
                .is_none()
        );
        assert!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::player_header")
                .next()
                .is_none()
        );
        key(&app, " ");
        for key_code in [Key::RightArrow, Key::UpArrow] {
            key(&app, key_code);
        }
        let position = slint::LogicalPosition::new(width as f32 / 2.0, height as f32 / 2.0);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        settle(&app, 300);
        assert_eq!(transport.get(), 0);
        app.set_show_stream_loading_text(false);
        settle(&app, 60);
        assert!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::loading_status_text")
                .next()
                .is_none()
        );
        app.set_player_status("Cannot play this stream".into());
        app.set_player_status_is_error(true);
        settle(&app, 60);
        assert_eq!(
            element(&app, "PlayerOverlay::loading_status_text").computed_opacity(),
            1.0
        );
        let before = exits.get();
        back.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(exits.get(), before + 1);
        let before = exits.get();
        key(&app, Key::Escape);
        assert_eq!(exits.get(), before + 1);
    }
    // A failed startup is no longer loading: its existing recovery action
    // remains keyboard-accessible even when routine status text is disabled.
    let retries = Rc::new(Cell::new(0));
    let calls = retries.clone();
    app.on_try_another_stream(move || calls.set(calls.get() + 1));
    app.set_try_another_stream_available(true);
    app.set_automatic_stream_recovery_visible(true);
    settle(&app, 100);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(retries.get(), 1);
    assert_eq!(transport.get(), 0);
    app.set_automatic_stream_recovery_visible(false);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    settle(&app, 100);
    assert!(
        ElementHandle::find_by_element_id(&app, "PlayerLoadingArtwork::loading_title")
            .next()
            .is_none()
    );
    assert!(element(&app, "PlayerOverlay::osd").computed_opacity() > 0.9);
}
#[test]
fn logo_pulses_without_moving_status_and_reduced_motion_stops_it() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::logo_pulses_without_moving_status_and_reduced_motion_stops_it"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_player_status("Buffering…".into());
    app.set_player_show_title("A show".into());
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(400, 180);
    pixels
        .make_mut_slice()
        .fill(slint::Rgba8Pixel::new(240, 240, 240, 200));
    app.set_player_logo(slint::Image::from_rgba8(pixels));
    app.show().unwrap();
    settle(&app, 100);
    let y = element(&app, "PlayerOverlay::loading_status_text")
        .absolute_position()
        .y;
    let mut minimum = f32::MAX;
    let mut maximum = 0f32;
    for _ in 0..45 {
        settle(&app, 100);
        let logo = element(&app, "PlayerLoadingArtwork::loading_logo");
        minimum = minimum.min(logo.size().width);
        maximum = maximum.max(logo.size().width);
        assert!(
            (element(&app, "PlayerOverlay::loading_status_text")
                .absolute_position()
                .y
                - y)
                .abs()
                < 0.5
        );
    }
    assert!(
        maximum > minimum * 1.015 && maximum < minimum * 1.04,
        "pulse {minimum}..{maximum}"
    );
    app.global::<nova::Anim>().set_enabled(false);
    settle(&app, 40);
    let resting = element(&app, "PlayerLoadingArtwork::loading_logo")
        .size()
        .width;
    settle(&app, 4000);
    assert_eq!(
        element(&app, "PlayerLoadingArtwork::loading_logo")
            .size()
            .width,
        resting
    );
    app.global::<nova::Anim>().set_player(false);
    app.global::<nova::Anim>().set_enabled(true);
    settle(&app, 4000);
    assert_eq!(
        element(&app, "PlayerLoadingArtwork::loading_logo")
            .size()
            .width,
        resting
    );
}

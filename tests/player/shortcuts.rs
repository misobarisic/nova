//! Direct playback keys, repeat guards, popup ownership and OSD Tab navigation.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn key(app: &nova::AppWindow, text: impl Into<SharedString>, repeat: bool) {
    let text = text.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    if repeat {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressRepeated { text: text.clone() });
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
}

#[test]
fn playback_shortcuts_and_popup_navigation_are_independent() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "playback_shortcuts_and_popup_navigation_are_independent"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_position(100.0);
    app.set_duration(120.0);
    app.window().show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |position| recorded.borrow_mut().push(position));
    let recorded = seeks.clone();
    let weak = app.as_weak();
    app.on_seek_relative(move |delta| {
        let app = weak.upgrade().unwrap();
        let position = (app.get_position() + delta).clamp(0.0, app.get_duration());
        app.set_position(position);
        recorded.borrow_mut().push(position);
    });
    let volumes = Rc::new(RefCell::new(Vec::new()));
    let recorded = volumes.clone();
    app.on_set_volume(move |volume| recorded.borrow_mut().push(volume));
    let recorded = volumes.clone();
    let weak = app.as_weak();
    app.on_volume_step(move |delta| {
        let app = weak.upgrade().unwrap();
        let volume = (app.get_volume() + delta).clamp(0.0, 100.0);
        app.set_volume(volume);
        recorded.borrow_mut().push(volume);
    });
    let system_steps = Rc::new(RefCell::new(Vec::new()));
    let recorded = system_steps.clone();
    app.on_android_volume_step(move |step| recorded.borrow_mut().push(step));
    let pauses = Rc::new(Cell::new(0));
    let recorded = pauses.clone();
    app.on_toggle_pause(move || recorded.set(recorded.get() + 1));
    let fullscreen = Rc::new(Cell::new(0));
    let recorded = fullscreen.clone();
    app.on_toggle_fullscreen(move || recorded.set(recorded.get() + 1));
    let captures = Rc::new(Cell::new(0));
    let recorded = captures.clone();
    app.on_take_screenshot(move || recorded.set(recorded.get() + 1));
    let closes = Rc::new(Cell::new(0));
    let recorded = closes.clone();
    app.on_close_player(move || recorded.set(recorded.get() + 1));
    let wakes = Rc::new(Cell::new(0));
    let recorded = wakes.clone();
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || {
        recorded.set(recorded.get() + 1);
        weak.upgrade().unwrap().set_osd_visible(true);
    });
    app.set_osd_visible(false);
    key(&app, "s", true);
    key(&app, "S", false);
    assert_eq!(captures.get(), 2, "held S captures only once");
    app.set_screenshot_busy(true);
    key(&app, "s", false);
    app.set_screenshot_busy(false);
    app.set_playback_started(false);
    key(&app, "s", false);
    assert_eq!(captures.get(), 2, "busy/loading captures are blocked");
    app.set_playback_started(true);
    key(&app, slint::platform::Key::RightArrow, true);
    key(&app, slint::platform::Key::RightArrow, false);
    key(&app, slint::platform::Key::LeftArrow, false);
    assert_eq!(&*seeks.borrow(), &[110.0, 120.0, 120.0, 110.0]);
    app.set_position(3.0);
    key(&app, slint::platform::Key::LeftArrow, false);
    assert_eq!(app.get_position(), 0.0);
    app.set_duration(0.0);
    let count = seeks.borrow().len();
    key(&app, slint::platform::Key::RightArrow, false);
    assert_eq!(
        seeks.borrow().len(),
        count,
        "unknown duration is not seekable"
    );
    app.set_volume(98.0);
    key(&app, slint::platform::Key::UpArrow, false);
    app.set_volume(2.0);
    key(&app, slint::platform::Key::DownArrow, false);
    assert_eq!(&*volumes.borrow(), &[100.0, 0.0]);
    app.set_is_android(true);
    key(&app, slint::platform::Key::UpArrow, false);
    key(&app, slint::platform::Key::DownArrow, false);
    assert_eq!(&*system_steps.borrow(), &[1, -1]);
    assert_eq!(volumes.borrow().len(), 2, "Android keys use system audio");
    app.set_is_android(false);
    for text in [" ", "k", "K"] {
        key(&app, text, true);
    }
    for text in ["f", "F"] {
        key(&app, text, true);
    }
    assert_eq!(pauses.get(), 3);
    assert_eq!(fullscreen.get(), 2);
    // A modifier chord cannot leak through to playback.
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Control.into(),
        });
    key(&app, "k", false);
    key(&app, "f", false);
    key(&app, "s", false);
    key(&app, slint::platform::Key::RightArrow, false);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Control.into(),
        });
    assert_eq!(pauses.get(), 3);
    assert_eq!(fullscreen.get(), 2);
    assert_eq!(captures.get(), 2, "modifier chords do not capture");
    assert_eq!(seeks.borrow().len(), count);
    assert_eq!(wakes.get(), 0, "direct shortcuts do not wake controls");
    assert!(!app.get_osd_visible());
    app.set_screenshot_notice("Screenshot saved".into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    assert!(
        ElementHandle::find_by_element_id(&app, "PlayerOverlay::screenshot_message")
            .next()
            .unwrap()
            .computed_opacity()
            > 0.99,
        "screenshot confirmation stays visible without the controls"
    );
    assert!(!app.get_osd_visible());
    app.set_screenshot_notice("".into());
    // Seek keys never alter the selected control: Tab still starts at pause.
    for _ in 0..8 {
        key(&app, slint::platform::Key::Tab, false);
    }
    assert!(app.get_osd_visible(), "control navigation reveals focus");
    key(&app, slint::platform::Key::Return, false);
    assert_eq!(captures.get(), 3, "Tab reaches screenshot at index 9");
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Shift.into(),
        });
    key(&app, slint::platform::Key::Tab, false);
    key(&app, slint::platform::Key::Tab, false);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Shift.into(),
        });
    key(&app, slint::platform::Key::Return, false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
            .next()
            .is_some()
    );
    let before = system_steps.borrow().len();
    key(&app, slint::platform::Key::DownArrow, false);
    key(&app, slint::platform::Key::RightArrow, false);
    key(&app, slint::platform::Key::UpArrow, false);
    key(&app, slint::platform::Key::LeftArrow, false);
    assert_eq!(system_steps.borrow().len(), before);
    assert_eq!(seeks.borrow().len(), count, "popup arrows do not seek");
    assert_eq!(
        volumes.borrow().len(),
        2,
        "popup arrows do not change volume"
    );
    key(&app, "s", false);
    assert_eq!(captures.get(), 3, "an open menu owns screenshot keys");
    key(&app, slint::platform::Key::Escape, true);
    assert_eq!(closes.get(), 0, "held Escape dismisses only the menu");
    app.set_is_fullscreen(true);
    app.set_osd_visible(false);
    key(&app, slint::platform::Key::Escape, true);
    assert_eq!(closes.get(), 1, "Escape closes fullscreen playback once");
    assert_eq!(fullscreen.get(), 2, "Escape does not toggle fullscreen");
    assert!(!app.get_osd_visible(), "closing does not flash the OSD");
    app.set_is_fullscreen(false);
    key(&app, slint::platform::Key::Escape, false);
    assert_eq!(
        closes.get(),
        2,
        "windowed playback can also close with Escape"
    );
}

#[test]
fn volume_wheel_keeps_the_control_open_and_tracks_are_complementary() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::volume_wheel_keeps_the_control_open_and_tracks_are_complementary"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    app.set_volume(50.0);
    let steps = Rc::new(RefCell::new(Vec::new()));
    let recorded = steps.clone();
    let weak = app.as_weak();
    app.on_volume_step(move |delta| {
        recorded.borrow_mut().push(delta);
        let app = weak.upgrade().unwrap();
        app.set_volume((app.get_volume() + delta).clamp(0.0, 100.0));
    });
    app.show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let element = |id: &str| {
        ElementHandle::find_by_element_id(&app, &format!("PlayerOverlay::{id}"))
            .next()
            .unwrap()
    };
    let move_to = |id: &str| {
        let item = element(id);
        let p = item.absolute_position();
        let size = item.size();
        let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
        position
    };
    move_to("vol_btn");
    for (id, delta) in [
        ("vol_btn", 120.0),
        ("vol_catch", -120.0),
        ("vol_slider_touch", 120.0),
    ] {
        let position = move_to(id);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position,
                delta_x: 0.0,
                delta_y: delta,
            });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert!(
            element("vol_slider").size().width >= 99.0,
            "wheel collapsed volume over {id}"
        );
    }
    assert_eq!(&*steps.borrow(), &[5.0, -5.0, 5.0]);
    for (volume, muted) in [
        (0.0, false),
        (25.0, false),
        (50.0, false),
        (100.0, false),
        (75.0, true),
    ] {
        app.set_volume(volume);
        app.set_muted(muted);
        let fill = element("vol_fill");
        let rest = element("vol_remaining");
        let track = element("vol_slider");
        let length = track.size().width - 16.0;
        let selected = if muted { 0.0 } else { volume / 100.0 };
        assert!((fill.size().width - length * selected).abs() < 0.5);
        assert!(
            (fill.absolute_position().x + fill.size().width - rest.absolute_position().x).abs()
                < 0.5
        );
        assert!(
            (rest.absolute_position().x + rest.size().width
                - track.absolute_position().x
                - track.size().width
                + 8.0)
                .abs()
                < 0.5
        );
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640.0, 200.0),
        });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    assert_eq!(
        element("vol_slider").size().width,
        0.0,
        "leaving the control still collapses it"
    );
}

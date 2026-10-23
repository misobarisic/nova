//! TV remote navigation, seek previews and switching back to pointer input.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle(app: &nova::AppWindow) {
    for _ in 0..4 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
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

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn setup() -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_position(120.0);
    app.set_duration(600.0);
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.on_format_time(|value| nova::player::format_time(value as f64).into());
    app.window().show().unwrap();
    settle(&app);
    app
}

#[test]
fn tv_remote_moves_between_buttons_and_timeline_and_commits_one_held_seek() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_remote_moves_between_buttons_and_timeline_and_commits_one_held_seek"
    )) {
        return;
    }
    let app = setup();
    app.set_is_android(true);
    let pauses = Rc::new(Cell::new(0));
    let count = pauses.clone();
    let weak = app.as_weak();
    app.on_toggle_pause(move || {
        count.set(count.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_is_paused(!app.get_is_paused());
    });
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    let weak = app.as_weak();
    app.on_seek(move |value| {
        recorded.borrow_mut().push(value);
        weak.upgrade().unwrap().set_position(value);
    });
    let relative = Rc::new(RefCell::new(Vec::new()));
    let recorded = relative.clone();
    app.on_seek_relative(move |value| recorded.borrow_mut().push(value));
    let closes = Rc::new(Cell::new(0));
    let count = closes.clone();
    app.on_close_player(move || count.set(count.get() + 1));

    app.set_osd_visible(false);
    key(&app, slint::platform::Key::Return);
    assert!(app.get_osd_visible(), "OK reveals controls while pausing");
    assert!(app.get_is_paused());
    assert_eq!(pauses.get(), 1);
    key(&app, slint::platform::Key::UpArrow);
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::RightArrow.into(),
    });
    app.window().dispatch_event(WindowEvent::KeyPressRepeated {
        text: slint::platform::Key::RightArrow.into(),
    });
    settle(&app);
    assert!(
        seeks.borrow().is_empty(),
        "repeat events only update a preview"
    );
    assert_eq!(
        app.get_position(),
        120.0,
        "native playhead is not overwritten by preview"
    );
    let track = element(&app, "PlayerOverlay::unbuffered_track");
    let played = element(&app, "PlayerOverlay::played_track");
    assert!((played.size().width / track.size().width - 140.0 / 600.0).abs() < 0.01);
    assert!(track.size().height >= 5.9, "timeline focus is visible");
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::RightArrow.into(),
    });
    assert_eq!(&*seeks.borrow(), &[140.0]);
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(pauses.get(), 2, "Down returns to Play/Pause");
    key(&app, slint::platform::Key::RightArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(
        &*relative.borrow(),
        &[-10.0],
        "bottom-row navigation skips hidden volume"
    );
    for _ in 0..4 {
        key(&app, slint::platform::Key::RightArrow);
    }
    key(&app, slint::platform::Key::Return);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Back);
    assert!(
        app.get_osd_visible(),
        "closing a menu keeps the control row"
    );
    key(&app, slint::platform::Key::Back);
    assert!(
        !app.get_osd_visible(),
        "Back hides OSD before exiting playback"
    );
    key(&app, slint::platform::Key::Back);
    assert_eq!(closes.get(), 1);
}

#[test]
fn tv_hidden_seek_and_party_guards_do_not_require_showing_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_hidden_seek_and_party_guards_do_not_require_showing_controls"
    )) {
        return;
    }
    let app = setup();
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |value| recorded.borrow_mut().push(value));
    let relative = Rc::new(Cell::new(0));
    let count = relative.clone();
    app.on_seek_relative(move |_| count.set(count.get() + 1));
    app.set_osd_visible(false);
    key(&app, slint::platform::Key::LeftArrow);
    assert_eq!(&*seeks.borrow(), &[110.0]);
    assert!(
        !app.get_osd_visible(),
        "hidden seek does not open the whole OSD"
    );
    app.set_duration(0.0);
    key(&app, slint::platform::Key::RightArrow);
    app.set_duration(600.0);
    app.set_playback_started(false);
    key(&app, slint::platform::Key::RightArrow);
    app.set_playback_started(true);
    app.global::<nova::PartyBridge>().set_active(true);
    app.global::<nova::PartyBridge>().set_is_host(false);
    app.set_osd_visible(false);
    key(&app, slint::platform::Key::RightArrow);
    key(&app, "\u{e107}");
    assert_eq!(
        &*seeks.borrow(),
        &[110.0],
        "live/loading/party guests cannot seek"
    );
    assert_eq!(relative.get(), 0);
    app.global::<nova::PartyBridge>().set_active(false);
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::RightArrow.into(),
    });
    key(&app, slint::platform::Key::Back);
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::RightArrow.into(),
    });
    assert_eq!(seeks.borrow().len(), 1, "Back cancels a pending preview");
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&app, slint::platform::Key::RightArrow);
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_eq!(
        seeks.borrow().len(),
        1,
        "modifier chords do not seek in TV mode"
    );
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::RightArrow.into(),
    });
    app.window()
        .dispatch_event(WindowEvent::WindowActiveChanged(false));
    settle(&app);
    app.window()
        .dispatch_event(WindowEvent::WindowActiveChanged(true));
    settle(&app);
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::RightArrow.into(),
    });
    assert_eq!(
        seeks.borrow().len(),
        1,
        "focus loss cancels a held seek before returning"
    );
}

#[test]
fn tv_desktop_pointer_scrubbing_and_settings_work_after_remote_navigation() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_desktop_pointer_scrubbing_and_settings_work_after_remote_navigation"
    )) {
        return;
    }
    let app = setup();
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |value| recorded.borrow_mut().push(value));
    app.set_osd_visible(true);
    key(&app, slint::platform::Key::UpArrow);
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::RightArrow.into(),
    });
    let track = element(&app, "PlayerOverlay::seek_touch");
    track.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let pointer_seeks = seeks.borrow().len();
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::RightArrow.into(),
    });
    assert_eq!(
        seeks.borrow().len(),
        pointer_seeks,
        "mouse input cancels the old remote preview"
    );
    assert!(
        seeks
            .borrow()
            .iter()
            .any(|value| (*value - 300.0).abs() < 1.0),
        "mouse still seeks to clicked position"
    );
    element(&app, "PlayerOverlay::settings_gear")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
            .next()
            .is_some()
    );
    key(&app, slint::platform::Key::Back);
    assert!(app.get_osd_visible());
    let captures = Rc::new(Cell::new(0));
    let count = captures.clone();
    app.on_take_screenshot(move || count.set(count.get() + 1));
    ElementHandle::find_by_accessible_label(&app, "Take screenshot")
        .find(|item| item.size().width >= 55.0)
        .expect("TV screenshot is in the bottom row")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(captures.get(), 1);
    let relative = Rc::new(Cell::new(0));
    let captured = relative.clone();
    app.on_seek_relative(move |_| captured.set(captured.get() + 1));
    app.set_duration(0.0);
    settle(&app);
    element(&app, "PlayerOverlay::rewind_button")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    element(&app, "PlayerOverlay::forward_button")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        relative.get(),
        0,
        "mouse transport buttons also guard live streams"
    );
}

#[test]
fn tv_resume_dialog_and_media_buttons_have_explicit_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_resume_dialog_and_media_buttons_have_explicit_actions"
    )) {
        return;
    }
    let app = setup();
    let pauses = Rc::new(Cell::new(0));
    let count = pauses.clone();
    let weak = app.as_weak();
    app.on_toggle_pause(move || {
        count.set(count.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_is_paused(!app.get_is_paused());
    });
    app.set_osd_visible(false);
    key(&app, "\u{e102}"); // Pause is idempotent, unlike Play/Pause.
    key(&app, "\u{e102}");
    assert_eq!(pauses.get(), 1);
    key(&app, "\u{e101}");
    key(&app, "\u{e101}");
    assert_eq!(pauses.get(), 2);
    assert!(
        !app.get_osd_visible(),
        "media transport keys do not reveal controls"
    );

    let choices = Rc::new(RefCell::new(Vec::new()));
    let captured = choices.clone();
    let weak = app.as_weak();
    app.on_resume_choice(move |resume| {
        captured.borrow_mut().push(resume);
        weak.upgrade().unwrap().set_resume_prompt_visible(false);
    });
    let cancellations = Rc::new(Cell::new(0));
    let captured = cancellations.clone();
    let weak = app.as_weak();
    app.on_resume_cancel(move || {
        captured.set(captured.get() + 1);
        weak.upgrade().unwrap().set_resume_prompt_visible(false);
    });
    app.set_resume_prompt_visible(true);
    settle(&app);
    key(&app, slint::platform::Key::LeftArrow);
    key(&app, slint::platform::Key::Return);
    app.set_resume_prompt_visible(true);
    settle(&app);
    key(&app, slint::platform::Key::LeftArrow);
    key(&app, slint::platform::Key::RightArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(&*choices.borrow(), &[false, true]);
    app.set_resume_prompt_visible(true);
    settle(&app);
    key(&app, slint::platform::Key::Back);
    assert_eq!(cancellations.get(), 1);
    assert_eq!(
        pauses.get(),
        2,
        "dialog OK never leaks into underlying playback"
    );
}

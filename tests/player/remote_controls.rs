//! Remote controls never invoke the local player, and closing them only detaches.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition, SharedString};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn key(app: &nova::AppWindow, text: impl Into<SharedString>) {
    let text = text.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
}

fn click(app: &nova::AppWindow, label: &str) {
    let item = ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"));
    let p = item.absolute_position();
    let size = item.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}

#[test]
fn controls_require_confirmed_session_and_back_only_detaches() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "controls_require_confirmed_session_and_back_only_detaches"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 720));
    app.global::<nova::Anim>().set_enabled(false);
    app.set_remote_controls_open(true);
    app.set_remote_target_name("Living room".into());
    app.set_remote_can_seek(true);
    app.set_remote_can_volume(true);
    app.set_remote_can_speed(true);
    app.set_remote_can_pause(true);
    app.window().show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_remote_action(move |action, value, token| {
        recorded
            .borrow_mut()
            .push((action.to_string(), value, token.to_string()))
    });
    app.on_toggle_pause(|| panic!("remote controls invoked local pause"));
    app.on_seek(|_| panic!("remote controls invoked local seek"));
    app.on_close_player(|| panic!("detaching remote controls stopped local playback"));
    key(&app, "k");
    assert!(actions.borrow().is_empty());
    app.set_remote_connected(true);
    key(&app, "k");
    assert!(
        actions.borrow().is_empty(),
        "connected transport is insufficient without a session"
    );
    app.set_remote_session("confirmed-session".into());
    app.set_remote_can_pause(false);
    key(&app, "k");
    assert!(
        actions.borrow().is_empty(),
        "speed support does not imply pause support"
    );
    app.set_remote_can_pause(true);
    app.set_remote_can_speed(false);
    key(&app, "k");
    key(&app, slint::platform::Key::RightArrow);
    key(&app, slint::platform::Key::UpArrow);
    assert_eq!(
        &*actions.borrow(),
        &[
            ("pause".into(), 0.0, "confirmed-session".into()),
            ("seek_relative".into(), 10.0, "confirmed-session".into()),
            ("volume_relative".into(), 5.0, "confirmed-session".into())
        ]
    );
    // Stop stays separate from routine transport/volume controls on narrow screens.
    app.window().set_size(slint::PhysicalSize::new(320, 720));
    app.set_remote_can_stop(true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let element = |id: &str| {
        ElementHandle::find_by_element_id(&app, id)
            .next()
            .unwrap_or_else(|| panic!("missing element {id}"))
    };
    let stop = element("RemoteControlsPage::stop_button");
    let header = element("RemoteControlsPage::controls_header");
    let timeline = element("RemoteControlsPage::timeline");
    let volume = element("RemoteControlsPage::volume_label");
    let stop_bottom = stop.absolute_position().y + stop.size().height;
    assert!(stop_bottom <= timeline.absolute_position().y);
    assert!(stop_bottom <= volume.absolute_position().y);
    assert!(stop.absolute_position().x >= header.absolute_position().x + header.size().width);
    assert!(stop.absolute_position().x > 160.0);
    assert!(stop.absolute_position().x + stop.size().width <= 320.0);
    click(&app, "Stop");
    assert_eq!(
        actions.borrow().last().unwrap(),
        &("stop".into(), 0.0, "confirmed-session".into())
    );
    // A receiver resume prompt distinguishes starting over from cancelling.
    app.window().set_size(slint::PhysicalSize::new(360, 1600));
    app.set_remote_resume_prompt(true);
    app.set_remote_prompt("Resume playback?".into());
    app.set_remote_prompt_token("resume-session".into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    click(&app, "Resume");
    click(&app, "Start over");
    click(&app, "Cancel");
    assert_eq!(
        &actions.borrow()[4..],
        &[
            ("prompt_accept".into(), 0.0, "resume-session".into()),
            ("prompt_reject".into(), 0.0, "resume-session".into()),
            ("prompt_cancel".into(), 0.0, "resume-session".into()),
        ]
    );
    let detached = Rc::new(std::cell::Cell::new(false));
    let recorded = detached.clone();
    app.on_remote_device_close(move || recorded.set(true));
    key(&app, slint::platform::Key::Escape);
    assert!(detached.get());
    assert_eq!(actions.borrow().len(), 7, "Back must not send Stop");
    assert!(
        !app.get_player_open(),
        "controller has no local player surface"
    );
}

#[test]
fn devices_cards_stay_compact_and_only_ready_sessions_open() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::devices_cards_stay_compact_and_only_ready_sessions_open"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_remote_devices_open(true);
    app.set_remote_devices(
        Rc::new(slint::VecModel::from(vec![
            nova::RemoteDeviceRow {
                id: "active".into(),
                name: "Living room".into(),
                status: "Available".into(),
                playing_title: "A film".into(),
                ready: true,
                has_session: true,
            },
            nova::RemoteDeviceRow {
                id: "idle".into(),
                name: "Bedroom".into(),
                status: "Available".into(),
                ready: true,
                ..Default::default()
            },
            nova::RemoteDeviceRow {
                id: "offline".into(),
                name: "Laptop".into(),
                status: "Open Nova to connect".into(),
                has_session: true,
                ..Default::default()
            },
        ]))
        .into(),
    );
    let opened = Rc::new(RefCell::new(Vec::new()));
    let recorded = opened.clone();
    app.on_remote_device_open(move |id| recorded.borrow_mut().push(id.to_string()));
    app.window().show().unwrap();
    for width in [320, 1200] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        let cards: Vec<_> =
            ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::device_card").collect();
        let names: Vec<_> =
            ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::device_name").collect();
        let statuses: Vec<_> =
            ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::device_status").collect();
        let buttons: Vec<_> =
            ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::device_controls").collect();
        assert_eq!(cards.len(), 3);
        for i in 0..3 {
            assert!(
                cards[i].size().height < 200.0,
                "device card stretched into spare height"
            );
            assert!(cards[i].absolute_position().x >= 0.0);
            assert!(cards[i].absolute_position().x + cards[i].size().width <= width as f32);
            let name_bottom = names[i].absolute_position().y + names[i].size().height;
            assert!(statuses[i].absolute_position().y - name_bottom < 10.0);
            assert_eq!(buttons[i].accessible_enabled(), Some(i == 0));
            buttons[i].mock_single_click(slint::platform::PointerEventButton::Left);
        }
        assert!(
            cards[1].absolute_position().y
                - cards[0].absolute_position().y
                - cards[0].size().height
                < 20.0
        );
    }
    assert_eq!(&*opened.borrow(), &["active", "active"]);
    key(&app, slint::platform::Key::Escape);
    assert!(!app.get_remote_devices_open());
}

#[test]
fn remote_pages_translate_and_fit_in_english_and_croatian() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::remote_pages_translate_and_fit_in_english_and_croatian"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(320, 1800));
    app.set_remote_target_name("Living room television with a long device name".into());
    app.set_remote_devices(
        Rc::new(slint::VecModel::from(vec![nova::RemoteDeviceRow {
            id: "receiver".into(),
            name: "Living room television with a long device name".into(),
            ready: true,
            has_session: true,
            ..Default::default()
        }]))
        .into(),
    );
    app.window().show().unwrap();
    let has = |label: &str| {
        assert!(
            ElementHandle::find_by_accessible_label(&app, label)
                .next()
                .is_some(),
            "missing translated label: {label}"
        );
    };
    for (language, devices, refresh, controls, connecting, disconnected, labels) in [
        (
            "en",
            "Devices",
            "Refresh",
            "Controls",
            "Connecting…",
            "Disconnected",
            ["Pause", "Mute", "Volume", "Audio tracks"],
        ),
        (
            "hr",
            "Uređaji",
            "Osvježite",
            "Kontrole",
            "Povezivanje…",
            "Veza je prekinuta",
            ["Pauzirajte", "Isključite zvuk", "Glasnoća", "Zvučni zapisi"],
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        app.set_remote_controls_open(false);
        app.set_remote_devices_open(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        has(devices);
        has(refresh);
        has(controls);
        app.set_remote_devices_open(false);
        app.set_remote_controls_open(true);
        app.set_remote_loading(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        has(connecting);
        app.set_remote_loading(false);
        app.set_remote_connected(false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        has(disconnected);
        app.set_remote_connected(true);
        app.set_remote_session("session".into());
        app.set_remote_can_tracks(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        for label in labels {
            has(label);
        }
        has(controls);
        for button in i_slint_backend_testing::ElementQuery::from_root(&app)
            .find_all()
            .into_iter()
            .filter(|item| item.accessible_role() == Some(AccessibleRole::Button))
        {
            assert!(button.absolute_position().x >= 0.0);
            assert!(
                button.absolute_position().x + button.size().width <= 320.5,
                "{language} button overflows: {:?}",
                button.accessible_label()
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn track_picker_dispatches_only_its_confirmed_session_and_back_closes_picker_first() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::track_picker_dispatches_only_its_confirmed_session_and_back_closes_picker_first"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 1600));
    app.set_remote_controls_open(true);
    app.set_remote_connected(true);
    app.set_remote_session("session-a".into());
    app.set_remote_can_tracks(true);
    app.set_remote_audio_tracks(
        Rc::new(slint::VecModel::from(vec![nova::TrackRow {
            index: 4,
            title: "English stereo".into(),
            selected: true,
            ..Default::default()
        }]))
        .into(),
    );
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_remote_action(move |name, value, token| {
        recorded
            .borrow_mut()
            .push((name.to_string(), value, token.to_string()))
    });
    let detached = Rc::new(std::cell::Cell::new(false));
    let recorded = detached.clone();
    app.on_remote_device_close(move || recorded.set(true));
    app.window().show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "English stereo")
            .next()
            .is_none(),
        "track choices must stay inside the picker"
    );
    click(&app, "Audio tracks");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    click(&app, "English stereo");
    assert_eq!(
        &*actions.borrow(),
        &[("audio".into(), 4.0, "session-a".into())]
    );
    click(&app, "Subtitles");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    key(&app, slint::platform::Key::Escape);
    assert!(
        !detached.get(),
        "Back must dismiss the picker before controls"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Off")
            .next()
            .is_none()
    );
    click(&app, "Audio tracks");
    app.set_remote_session("session-b".into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "English stereo")
            .next()
            .is_none(),
        "session replacement dismisses stale tracks"
    );
    assert_eq!(actions.borrow().len(), 1);
    key(&app, slint::platform::Key::Escape);
    assert!(detached.get());
}

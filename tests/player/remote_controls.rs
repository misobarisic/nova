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
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Playback tools")
            .next()
            .is_none(),
        "legacy-only receivers must not offer an empty tools menu"
    );
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
    assert!(stop.absolute_position().y >= timeline.absolute_position().y + timeline.size().height);
    assert!(stop.absolute_position().y >= volume.absolute_position().y + volume.size().height);
    assert!(stop.absolute_position().y > header.absolute_position().y + header.size().height);
    assert!(stop.absolute_position().x >= 0.0);
    assert!(stop.absolute_position().x + stop.size().width <= 320.0);
    // Stop follows the compact playback panels. Reach it through the real
    // viewport, including on phones whose footer initially needs scrolling.
    let scroll = element("RemoteControlsPage::controls_scroll");
    let viewport_top = scroll.absolute_position().y;
    let viewport_bottom = viewport_top + scroll.size().height;
    for _ in 0..6 {
        let top = stop.absolute_position().y;
        let bottom = top + stop.size().height;
        if top >= viewport_top && bottom <= viewport_bottom {
            break;
        }
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: LogicalPosition::new(
                    scroll.absolute_position().x + 2.0,
                    (viewport_top + viewport_bottom) / 2.0,
                ),
                delta_x: 0.0,
                delta_y: if top < viewport_top { 240.0 } else { -240.0 },
            });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    }
    assert!(stop.absolute_position().y >= viewport_top);
    assert!(
        stop.absolute_position().y + stop.size().height <= viewport_bottom,
        "the separate Stop action must be reachable by scrolling"
    );
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
    // Desktop selection stays bound to a peer when discovery reorders the rows.
    let offline = ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::device_card")
        .find(|item| item.accessible_label().as_deref() == Some("Laptop"))
        .unwrap();
    offline.mock_single_click(slint::platform::PointerEventButton::Left);
    let selected = || {
        ElementHandle::find_by_element_id(&app, "RemoteDevicesPage::selected_device_name")
            .next()
            .unwrap()
            .accessible_label()
    };
    assert_eq!(selected().as_deref(), Some("Laptop"));
    app.set_remote_devices(
        Rc::new(slint::VecModel::from(vec![
            nova::RemoteDeviceRow {
                id: "offline".into(),
                name: "Laptop".into(),
                ..Default::default()
            },
            nova::RemoteDeviceRow {
                id: "idle".into(),
                name: "Bedroom".into(),
                ..Default::default()
            },
            nova::RemoteDeviceRow {
                id: "active".into(),
                name: "Living room".into(),
                ..Default::default()
            },
        ]))
        .into(),
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert_eq!(selected().as_deref(), Some("Laptop"));
    assert_eq!(
        opened.borrow().len(),
        2,
        "selecting a peer must not open controls"
    );
    key(&app, slint::platform::Key::Escape);
    assert!(!app.get_remote_devices_open());
}

#[test]
fn compact_controls_keep_secondary_actions_and_captured_preview_session() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::compact_controls_keep_secondary_actions_and_captured_preview_session"
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
    app.set_remote_can_speed(true);
    app.set_remote_can_fullscreen(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_remote_action(move |name, value, token| {
        recorded
            .borrow_mut()
            .push((name.to_string(), value, token.to_string()))
    });
    app.window().show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_element_id(&app, "RemoteControlsPage::reset_speed")
            .next()
            .is_none()
    );
    let speed_toggle = ElementHandle::find_by_element_id(&app, "RemoteControlsPage::speed_toggle")
        .next()
        .unwrap();
    speed_toggle.mock_single_click(slint::platform::PointerEventButton::Left);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    click(&app, "Use global default");
    let more_toggle = ElementHandle::find_by_element_id(&app, "RemoteControlsPage::more_toggle")
        .next()
        .unwrap();
    more_toggle.mock_single_click(slint::platform::PointerEventButton::Left);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    click(&app, "Fullscreen on device");
    let hold = ElementHandle::find_by_element_id(&app, "RemoteControlsPage::speed_hold")
        .next()
        .unwrap();
    let p = hold.absolute_position();
    let size = hold.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.set_remote_session("session-b".into());
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    assert_eq!(
        &*actions.borrow(),
        &[
            ("reset_speed".into(), 0.0, "session-a".into()),
            ("fullscreen".into(), 0.0, "session-a".into()),
            ("speed_preview".into(), 2.0, "session-a".into()),
            ("speed_preview_end".into(), 0.0, "session-a".into()),
        ]
    );
    app.window().set_size(slint::PhysicalSize::new(1200, 1000));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let transport = ElementHandle::find_by_element_id(&app, "RemoteControlsPage::transport_panel")
        .next()
        .unwrap();
    let volume = ElementHandle::find_by_element_id(&app, "RemoteControlsPage::volume_label")
        .next()
        .unwrap();
    assert!(
        transport.absolute_position().x + transport.size().width <= volume.absolute_position().x
    );
    assert!(
        volume.absolute_position().y < transport.absolute_position().y + transport.size().height
    );
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

fn settle_remote() {
    for _ in 0..5 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}

#[test]
fn grouped_tools_route_semantic_actions_and_bookmark_drafts_wait_for_matching_ack() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::grouped_tools_route_semantic_actions_and_bookmark_drafts_wait_for_matching_ack"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(390, 1800));
    app.set_remote_controls_open(true);
    app.set_remote_connected(true);
    app.set_remote_session("receiver-a".into());
    app.set_remote_can_chapters(true);
    app.set_remote_can_subtitle_style(true);
    app.set_remote_can_bookmarks(true);
    app.set_remote_can_secondary_subtitles(true);
    app.set_remote_chapters(
        Rc::new(slint::VecModel::from(vec![nova::RemoteChapterRow {
            id: "chapter-stable".into(),
            title: "Opening scene".into(),
            time: "00:42".into(),
        }]))
        .into(),
    );
    app.set_remote_bookmarks(
        Rc::new(slint::VecModel::from(vec![nova::RemoteBookmarkRow {
            id: "bookmark-stable".into(),
            note: "Keep this note".into(),
            time: "03:21".into(),
        }]))
        .into(),
    );
    let calls = Rc::new(RefCell::new(Vec::new()));
    let recorded = calls.clone();
    app.on_remote_semantic_action(move |name, value, id, text, session| {
        recorded.borrow_mut().push((
            name.to_string(),
            value,
            id.to_string(),
            text.to_string(),
            session.to_string(),
        ));
    });
    let detached = Rc::new(std::cell::Cell::new(false));
    let recorded = detached.clone();
    app.on_remote_device_close(move || recorded.set(true));
    app.show().unwrap();
    settle_remote();
    click(&app, "Playback tools");
    settle_remote();
    for (width, height) in [(320, 844), (390, 844), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.window()
            .dispatch_event(slint::platform::WindowEvent::Resized {
                size: slint::LogicalSize::new(width as f32, height as f32),
            });
        settle_remote();
        for label in ["Chapters", "Secondary subtitles", "Scene bookmarks"] {
            let row = ElementHandle::find_by_accessible_label(&app, label)
                .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
                .unwrap();
            assert!(row.absolute_position().x >= 0.0);
            assert!(row.absolute_position().x + row.size().width <= width as f32 + 1.0);
            assert!(row.size().height >= 44.0);
        }
    }
    app.window().set_size(slint::PhysicalSize::new(390, 1800));
    settle_remote();
    click(&app, "Chapters");
    settle_remote();
    click(&app, "Opening scene");
    assert_eq!(
        calls.borrow().last().unwrap(),
        &(
            "chapter".into(),
            0.0,
            "chapter-stable".into(),
            "".into(),
            "receiver-a".into()
        )
    );
    key(&app, slint::platform::Key::Escape);
    settle_remote();
    click(&app, "Secondary subtitles");
    settle_remote();
    click(&app, "Off");
    assert_eq!(calls.borrow().last().unwrap().0, "secondary_subtitle");
    assert_eq!(calls.borrow().last().unwrap().1, -1.0);
    key(&app, slint::platform::Key::Escape);
    settle_remote();
    click(&app, "Subtitle appearance");
    settle_remote();
    let background =
        ElementHandle::find_by_accessible_label(&app, "Increase Background opacity (%)")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
    assert!(background.size().width >= 44.0 && background.size().height >= 44.0);
    click(&app, "Increase Background opacity (%)");
    assert_eq!(
        calls.borrow().last().unwrap(),
        &(
            "subtitle_style".into(),
            46.0,
            "7".into(),
            "".into(),
            "receiver-a".into()
        )
    );
    app.global::<nova::InterfaceStyle>().set_tv(true);
    settle_remote();
    // Tool pages begin with no row selected; eight Down presses reach field7.
    for _ in 0..8 {
        key(&app, slint::platform::Key::DownArrow);
    }
    settle_remote();
    let before_keyboard_backdrop = calls.borrow().len();
    key(&app, slint::platform::Key::RightArrow);
    assert_eq!(calls.borrow().len(), before_keyboard_backdrop + 1);
    assert_eq!(
        calls.borrow().last().unwrap(),
        &(
            "subtitle_style".into(),
            46.0,
            "7".into(),
            "".into(),
            "receiver-a".into()
        )
    );
    key(&app, slint::platform::Key::Escape);
    settle_remote();
    assert_eq!(app.global::<nova::RemoteBridge>().get_menu_page(), "tools");
    app.global::<nova::InterfaceStyle>().set_tv(false);
    settle_remote();
    click(&app, "Scene bookmarks");
    settle_remote();
    click(&app, "Edit note");
    settle_remote();
    click(&app, "Clear note");
    settle_remote();
    let bridge = app.global::<nova::RemoteBridge>();
    assert_eq!(bridge.get_menu_page(), "bookmark-editor");
    let call = calls.borrow().last().unwrap().clone();
    assert_eq!(
        call,
        (
            "bookmark_edit".into(),
            0.0,
            "bookmark-stable".into(),
            "".into(),
            "receiver-a".into()
        )
    );
    // A failed clear preserves the pre-existing draft for a real retry.
    app.set_remote_semantic_name("bookmark_edit".into());
    app.set_remote_semantic_id("bookmark-stable".into());
    app.set_remote_semantic_session("receiver-a".into());
    app.set_remote_semantic_success(false);
    app.set_remote_semantic_notice("Device could not save the note.".into());
    app.set_remote_semantic_revision(1);
    settle_remote();
    let note = ElementHandle::find_by_accessible_label(&app, "Bookmark note")
        .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
        .unwrap();
    assert_eq!(note.accessible_value().unwrap(), "Keep this note");
    assert_eq!(bridge.get_menu_page(), "bookmark-editor");
    click(&app, "Save note");
    settle_remote();
    assert_eq!(calls.borrow().last().unwrap().3, "Keep this note");
    // A newer acknowledgement for another action never closes this editor.
    app.set_remote_semantic_name("chapter".into());
    app.set_remote_semantic_id("chapter-stable".into());
    app.set_remote_semantic_success(true);
    app.set_remote_semantic_revision(2);
    settle_remote();
    assert_eq!(bridge.get_menu_page(), "bookmark-editor");
    app.set_remote_semantic_name("bookmark_edit".into());
    app.set_remote_semantic_id("bookmark-stable".into());
    app.set_remote_semantic_revision(3);
    settle_remote();
    assert_eq!(bridge.get_menu_page(), "bookmarks");
    key(&app, slint::platform::Key::Escape);
    settle_remote();
    assert_eq!(bridge.get_menu_page(), "tools");
    key(&app, slint::platform::Key::Escape);
    settle_remote();
    assert_eq!(bridge.get_menu_page(), "");
    assert!(!detached.get(), "nearest Back remains inside grouped tools");
    key(&app, slint::platform::Key::Escape);
    assert!(detached.get());
}

// Slint evaluates changed bindings and mounts their timers during an event
// turn. One large clock jump can start an exit timer at the *end* of that jump.
// Advance rendered frames so both timer installation and its deadline run,
// matching the production event loop while retaining the full elapsed bound.
fn advance_remote_motion(app: &nova::AppWindow, millis: u64) {
    for _ in 0..millis / 20 {
        if let Some(page) =
            ElementHandle::find_by_element_type_name(app, "RemoteControlsPage").next()
        {
            let _ = (page.size(), page.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

#[test]
fn controls_retain_outgoing_snapshot_disable_input_and_cancel_stale_exit_on_reopen() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::controls_retain_outgoing_snapshot_disable_input_and_cancel_stale_exit_on_reopen"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    let anim = app.global::<nova::Anim>();
    anim.set_enabled(true);
    anim.set_transitions(true);
    app.set_remote_title("Retained receiver title".into());
    app.set_remote_connected(true);
    app.set_remote_session("session-a".into());
    app.set_remote_can_pause(true);
    let calls = Rc::new(std::cell::Cell::new(0));
    let recorded = calls.clone();
    app.on_remote_action(move |_, _, _| recorded.set(recorded.get() + 1));
    let closed = Rc::new(std::cell::Cell::new(0));
    let recorded = closed.clone();
    app.on_remote_controls_visual_closed(move || recorded.set(recorded.get() + 1));
    app.show().unwrap();
    app.set_remote_controls_open(true);
    settle_remote();
    assert!(app.get_remote_controls_visual_retained());
    let page = || ElementHandle::find_by_element_type_name(&app, "RemoteControlsPage").next();
    assert!(page().is_some());
    app.set_remote_controls_open(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    assert!(page().is_some(), "outgoing page must remain mounted");
    assert!(app.get_remote_controls_visual_retained());
    assert_eq!(app.get_remote_title(), "Retained receiver title");
    key(&app, "k");
    assert_eq!(calls.get(), 0);
    assert_eq!(closed.get(), 0);
    app.set_remote_controls_open(true);
    advance_remote_motion(&app, 800);
    assert!(page().is_some());
    assert_eq!(closed.get(), 0);
    app.set_remote_controls_open(false);
    advance_remote_motion(&app, 800);
    assert!(page().is_none());
    assert_eq!(closed.get(), 1);
    assert!(!app.get_remote_controls_visual_retained());
    app.set_remote_controls_open(true);
    settle_remote();
    app.set_remote_controls_open(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    anim.set_enabled(false);
    settle_remote();
    assert!(page().is_none(), "disabling motion finishes an active exit");
    assert_eq!(closed.get(), 2);
    app.set_remote_controls_open(true);
    settle_remote();
    app.set_remote_controls_open(false);
    settle_remote();
    assert!(page().is_none(), "reduced-motion navigation is immediate");
    assert_eq!(closed.get(), 3);
}

#[test]
fn tv_stream_offers_reveal_the_action_and_description_in_both_directions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_stream_offers_reveal_the_action_and_description_in_both_directions"
    )) {
        return;
    }
    use slint::{VecModel, platform::Key};
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_remote_connected(true);
    app.set_remote_session("offer-session".into());
    app.set_remote_offers(
        Rc::new(VecModel::from(
            (0..12)
                .map(|index| nova::RemoteOfferRow {
                    id: format!("offer-{index}").into(),
                    title: format!("Stream offer {index}").into(),
                    description: format!(
                        "Release {index}: 1080p with multiple audio and subtitle tracks. \
                         This description wraps across lines so the selected source details \
                         must be revealed along with its action."
                    )
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let picked = Rc::new(RefCell::new(Vec::new()));
    let record = picked.clone();
    app.on_remote_stream_picked(move |index| record.borrow_mut().push(index));
    let backs = Rc::new(std::cell::Cell::new(0));
    let record = backs.clone();
    app.on_remote_device_close(move || record.set(record.get() + 1));
    app.window().show().unwrap();
    let mut expected = Vec::new();
    for (width, height) in [(640, 480), (1280, 720)] {
        app.set_remote_controls_open(false);
        settle_remote();
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_remote_controls_open(true);
        settle_remote();
        let viewport =
            ElementHandle::find_by_element_id(&app, "RemoteControlsPage::controls_scroll")
                .next()
                .unwrap();
        let assert_revealed = |index: i32| {
            let title = format!("Stream offer {index}");
            let row = ElementHandle::find_by_accessible_label(&app, &title)
                .find(|item| item.accessible_role() == Some(AccessibleRole::Groupbox))
                .unwrap_or_else(|| panic!("selected offer {index} must be accessible"));
            let top = row.absolute_position().y;
            let bottom = top + row.size().height;
            let viewport_top = viewport.absolute_position().y;
            let viewport_bottom = viewport_top + viewport.size().height;
            assert!(
                top >= viewport_top - 0.5 && bottom <= viewport_bottom + 0.5,
                "offer {index} including description must fit at {width}x{height}: \
                 {top}..{bottom}, viewport {viewport_top}..{viewport_bottom}"
            );
        };
        for index in 0..12 {
            key(&app, Key::DownArrow);
            settle_remote();
            assert_revealed(index);
            key(&app, Key::Return);
            assert_eq!(picked.borrow().last(), Some(&index));
            expected.push(index);
        }
        for index in (0..11).rev() {
            key(&app, Key::UpArrow);
            settle_remote();
            assert_revealed(index);
            key(&app, Key::Return);
            assert_eq!(picked.borrow().last(), Some(&index));
            expected.push(index);
        }
    }
    assert_eq!(*picked.borrow(), expected);
    key(&app, Key::Escape);
    assert_eq!(backs.get(), 1, "Back from an offer detaches its controls");
}

#[test]
fn tv_long_stream_description_pages_before_leaving_its_visible_action() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_long_stream_description_pages_before_leaving_its_visible_action"
    )) {
        return;
    }
    use slint::{VecModel, platform::Key};
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_remote_connected(true);
    app.set_remote_session("long-offer-session".into());
    app.set_remote_offers(
        Rc::new(VecModel::from(vec![
            nova::RemoteOfferRow {
                id: "previous".into(),
                title: "Previous offer".into(),
                ..Default::default()
            },
            nova::RemoteOfferRow {
                id: "long".into(),
                title: "Long description offer".into(),
                description:
                    "Release details with audio languages, subtitle tracks and source metadata. "
                        .repeat(300)
                        .into(),
            },
            nova::RemoteOfferRow {
                id: "next".into(),
                title: "Next offer".into(),
                ..Default::default()
            },
        ]))
        .into(),
    );
    let picked = Rc::new(RefCell::new(Vec::new()));
    let record = picked.clone();
    app.on_remote_stream_picked(move |index| record.borrow_mut().push(index));
    app.window().show().unwrap();
    for (width, height) in [(640, 480), (1280, 720)] {
        app.set_remote_controls_open(false);
        settle_remote();
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_remote_controls_open(true);
        settle_remote();
        key(&app, Key::DownArrow);
        settle_remote();
        key(&app, Key::DownArrow);
        settle_remote();
        let action = ElementHandle::find_by_accessible_label(&app, "Long description offer")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        let description =
            ElementHandle::find_by_element_id(&app, "RemoteStreamOffer::offer_description")
                .max_by(|left, right| {
                    left.size()
                        .height
                        .partial_cmp(&right.size().height)
                        .unwrap()
                })
                .unwrap();
        let viewport =
            ElementHandle::find_by_element_id(&app, "RemoteStreamOffer::description_scroll")
                .max_by(|left, right| {
                    left.size()
                        .height
                        .partial_cmp(&right.size().height)
                        .unwrap()
                })
                .unwrap();
        assert!(
            (viewport.absolute_position().y
                - action.absolute_position().y
                - action.size().height
                - 4.0)
                .abs()
                < 0.5,
            "description viewport y={} must follow action y={}, height={} by 4px",
            viewport.absolute_position().y,
            action.absolute_position().y,
            action.size().height
        );
        assert!(description.size().height > viewport.size().height * 2.0);
        let assert_visible_action = || {
            let top = action.absolute_position().y;
            assert!(
                top >= 18.0 - 0.5 && top + action.size().height <= height as f32 - 18.0 + 0.5,
                "long offer action must remain visible at {width}x{height}"
            );
        };
        assert_visible_action();
        assert!((description.absolute_position().y - viewport.absolute_position().y).abs() < 0.5);
        let mut pages = 0;
        while description.absolute_position().y + description.size().height
            > viewport.absolute_position().y + viewport.size().height + 0.5
        {
            let previous = description.absolute_position().y;
            key(&app, Key::DownArrow);
            settle_remote();
            assert!(description.absolute_position().y < previous - 0.5);
            assert_visible_action();
            key(&app, Key::Return);
            assert_eq!(picked.borrow().last(), Some(&1), "paging retains the offer");
            pages += 1;
            assert!(pages < 100, "the full description must be reachable");
        }
        assert!(pages > 1);
        key(&app, Key::DownArrow);
        settle_remote();
        key(&app, Key::Return);
        assert_eq!(
            picked.borrow().last(),
            Some(&2),
            "Down leaves after the last page"
        );

        key(&app, Key::UpArrow);
        settle_remote();
        assert_visible_action();
        assert!(
            (description.absolute_position().y + description.size().height
                - viewport.absolute_position().y
                - viewport.size().height)
                .abs()
                < 0.5
        );
        let mut reverse_pages = 0;
        while description.absolute_position().y < viewport.absolute_position().y - 0.5 {
            let previous = description.absolute_position().y;
            key(&app, Key::UpArrow);
            settle_remote();
            assert!(description.absolute_position().y > previous + 0.5);
            assert_visible_action();
            key(&app, Key::Return);
            assert_eq!(picked.borrow().last(), Some(&1));
            reverse_pages += 1;
            assert!(reverse_pages < 100);
        }
        assert!(reverse_pages > 1);
        key(&app, Key::UpArrow);
        settle_remote();
        key(&app, Key::Return);
        assert_eq!(
            picked.borrow().last(),
            Some(&0),
            "Up leaves after the first page"
        );
    }
}

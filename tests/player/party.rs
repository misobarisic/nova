//! Watch-room controls and responsive, translated UI without a network or player.

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, Model};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
}

// App-level snapshot publication derives these capabilities from role and policy.
fn set_host(app: &nova::AppWindow, host: bool) {
    app.set_party_is_host(host);
    app.set_party_can_control(host);
    app.set_party_can_choose_media(host);
}

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

#[test]
fn room_actions_are_guarded_and_guests_have_no_shared_timeline_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::room_actions_are_guarded_and_guests_have_no_shared_timeline_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 1800));
    app.set_party_can_host(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_party_create(move || recorded.borrow_mut().push("create".to_string()));
    let recorded = actions.clone();
    app.on_party_join(move |invite| recorded.borrow_mut().push(format!("join:{invite}")));
    let recorded = actions.clone();
    app.on_party_set_paused(move |paused| recorded.borrow_mut().push(format!("pause:{paused}")));
    let recorded = actions.clone();
    app.on_party_choose_stream(move || recorded.borrow_mut().push("stream".into()));
    app.on_toggle_pause(|| panic!("room button invoked the local player directly"));
    app.window().show().unwrap();
    settle();
    app.set_party_page_open(true);
    settle();
    assert!(!app.get_player_open(), "a party can start before playback");
    assert_eq!(
        button(&app, "Create party").accessible_enabled(),
        Some(true)
    );
    app.set_party_can_host(false);
    settle();
    assert_eq!(
        button(&app, "Create party").accessible_enabled(),
        Some(true),
        "creation does not require a seekable local source"
    );
    app.set_party_busy(true);
    settle();
    assert_eq!(
        button(&app, "Create party").accessible_enabled(),
        Some(false)
    );
    app.set_party_busy(false);
    settle();
    button(&app, "Create party").mock_single_click(slint::platform::PointerEventButton::Left);
    app.set_party_input("NVP2-private-test-invite".into());
    settle();
    button(&app, "Join party").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        &*actions.borrow(),
        &["create", "join:NVP2-private-test-invite"]
    );
    app.set_party_active(true);
    app.set_party_has_media(true);
    app.set_party_duration(3600.0);
    app.set_party_title("A film".into());
    settle();
    let timeline = || {
        ElementHandle::find_by_element_id(&app, "PartyPage::party_timeline")
            .next()
            .unwrap()
    };
    assert_eq!(timeline().accessible_enabled(), Some(false));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Pause")
            .next()
            .is_none()
    );
    button(&app, "Choose a stream").mock_single_click(slint::platform::PointerEventButton::Left);
    set_host(&app, true);
    settle();
    assert_eq!(timeline().accessible_enabled(), Some(true));
    button(&app, "Pause").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(&actions.borrow()[2..], &["stream", "pause:true"]);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Escape.into(),
        });
    assert!(!app.get_party_page_open());
    assert!(app.get_party_active(), "Back keeps the party connected");
    app.set_player_open(true);
    app.set_playback_started(true);
    set_host(&app, false);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Host controls playback")
            .next()
            .is_some()
    );
    app.set_party_error("Only the host controls the shared timeline.".into());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(
            &app,
            "Only the host controls the shared timeline."
        )
        .next()
        .is_some()
    );
    button(&app, "Open watch party").mock_single_click(slint::platform::PointerEventButton::Left);
    assert!(app.get_party_page_open());
}

#[test]
fn room_translates_and_keeps_long_participants_inside_portrait_and_landscape() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::room_translates_and_keeps_long_participants_inside_portrait_and_landscape"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_party_page_open(true);
    app.set_party_active(true);
    set_host(&app, true);
    app.set_party_has_media(true);
    app.set_party_can_return_to_player(true);
    app.set_party_waiting(true);
    app.set_party_invite("NVP1-test-invite".into());
    app.set_party_invite_qr(slint::Image::from_rgb8(slint::SharedPixelBuffer::<
        slint::Rgb8Pixel,
    >::new(8, 8)));
    app.set_party_title("A very long film title that must wrap inside a phone screen".into());
    app.set_party_participants(
        Rc::new(slint::VecModel::from(vec![nova_ui::PartyMemberRow {
            id: "guest".into(),
            name: "A guest with a long device name that needs more than one line".into(),
            initial: "A".into(),
            status: "Choosing a stream".into(),
            can_remove: true,
            ..Default::default()
        }]))
        .into(),
    );
    app.window().show().unwrap();
    for (language, title, leave, invite, choose, change) in [
        (
            "en",
            "Watch party",
            "End party",
            "Copy invite",
            "Choose what to watch",
            "Change what to watch",
        ),
        (
            "hr",
            "Zajedničko gledanje",
            "Završite zajedničko gledanje",
            "Kopirajte pozivnicu",
            "Odaberite što ćete gledati",
            "Promijenite što ćete gledati",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for has_media in [false, true] {
            app.set_party_has_media(has_media);
            for (width, height) in [(320, 1800), (393, 820), (840, 380), (1280, 800)] {
                app.window()
                    .set_size(slint::PhysicalSize::new(width, height));
                settle();
                let scroll = |delta_y: f32| {
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(
                                width as f32 / 2.0,
                                height as f32 / 2.0,
                            ),
                            delta_x: 0.0,
                            delta_y,
                        });
                    settle();
                };
                // Each viewport starts at the header. Short landscape screens must
                // reveal the footer by scrolling, just as a person reaches it.
                scroll(10000.0);
                assert!(
                    ElementHandle::find_by_accessible_label(&app, title)
                        .next()
                        .is_some()
                );
                let mut saw_leave = false;
                let mut saw_invite = false;
                let mut saw_participant = false;
                let mut saw_choice = false;
                let mut saw_qr = false;
                for _ in 0..16 {
                    if let Some(qr) =
                        ElementHandle::find_by_element_id(&app, "PartyPage::party_invite_qr").next()
                    {
                        saw_qr = true;
                        assert!((qr.size().width - qr.size().height).abs() < 0.5);
                        assert!(qr.size().width > 0.0 && qr.size().width <= 232.5);
                        assert!(qr.absolute_position().x >= 0.0);
                        assert!(qr.absolute_position().x + qr.size().width <= width as f32 + 0.5);
                        if width < 900 {
                            assert!(qr.absolute_position().x >= 33.5);
                            assert!(
                                qr.absolute_position().x + qr.size().width <= width as f32 - 33.5,
                                "QR must stay within its card padding"
                            );
                        }
                    }
                    for item in i_slint_backend_testing::ElementQuery::from_root(&app)
                        .find_all()
                        .into_iter()
                        .filter(|item| item.accessible_role() == Some(AccessibleRole::Button))
                    {
                        saw_leave |= item.accessible_label().as_deref() == Some(leave);
                        saw_invite |= item.accessible_label().as_deref() == Some(invite);
                        saw_choice |= item.accessible_label().as_deref()
                            == Some(if has_media { change } else { choose });
                        assert!(
                            item.absolute_position().x >= 0.0,
                            "{language}: {:?}",
                            item.accessible_label()
                        );
                        assert!(
                            item.absolute_position().x + item.size().width <= width as f32 + 0.5,
                            "{language} button overflows: {:?}",
                            item.accessible_label()
                        );
                    }
                    if let Some(participant) =
                        ElementHandle::find_by_element_id(&app, "PartyPage::participant").next()
                    {
                        saw_participant = true;
                        if width < 900 {
                            assert!(
                                (participant.size().width - (width as f32 - 68.0)).abs() < 1.0,
                                "{language} {width}x{height}: phone panels must fill the scroll viewport, participant={:?}/{:?}",
                                participant.absolute_position(),
                                participant.size()
                            );
                        }
                        assert!(
                            participant.size().height < 220.0,
                            "participant stretched into spare height"
                        );
                    }
                    if saw_leave && saw_invite && saw_participant && saw_choice && saw_qr {
                        break;
                    }
                    scroll(-(height as f32 * 0.6));
                }
                assert!(
                    saw_choice,
                    "{language} {width}×{height}: cannot reach media choice"
                );
                assert!(saw_qr, "host invite includes its QR code");
                assert!(
                    saw_leave,
                    "{language} {width}×{height}: cannot scroll to End party"
                );
                assert!(
                    saw_invite,
                    "{language} {width}×{height}: cannot scroll to Copy invite"
                );
                assert!(
                    saw_participant,
                    "{language} {width}×{height}: cannot scroll to participant"
                );
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    set_host(&app, false);
    settle();
    assert!(
        ElementHandle::find_by_element_id(&app, "PartyPage::party_invite_qr")
            .next()
            .is_none()
    );
}

#[test]
fn connecting_badge_reopens_setup_and_cancel_always_dispatches_leave() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::connecting_badge_reopens_setup_and_cancel_always_dispatches_leave"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 720));
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_party_busy(true);
    app.set_party_can_host(false);
    let cancelled = Rc::new(std::cell::Cell::new(0));
    let recorded = cancelled.clone();
    app.on_party_leave(move || recorded.set(recorded.get() + 1));
    app.window().show().unwrap();
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Connecting…")
            .next()
            .is_some()
    );
    button(&app, "Open watch party").mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
    assert!(app.get_party_page_open());
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Escape.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Escape.into(),
        });
    settle();
    assert!(!app.get_party_page_open());
    assert!(app.get_party_busy());
    button(&app, "Open watch party").mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
    let cancel = button(&app, "Cancel");
    assert_eq!(cancel.accessible_enabled(), Some(true));
    cancel.mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(cancelled.get(), 1);
}

#[test]
fn lobby_keeps_invites_and_people_while_host_chooses_or_changes_media() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::lobby_keeps_invites_and_people_while_host_chooses_or_changes_media"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 1800));
    app.set_party_page_open(true);
    app.set_party_active(true);
    set_host(&app, true);
    app.set_party_invite("NVP1-lobby-invite".into());
    app.set_party_participants(
        Rc::new(slint::VecModel::from(vec![nova_ui::PartyMemberRow {
            id: "guest".into(),
            name: "A friend".into(),
            status: "Ready".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_party_choose_media(move || recorded.borrow_mut().push("media"));
    let recorded = actions.clone();
    app.on_party_choose_stream(move || recorded.borrow_mut().push("stream"));
    let recorded = actions.clone();
    app.on_party_copy_invite(move || recorded.borrow_mut().push("invite"));
    app.window().show().unwrap();
    settle();
    for absent in [
        "Now watching",
        "Playback position",
        "Choose a stream",
        "Return to player",
        "Pause",
    ] {
        assert!(
            ElementHandle::find_by_accessible_label(&app, absent)
                .next()
                .is_none(),
            "lobby exposes {absent}"
        );
    }
    assert!(
        ElementHandle::find_by_accessible_label(&app, "A friend")
            .next()
            .is_some()
    );
    button(&app, "Copy invite").mock_single_click(slint::platform::PointerEventButton::Left);
    button(&app, "Choose what to watch")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(&*actions.borrow(), &["invite", "media"]);

    set_host(&app, false);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(
            &app,
            "Waiting for the host to choose what to watch."
        )
        .next()
        .is_some()
    );
    for absent in [
        "Choose what to watch",
        "Change what to watch",
        "Choose a stream",
        "Playback position",
        "Return to player",
    ] {
        assert!(
            ElementHandle::find_by_accessible_label(&app, absent)
                .next()
                .is_none(),
            "guest lobby exposes {absent}"
        );
    }
    assert!(
        ElementHandle::find_by_accessible_label(&app, "A friend")
            .next()
            .is_some()
    );

    set_host(&app, true);
    app.set_party_has_media(true);
    app.set_party_title("The next film".into());
    app.set_party_duration(7200.0);
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "The next film")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Return to player")
            .next()
            .is_none()
    );
    button(&app, "Change what to watch")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    button(&app, "Choose a stream").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(&*actions.borrow(), &["invite", "media", "media", "stream"]);
    assert_eq!(
        app.get_party_participants().row_count(),
        1,
        "changing media preserves people"
    );
    app.set_party_can_return_to_player(true);
    settle();
    assert_eq!(
        button(&app, "Return to player").accessible_enabled(),
        Some(true)
    );
    app.set_party_busy(true);
    settle();
    assert_eq!(
        button(&app, "Change what to watch").accessible_enabled(),
        Some(false)
    );
}

#[test]
fn room_places_people_beside_desktop_playback_and_below_phone_playback() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::room_places_people_beside_desktop_playback_and_below_phone_playback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_party_page_open(true);
    app.set_party_active(true);
    app.set_party_has_media(true);
    app.set_party_can_return_to_player(true);
    app.set_party_duration(3600.0);
    app.set_party_title("A film to watch together".into());
    app.set_party_participants(
        Rc::new(slint::VecModel::from(vec![nova_ui::PartyMemberRow {
            id: "guest".into(),
            name: "A friend".into(),
            initial: "A".into(),
            status: "Ready".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.window().show().unwrap();
    for host in [true, false] {
        set_host(&app, host);
        for width in [360, 840, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            settle();
            let timeline = ElementHandle::find_by_element_id(&app, "PartyPage::party_timeline")
                .next()
                .expect("room timeline");
            let participant = ElementHandle::find_by_element_id(&app, "PartyPage::participant")
                .next()
                .expect("room participant");
            let stream = button(&app, "Choose a stream");
            if width >= 900 {
                assert!(
                    participant.absolute_position().x
                        > timeline.absolute_position().x + timeline.size().width,
                    "desktop people occupy a separate column (host={host})"
                );
                assert!(
                    participant.absolute_position().y < stream.absolute_position().y,
                    "desktop sidebar starts beside the playback panel"
                );
            } else {
                assert!(
                    participant.absolute_position().y
                        > stream.absolute_position().y + stream.size().height,
                    "narrow people follow the playback panel (host={host}, width={width})"
                );
                assert!(
                    (participant.absolute_position().x - timeline.absolute_position().x).abs()
                        < 1.0,
                    "stacked panels share their content inset"
                );
            }
            for item in [&timeline, &participant, &stream] {
                assert!(item.absolute_position().x >= 0.0);
                assert!(item.absolute_position().x + item.size().width <= width as f32);
            }
            assert_eq!(timeline.accessible_enabled(), Some(host));
        }
    }
}

#[test]
fn room_tv_focus_visits_return_before_stream_actions_for_hosts_and_guests() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::room_tv_focus_visits_return_before_stream_actions_for_hosts_and_guests"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let key = |app: &nova::AppWindow, key: slint::platform::Key| {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
        settle();
    };
    for host in [true, false] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.global::<nova::InterfaceStyle>().set_tv(true);
        app.window().set_size(slint::PhysicalSize::new(1280, 900));
        app.set_party_page_open(true);
        app.set_party_active(true);
        set_host(&app, host);
        app.set_party_has_media(true);
        app.set_party_can_return_to_player(true);
        app.set_party_duration(3600.0);
        app.set_party_title("A film".into());
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = actions.clone();
        app.on_party_set_paused(move |_| recorded.borrow_mut().push("pause"));
        let recorded = actions.clone();
        app.on_party_return_to_player(move || recorded.borrow_mut().push("return"));
        let recorded = actions.clone();
        app.on_party_choose_stream(move || recorded.borrow_mut().push("stream"));
        let recorded = actions.clone();
        app.on_party_choose_media(move || recorded.borrow_mut().push("media"));
        app.on_toggle_pause(|| panic!("room navigation invoked the local player"));
        app.window().show().unwrap();
        settle();
        if host {
            key(&app, slint::platform::Key::DownArrow); // timeline
            key(&app, slint::platform::Key::DownArrow); // pause
            key(&app, slint::platform::Key::Return);
            assert_eq!(&*actions.borrow(), &["pause"]);
            actions.borrow_mut().clear();
        } else {
            for absent in ["Pause", "Change what to watch", "−10 s", "+10 s"] {
                assert!(
                    ElementHandle::find_by_accessible_label(&app, absent)
                        .next()
                        .is_none(),
                    "guest exposes host action {absent}"
                );
            }
        }
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(&*actions.borrow(), &["return"]);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(&*actions.borrow(), &["return", "stream"]);
        if host {
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(&*actions.borrow(), &["return", "stream", "media"]);
            key(&app, slint::platform::Key::UpArrow);
        }
        key(&app, slint::platform::Key::UpArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(actions.borrow().last(), Some(&"return"));
    }
}

#[test]
fn player_only_exposes_party_access_when_connected_or_connecting() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::player_only_exposes_party_access_when_connected_or_connecting"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 720));
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_party_can_host(true);
    app.window().show().unwrap();
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Open watch party")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Watch together")
            .next()
            .is_none()
    );
    app.set_party_active(true);
    app.set_playback_started(true);
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    for android in [true, false] {
        app.set_is_android(android);
        app.set_osd_visible(true);
        settle();
        let badge = button(&app, "Open watch party");
        assert_eq!(badge.computed_opacity(), 1.0);
        let origin = badge.absolute_position();
        let size = badge.size();
        let position =
            slint::LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
        app.set_osd_visible(false);
        settle();
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Open watch party")
                .next()
                .is_none(),
            "hidden OSD removes party access from hit-testing and accessibility"
        );
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        settle();
        assert!(
            !app.get_party_page_open(),
            "a hidden badge must not intercept a wake tap"
        );
        app.set_osd_visible(true);
        settle();
        button(&app, "Open watch party")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        assert!(app.get_party_page_open());
        app.set_party_page_open(false);
        settle();
    }
}

#[test]
fn unified_policy_is_live_creator_only_and_keeps_guest_sources_local() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::unified_policy_is_live_creator_only_and_keeps_guest_sources_local"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_party_page_open(true);
    let changes = Rc::new(RefCell::new(Vec::new()));
    let recorded = changes.clone();
    app.on_party_set_allow_everyone(move |value| recorded.borrow_mut().push(value));
    app.window().set_size(slint::PhysicalSize::new(360, 1800));
    app.window().show().unwrap();
    settle();
    let policy_label = "Allow everyone to control playback and choose episodes";
    let toggle = || {
        ElementHandle::find_by_accessible_label(&app, policy_label)
            .find(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
            .expect("creator policy toggle")
    };
    assert!(
        !app.get_party_allow_everyone(),
        "the default policy is host only"
    );
    toggle().mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        &*changes.borrow(),
        &[true],
        "configure the room before creation"
    );
    app.set_party_active(true);
    set_host(&app, true);
    app.set_party_allow_everyone(true);
    settle();
    toggle().mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        &*changes.borrow(),
        &[true, false],
        "creator can change a live room"
    );
    set_host(&app, false);
    app.set_party_duration(3600.0);
    for (language, everyone_status, host_status, choose, change, stream, pause) in [
        (
            "en",
            "Everyone can control playback and choose episodes.",
            "Only the host can control playback and choose episodes.",
            "Choose what to watch",
            "Change what to watch",
            "Choose a stream",
            "Pause",
        ),
        (
            "hr",
            "Svi mogu upravljati reprodukcijom i birati epizode.",
            "Samo domaćin može upravljati reprodukcijom i birati epizode.",
            "Odaberite što ćete gledati",
            "Promijenite što ćete gledati",
            "Odaberite izvor",
            "Pauzirajte",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height, tv) in [(320, 1800, false), (1280, 900, false), (1280, 900, true)] {
            app.global::<nova::InterfaceStyle>().set_tv(tv);
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for everyone in [false, true, false] {
                app.set_party_allow_everyone(everyone);
                app.set_party_can_control(everyone);
                app.set_party_can_choose_media(everyone);
                for has_media in [false, true] {
                    app.set_party_has_media(has_media);
                    settle();
                    assert!(
                        ElementHandle::find_by_element_id(&app, "PartyPage::party_policy")
                            .next()
                            .is_none(),
                        "guests cannot edit policy"
                    );
                    assert!(
                        ElementHandle::find_by_accessible_label(
                            &app,
                            if everyone {
                                everyone_status
                            } else {
                                host_status
                            }
                        )
                        .next()
                        .is_some()
                    );
                    let media_button = ElementHandle::find_by_accessible_label(
                        &app,
                        if has_media { change } else { choose },
                    )
                    .find(|item| item.accessible_role() == Some(AccessibleRole::Button));
                    assert_eq!(
                        media_button.is_some(),
                        everyone,
                        "{language}, {width}, TV={tv}, media={has_media}"
                    );
                    if let Some(item) = media_button {
                        assert!(item.absolute_position().x >= 0.0);
                        assert!(item.absolute_position().x + item.size().width <= width as f32);
                    }
                    assert_eq!(
                        ElementHandle::find_by_accessible_label(&app, pause)
                            .next()
                            .is_some(),
                        everyone && has_media
                    );
                    assert_eq!(
                        ElementHandle::find_by_accessible_label(&app, stream)
                            .next()
                            .is_some(),
                        has_media,
                        "own sources require only the current accepted episode"
                    );
                    for admin in [
                        "PartyPage::party_wait",
                        "PartyPage::party_copy",
                        "PartyPage::party_continue",
                    ] {
                        assert!(
                            ElementHandle::find_by_element_id(&app, admin)
                                .next()
                                .is_none(),
                            "guest must not acquire creator actions"
                        );
                    }
                }
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn live_policy_revocation_restores_tv_focus_to_current_episode_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::live_policy_revocation_restores_tv_focus_to_current_episode_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.set_party_active(true);
    app.set_party_page_open(true);
    app.set_party_has_media(true);
    app.set_party_can_return_to_player(true);
    app.set_party_duration(3600.0);
    app.set_party_allow_everyone(true);
    app.set_party_can_control(true);
    app.set_party_can_choose_media(true);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_party_set_paused(move |_| recorded.borrow_mut().push("pause"));
    let recorded = actions.clone();
    app.on_party_return_to_player(move || recorded.borrow_mut().push("return"));
    let recorded = actions.clone();
    app.on_party_choose_stream(move || recorded.borrow_mut().push("stream"));
    app.window().show().unwrap();
    settle();
    let key = |key: slint::platform::Key| {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
        settle();
    };
    key(slint::platform::Key::DownArrow); // timeline
    key(slint::platform::Key::DownArrow); // pause
    key(slint::platform::Key::Return);
    assert_eq!(&*actions.borrow(), &["pause"]);
    app.set_party_allow_everyone(false);
    app.set_party_can_control(false);
    app.set_party_can_choose_media(false);
    settle();
    key(slint::platform::Key::DownArrow);
    key(slint::platform::Key::Return);
    key(slint::platform::Key::DownArrow);
    key(slint::platform::Key::Return);
    assert_eq!(&*actions.borrow(), &["pause", "return", "stream"]);
}

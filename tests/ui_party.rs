//! Watch-room controls and responsive, translated UI without a network or player.
#[path = "support/isolation.rs"]
mod isolation;

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
}

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

#[test]
fn room_actions_are_guarded_and_guests_have_no_shared_timeline_controls() {
    if !isolation::run(concat!(
        module_path!(),
        "::room_actions_are_guarded_and_guests_have_no_shared_timeline_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 1800));
    app.set_player_open(true);
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
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Watch together")
            .next()
            .is_some()
    );
    button(&app, "Open watch party").mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
    assert!(app.get_party_page_open());
    assert_eq!(
        button(&app, "Create party").accessible_enabled(),
        Some(true)
    );
    app.set_party_can_host(false);
    settle();
    assert_eq!(
        button(&app, "Create party").accessible_enabled(),
        Some(false)
    );
    app.set_party_can_host(true);
    settle();
    button(&app, "Create party").mock_single_click(slint::platform::PointerEventButton::Left);
    app.set_party_input("NVP1-private-test-invite".into());
    settle();
    button(&app, "Join party").mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        &*actions.borrow(),
        &["create", "join:NVP1-private-test-invite"]
    );
    app.set_party_active(true);
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
    app.set_party_is_host(true);
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
    app.set_party_is_host(false);
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
    if !isolation::run(concat!(
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
    app.set_party_is_host(true);
    app.set_party_waiting(true);
    app.set_party_invite("NVP1-test-invite".into());
    app.set_party_title("A very long film title that must wrap inside a phone screen".into());
    app.set_party_participants(
        Rc::new(slint::VecModel::from(vec![nova_ui::PartyMemberRow {
            id: "guest".into(),
            name: "A guest with a long device name that needs more than one line".into(),
            status: "Choosing a stream".into(),
            can_remove: true,
            ..Default::default()
        }]))
        .into(),
    );
    app.window().show().unwrap();
    for (language, title, leave, invite) in [
        ("en", "Watch party", "End party", "Copy invite"),
        (
            "hr",
            "Zajedničko gledanje",
            "Završite zajedničko gledanje",
            "Kopirajte pozivnicu",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height) in [(320, 1800), (840, 380), (1280, 800)] {
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
            for _ in 0..16 {
                for item in i_slint_backend_testing::ElementQuery::from_root(&app)
                    .find_all()
                    .into_iter()
                    .filter(|item| item.accessible_role() == Some(AccessibleRole::Button))
                {
                    saw_leave |= item.accessible_label().as_deref() == Some(leave);
                    saw_invite |= item.accessible_label().as_deref() == Some(invite);
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
                    assert!(
                        participant.size().height < 220.0,
                        "participant stretched into spare height"
                    );
                }
                if saw_leave && saw_invite && saw_participant {
                    break;
                }
                scroll(-(height as f32 * 0.6));
            }
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
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn connecting_badge_reopens_setup_and_cancel_always_dispatches_leave() {
    if !isolation::run(concat!(
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

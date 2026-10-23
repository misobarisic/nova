//! Shared external-receiver picker: navigation is independent of local video and paired Nova.
#[path = "support/isolation.rs"]
mod isolation;
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{
    ComponentHandle, ModelRc, VecModel,
    platform::{Key, WindowEvent},
};
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
fn key(app: &nova::AppWindow, key: Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}
fn setup(width: u32, height: u32) -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::CastBridge>().set_available(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    app.show().unwrap();
    app.global::<nova::CastBridge>().set_open(true);
    settle(&app);
    app
}

#[test]
fn released_ui_hides_casting_on_every_page_and_in_settings_search() {
    if !isolation::run(concat!(
        module_path!(),
        "::released_ui_hides_casting_on_every_page_and_in_settings_search"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    assert!(!app.global::<nova::CastBridge>().get_available());
    app.show().unwrap();
    for page in 0..3 {
        app.set_show_home(page == 0);
        app.set_modal_visible(page == 1);
        app.set_player_open(page == 2);
        app.set_playback_started(page == 2);
        app.set_osd_visible(true);
        settle(&app);
        assert!(
            ElementHandle::find_by_element_type_name(&app, "CastButton")
                .next()
                .is_none(),
            "page {page}"
        );
    }
    app.set_player_open(false);
    app.set_modal_visible(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Casting remux buffer")
            .next()
            .is_none()
    );
    app.set_settings_detail_open(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.set_settings_search_query("remux buffer".into());
    settle(&app);
    assert!(
        ElementQuery::from_root(&app)
            .match_predicate(|item| item.accessible_id().as_deref() == Some("settings:player"))
            .find_first()
            .is_none()
    );
    // Even stale model state cannot render the retained picker in a release.
    app.global::<nova::CastBridge>().set_open(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "CastingPanel")
            .next()
            .is_none()
    );
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| {
            let labels = ElementQuery::from_root(app)
                .match_predicate(|item| item.accessible_label().is_some())
                .find_all()
                .into_iter()
                .map(|item| (item.accessible_label(), item.accessible_role()))
                .collect::<Vec<_>>();
            panic!("missing button {label}; available: {labels:?}")
        })
}
#[test]
fn picker_is_bounded_on_phone_landscape_and_desktop_and_back_keeps_the_session() {
    if !isolation::run(concat!(
        module_path!(),
        "::picker_is_bounded_on_phone_landscape_and_desktop_and_back_keeps_the_session"
    )) {
        return;
    }
    let app = setup(360, 640);
    let ui = app.global::<nova::CastBridge>();
    ui.set_selected(true);
    ui.set_active(true);
    ui.set_target_name("Living room television with a very long name".into());
    ui.set_title("An unusually long title for a narrow landscape window".into());
    ui.set_can_pause(true);
    ui.set_can_seek(true);
    ui.set_can_volume(true);
    ui.set_can_mute(true);
    ui.set_error(
        "A recoverable network error with a long explanation that must wrap inside the sheet"
            .into(),
    );
    for (width, height) in [(360, 640), (640, 360), (1280, 720)] {
        ui.set_open(false);
        settle(&app);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_open(true);
        settle(&app);
        let sheet = ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
            .next()
            .unwrap();
        let p = sheet.absolute_position();
        let size = sheet.size();
        assert!(p.x >= -0.5 && p.y >= -0.5, "sheet origin {p:?}");
        assert!(
            p.x + size.width <= width as f32 + 0.5 && p.y + size.height <= height as f32 + 0.5,
            "sheet bounds {p:?} {size:?} in {width}x{height}"
        );
        // The controller body scrolls on short windows. D-pad focus must
        // reveal Stop before checking its visible touch target.
        for _ in 0..4 {
            key(&app, Key::DownArrow);
        }
        let stop = button(&app, "Stop casting");
        assert!(stop.size().height >= 44.);
        assert!(stop.absolute_position().y >= p.y);
        assert!(stop.absolute_position().y + stop.size().height <= p.y + size.height);
    }
    let actions = Rc::new(RefCell::new(Vec::<String>::new()));
    let recorded = actions.clone();
    ui.on_action(move |name, _| recorded.borrow_mut().push(name.into()));
    key(&app, Key::Back);
    assert!(!ui.get_open());
    assert!(ui.get_active());
    assert!(actions.borrow().is_empty(), "Back must never send STOP");
}
#[test]
fn television_can_reach_inline_uncertain_and_network_recovery_actions() {
    if !isolation::run(concat!(
        module_path!(),
        "::television_can_reach_inline_uncertain_and_network_recovery_actions"
    )) {
        return;
    }
    let app = setup(1280, 720);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    let ui = app.global::<nova::CastBridge>();
    ui.set_open(false);
    settle(&app);
    ui.set_stale(true);
    ui.set_uncertain(true);
    ui.set_unverified(true);
    ui.set_connecting(true);
    ui.set_error("Receiver stop is unconfirmed".into());
    let actions = Rc::new(RefCell::new(Vec::<String>::new()));
    let recorded = actions.clone();
    ui.on_action(move |name, _| recorded.borrow_mut().push(name.into()));
    let cancelled = Rc::new(RefCell::new(0));
    let recorded = cancelled.clone();
    ui.on_cancel(move || *recorded.borrow_mut() += 1);
    ui.set_open(true);
    settle(&app);
    // The receiver list has one local row. Down reaches network search, then
    // address, Close, the compatibility opt-in, retry, cancellation and recovery.
    for _ in 0..4 {
        key(&app, Key::DownArrow);
    }
    key(&app, Key::Return);
    assert!(
        actions.borrow().contains(&"unverified".to_owned()),
        "compatibility recovery must be reachable by D-pad: {:?}",
        actions.borrow()
    );
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert!(actions.borrow().contains(&"retry".to_owned()));
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(*cancelled.borrow(), 1);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert!(actions.borrow().contains(&"stop".to_owned()));
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert!(actions.borrow().contains(&"local_anyway".to_owned()));
    ui.set_open(false);
    settle(&app);
    ui.set_stale(false);
    ui.set_connecting(false);
    ui.set_uncertain(false);
    ui.set_unverified(false);
    ui.set_active(true);
    ui.set_can_text_tracks(true);
    ui.set_text_tracks(
        Rc::new(VecModel::from(vec![
            nova::CastTextTrackRow {
                id: 1,
                name: "First remote subtitles".into(),
            },
            nova::CastTextTrackRow {
                id: 2,
                name: "Second remote subtitles".into(),
            },
        ]))
        .into(),
    );
    let tracks = Rc::new(RefCell::new(Vec::new()));
    let recorded = tracks.clone();
    ui.on_action(move |name, value| recorded.borrow_mut().push((name.to_string(), value)));
    ui.set_open(true);
    settle(&app);
    // Skip unavailable pause/seek/volume actions and reveal the chosen subtitle.
    for _ in 0..8 {
        key(&app, Key::DownArrow);
    }
    let selected = button(&app, "Second remote subtitles");
    let sheet = ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
        .next()
        .unwrap();
    assert!(selected.absolute_position().y >= sheet.absolute_position().y);
    assert!(
        selected.absolute_position().y + selected.size().height
            <= sheet.absolute_position().y + sheet.size().height
    );
    key(&app, Key::Return);
    assert!(tracks.borrow().contains(&("text".to_owned(), 2.)));
}
#[test]
fn receiver_focus_survives_reordered_discovery_and_busy_confirmation_starts_safe() {
    if !isolation::run(concat!(
        module_path!(),
        "::receiver_focus_survives_reordered_discovery_and_busy_confirmation_starts_safe"
    )) {
        return;
    }
    let app = setup(1280, 720);
    let ui = app.global::<nova::CastBridge>();
    let rows = |order: &[&str]| -> ModelRc<nova::CastDeviceRow> {
        Rc::new(VecModel::from(
            order
                .iter()
                .map(|id| nova::CastDeviceRow {
                    id: (*id).into(),
                    name: format!("Receiver {id}").into(),
                    protocol: "Google Cast".into(),
                    available: true,
                })
                .collect::<Vec<_>>(),
        ))
        .into()
    };
    ui.set_devices(rows(&["first", "second"]));
    let selected = Rc::new(RefCell::new(Vec::<String>::new()));
    let recorded = selected.clone();
    ui.on_select(move |id| recorded.borrow_mut().push(id.into()));
    settle(&app);
    key(&app, Key::DownArrow);
    ui.set_devices(rows(&["second", "first"]));
    settle(&app);
    key(&app, Key::Return);
    assert_eq!(&*selected.borrow(), &["first".to_owned()]);
    let replace = Rc::new(RefCell::new(Vec::new()));
    let recorded = replace.clone();
    ui.on_replace(move |yes| recorded.borrow_mut().push(yes));
    ui.set_replacement(true);
    settle(&app);
    key(&app, Key::Return);
    assert_eq!(
        &*replace.borrow(),
        &[false],
        "replacement confirmation defaults to Cancel"
    );
}

fn frames(app: &nova::AppWindow, count: u64) {
    for _ in 0..count {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.computed_opacity(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    }
}
#[test]
fn picker_motion_reverses_and_master_off_snaps_selection_and_retained_exit() {
    if !isolation::run(concat!(
        module_path!(),
        "::picker_motion_reverses_and_master_off_snaps_selection_and_retained_exit"
    )) {
        return;
    }
    let app = setup(360, 720);
    let ui = app.global::<nova::CastBridge>();
    let anim = app.global::<nova::Anim>();
    anim.set_enabled(true);
    ui.set_open(false);
    frames(&app, 70);
    let sheet = ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
        .next()
        .unwrap();
    let fading = sheet.computed_opacity();
    assert!(
        fading > 0. && fading < 1.,
        "retained exit midpoint opacity={fading}"
    );
    ui.set_open(true);
    frames(&app, 70);
    assert!(ui.get_open());
    let reversing = sheet.computed_opacity();
    assert!(
        reversing > fading && reversing < 1.,
        "rapid reopen reverses opacity {fading}->{reversing}"
    );
    frames(&app, 300);
    assert_eq!(sheet.computed_opacity(), 1.);
    let dot = ElementHandle::find_by_element_id(&app, "ReceiverRow::selection_dot")
        .next()
        .unwrap();
    assert_eq!(dot.computed_opacity(), 1.);
    ui.set_selected(true);
    frames(&app, 70);
    let middle = dot.computed_opacity();
    assert!(
        middle > 0. && middle < 1.,
        "radio deselection is retained and animated: {middle}"
    );
    anim.set_enabled(false);
    frames(&app, 2);
    assert_eq!(dot.computed_opacity(), 0.);
    anim.set_enabled(true);
    ui.set_open(false);
    frames(&app, 40);
    assert!(
        ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
            .next()
            .is_some()
    );
    anim.set_transitions(false);
    frames(&app, 2);
    assert!(
        ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
            .next()
            .is_none(),
        "disabling transitions clears retained exit immediately"
    );
    anim.set_transitions(true);
    frames(&app, 300);
    assert!(!ui.get_open());
    assert!(
        ElementHandle::find_by_element_id(&app, "CastingPanel::sheet")
            .next()
            .is_none(),
        "old exit timer cannot reopen the picker"
    );
}

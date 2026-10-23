//! Replaced controls must not leave Android Back unhandled.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn idle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
}

fn click(app: &nova::AppWindow, label: &str) {
    let control = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label);
    click_element(app, control);
}

fn click_element(app: &nova::AppWindow, control: ElementHandle) {
    let p = control.absolute_position();
    let s = control.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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

// Exercise native ownership directly for controls whose touch handlers leave
// focus on the page shell. A surviving strong test handle would mask removal.
fn focus_control(app: &nova::AppWindow, id: &str) {
    use std::ops::ControlFlow;
    let id = id.replace('_', "-");
    let window = i_slint_core::window::WindowInner::from_pub(app.window());
    let root = i_slint_core::item_tree::ItemRc::new_root(window.component());
    let control = root
        .visit_descendants(|item| {
            if (0..item.element_count().unwrap_or(0))
                .any(|index| format!("{:?}", item.element_type_names_and_ids(index)).contains(&id))
            {
                ControlFlow::Break(item.clone())
            } else {
                ControlFlow::Continue(())
            }
        })
        .expect("control in live tree");
    let scope = if control
        .downcast::<i_slint_core::items::FocusScope>()
        .is_some()
    {
        control
    } else {
        control
            .visit_descendants(|item| {
                if item.downcast::<i_slint_core::items::FocusScope>().is_some() {
                    ControlFlow::Break(item.clone())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .expect("native control scope")
    };
    window.set_focus_item(&scope, true, i_slint_core::input::FocusReason::Programmatic);
}

fn back_accepted(app: &nova::AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyPressRepeated {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        assert!(
            matches!(
                app.window().dispatch_event_with_result(event),
                Ok(slint::platform::WindowEventDispatchResult::Accepted)
            ),
            "Back must be accepted so Android does not finish the activity"
        );
    }
    idle();
}

fn back(app: &nova::AppWindow) {
    back_accepted(app);
    assert!(app.get_more_open(), "submenu Back returns to More");
}

fn setup() -> (nova::AppWindow, Rc<Cell<u32>>) {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.global::<nova::Anim>().set_enabled(false);
    let backgrounds = Rc::new(Cell::new(0));
    let count = backgrounds.clone();
    app.on_exit_to_background(move || count.set(count.get() + 1));
    app.window().set_size(slint::PhysicalSize::new(390, 1300));
    app.window().show().unwrap();
    idle();
    (app, backgrounds)
}

fn downloads(app: &nova::AppWindow) {
    app.set_downloads_page_groups(
        Rc::new(VecModel::from(vec![nova::DownloadGroup {
            id: "series".into(),
            title: "Downloaded series".into(),
            episodes: Rc::new(VecModel::from(vec![nova::DownloadEpisode {
                id: "episode".into(),
                title: "Episode".into(),
                action_label: "Resume".into(),
                ..Default::default()
            }]))
            .into(),
            ..Default::default()
        }]))
        .into(),
    );
}

fn open(app: &nova::AppWindow, label: &str) {
    app.invoke_navigate_section(3);
    idle();
    click(app, label);
    idle();
}

#[test]
fn removing_a_download_keeps_back_in_more() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "removing_a_download_keeps_back_in_more"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    open(&app, "Downloads");
    downloads(&app);
    let weak = app.as_weak();
    app.on_downloads_page_action(move |_, action| {
        assert_eq!(action, 1);
        weak.upgrade()
            .unwrap()
            .set_downloads_page_groups(Default::default());
    });
    idle();
    click(&app, "Cancel download");
    assert_eq!(app.get_downloads_page_groups().row_count(), 0);
    back(&app);
    assert!(!app.get_show_downloads());
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn replacing_download_groups_and_transfers_keeps_back_routed() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "replacing_download_groups_and_transfers_keeps_back_routed"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    for transfer in [false, true] {
        open(&app, "Downloads");
        downloads(&app);
        if transfer {
            let episodes = app
                .get_downloads_page_groups()
                .row_data(0)
                .unwrap()
                .episodes;
            app.set_downloads_page_transfers(episodes);
            app.set_downloads_page_groups(Default::default());
        }
        idle();
        click(&app, "Resume");
        // A background completion/filter/queue refresh can remove a focused
        // row without entering its action callback. Use the same handoff as
        // refresh_downloads_page, with no intervening idle before Back.
        app.set_downloads_page_groups(Default::default());
        app.set_downloads_page_transfers(Default::default());
        app.recover_back_focus();
        back(&app);
    }
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn choosing_a_calendar_day_keeps_back_routed_after_rebuild() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "choosing_a_calendar_day_keeps_back_routed_after_rebuild"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    open(&app, "Calendar");
    app.set_calendar_view(1);
    app.set_calendar_week(
        Rc::new(VecModel::from(vec![
            nova::CalendarDay {
                weekday: "Mon".into(),
                cell: nova::CalCell {
                    day: 12,
                    epoch: 123,
                    in_month: true,
                    ..Default::default()
                },
                ..Default::default()
            };
            7
        ]))
        .into(),
    );
    let weak = app.as_weak();
    let picks = Rc::new(Cell::new(0));
    let count = picks.clone();
    app.on_calendar_pick(move |_| {
        count.set(count.get() + 1);
        weak.upgrade()
            .unwrap()
            .set_calendar_week(Default::default());
    });
    idle();
    click(&app, "Mon 12");
    assert_eq!(picks.get(), 1);
    back(&app);
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn recovery_preserves_download_search_input() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "recovery_preserves_download_search_input"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    open(&app, "Downloads");
    downloads(&app);
    idle();
    let search = ElementHandle::find_by_element_type_name(&app, "SearchField")
        .next()
        .unwrap();
    let p = search.absolute_position();
    let s = search.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
    let weak = app.as_weak();
    app.on_downloads_page_query_changed(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_downloads_page_groups(Default::default());
        app.recover_back_focus();
    });
    for text in ["a", "b"] {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: text.into() });
    }
    assert_eq!(app.get_downloads_page_query(), "ab");
    back(&app);
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn devices_refresh_keeps_back_after_a_focused_device_disappears() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "devices_refresh_keeps_back_after_a_focused_device_disappears"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    open(&app, "Devices");
    app.set_remote_devices(
        Rc::new(VecModel::from(vec![nova::RemoteDeviceRow {
            id: "peer".into(),
            name: "TV".into(),
            ready: true,
            has_session: true,
            ..Default::default()
        }]))
        .into(),
    );
    idle();
    focus_control(&app, "device_controls");
    app.set_remote_devices(Default::default());
    app.recover_back_focus();
    back(&app);
    assert!(!app.get_remote_devices_open());
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn joining_and_ending_a_party_keep_back_routed() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "joining_and_ending_a_party_keep_back_routed"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    open(&app, "Watch party");
    let input = ElementHandle::find_by_element_id(&app, "PartyPage::invite_input")
        .next()
        .unwrap();
    click_element(&app, input);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: "invite".into(),
        });
    assert_eq!(app.get_party_input(), "invite");
    // The asynchronous join result replaces the focused invite editor.
    app.set_party_active(true);
    app.set_party_is_host(true);
    app.recover_back_focus();
    back(&app);
    assert!(!app.get_party_page_open());
    open(&app, "Watch party");
    focus_control(&app, "choose_media");
    app.set_party_active(false);
    app.recover_back_focus();
    back(&app);
    assert_eq!(backgrounds.get(), 0);
}

#[test]
fn player_submenu_teardown_preserves_native_back() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "player_submenu_teardown_preserves_native_back"
    )) {
        return;
    }
    let (app, backgrounds) = setup();
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(false);
    idle();
    let gear = ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap();
    click_element(&app, gear);
    idle();
    focus_control(&app, "MenuRow::row_focus");
    click(&app, "Playback speed");
    back_accepted(&app);
    assert!(app.get_player_open(), "first Back leaves the speed submenu");
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Playback speed")
            .next()
            .is_some()
    );
    focus_control(&app, "MenuRow::row_focus");
    back_accepted(&app);
    assert!(app.get_player_open(), "next Back dismisses settings");
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
            .next()
            .is_none()
    );
    assert_eq!(backgrounds.get(), 0);
}

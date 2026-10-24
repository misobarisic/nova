//! Optional reminders keep Play direct and their captured episode identity stable.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use nova_ui_shared::LastTimeBridge;
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.size(), e.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing Last time button: {label}"))
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    for event in [
        slint::platform::WindowEvent::KeyPressed { text: key.into() },
        slint::platform::WindowEvent::KeyReleased { text: key.into() },
    ] {
        app.window().dispatch_event(event);
    }
    settle(app);
}

#[test]
fn optional_reminder_and_replay_confirmation_fit_phone_desktop_and_tv() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::optional_reminder_and_replay_confirmation_fit_phone_desktop_and_tv"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.set_show_home(true);
    app.set_touch_menus(true);
    let sheet = app.global::<LastTimeBridge>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    app.on_continue_picked(move |_| captured.borrow_mut().push("play".into()));
    let captured = events.clone();
    let weak = app.as_weak();
    app.on_home_episode_action(move |_, type_, id, episode, action| {
        captured
            .borrow_mut()
            .push(format!("{type_}:{id}:{episode}:{action}"));
        if action == 4 {
            if let Some(app) = weak.upgrade() {
                let sheet = app.global::<LastTimeBridge>();
                sheet.set_title("Returning show".into());
                sheet.set_synopsis("Recorded last episode.".into());
                sheet.set_can_replay(true);
                sheet.set_confirmation(false);
                sheet.set_open(true);
            }
        }
    });
    let captured = events.clone();
    sheet.on_replay(move || captured.borrow_mut().push("replay".into()));
    let captured = events.clone();
    sheet.on_confirm(move || captured.borrow_mut().push("confirm".into()));
    let weak = app.as_weak();
    sheet.on_closed(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<LastTimeBridge>().set_open(false);
        }
    });
    let weak = app.as_weak();
    sheet.on_visual_closed(move || {
        if let Some(app) = weak.upgrade() {
            app.invoke_preserve_back_focus();
            app.invoke_focus_mounted_page();
        }
    });
    app.set_home_continue(
        Rc::new(VecModel::from(vec![nova::ContinueRow {
            media_type: "series".into(),
            id: "show".into(),
            episode_id: "show:1:3".into(),
            title: "Returning show".into(),
            subtitle: "S1 E3".into(),
            last_time: true,
            ..Default::default()
        }]))
        .into(),
    );
    app.show().unwrap();
    for (width, height, tv) in [(320, 760, false), (1280, 900, false), (1920, 1080, true)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        sheet.set_open(false);
        settle(&app);
        let card = ElementHandle::find_by_element_type_name(&app, "ContinueCard")
            .find(|e| e.size().width > 40.0 && e.absolute_position().x >= 0.0)
            .unwrap();
        let origin = card.absolute_position();
        let card_size = card.size();
        let poster = slint::LogicalPosition::new(
            origin.x + card_size.width / 2.0,
            origin.y + card_size.width * 0.65,
        );
        for event in [
            slint::platform::WindowEvent::PointerPressed {
                position: poster,
                button: slint::platform::PointerEventButton::Left,
            },
            slint::platform::WindowEvent::PointerReleased {
                position: poster,
                button: slint::platform::PointerEventButton::Left,
            },
        ] {
            app.window().dispatch_event(event);
        }
        settle(&app);
        assert_eq!(
            events.borrow().last().unwrap(),
            "play",
            "ordinary Continue remains direct"
        );
        for event in [
            slint::platform::WindowEvent::PointerPressed {
                position: poster,
                button: slint::platform::PointerEventButton::Right,
            },
            slint::platform::WindowEvent::PointerReleased {
                position: poster,
                button: slint::platform::PointerEventButton::Right,
            },
        ] {
            app.window().dispatch_event(event);
        }
        settle(&app);
        let menu_action = ElementHandle::find_by_accessible_label(&app, "Last time…")
            .filter(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .max_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y))
            .unwrap();
        menu_action.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            events
                .borrow()
                .last()
                .unwrap()
                .ends_with("series:show:show:1:3:4")
        );
        assert!(sheet.get_open());
        key(&app, slint::platform::Key::UpArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(
            events.borrow().last().unwrap(),
            "replay",
            "the menu callback's new sheet must retain keyboard focus on TV"
        );
        key(&app, slint::platform::Key::Back);
        assert!(!sheet.get_open());
        let reminder = button(&app, "Last time…");
        let p = reminder.absolute_position();
        let size = reminder.size();
        assert!(size.height >= 44.0 && p.x >= 0.0 && p.x + size.width <= width as f32 + 1.0);
        reminder.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            events
                .borrow()
                .last()
                .unwrap()
                .ends_with("series:show:show:1:3:4"),
            "secondary action width={width}, point={p:?}, size={size:?}, retained={}, open={}, events={:?}",
            sheet.get_visual_retained(),
            sheet.get_open(),
            events.borrow()
        );
        assert!(
            sheet.get_open(),
            "the secondary action opens its own reminder without invoking Play"
        );
        key(&app, slint::platform::Key::Back);
        assert!(!sheet.get_open());
        sheet.set_title("Returning show".into());
        sheet.set_date("2026-09-01".into());
        sheet.set_episode("S1 E2 · Last completed episode".into());
        sheet.set_synopsis("Only the last completed episode's synopsis belongs here.".into());
        sheet.set_partial("In progress: S1 E3 · 35%".into());
        sheet.set_can_replay(true);
        sheet.set_confirmation(false);
        sheet.set_open(true);
        settle(&app);
        for label in ["Replay ending…", "Close"] {
            let control = button(&app, label);
            let p = control.absolute_position();
            let s = control.size();
            assert!(
                s.height >= 44.0
                    && p.x >= 0.0
                    && p.x + s.width <= width as f32 + 1.0
                    && p.y >= 0.0
                    && p.y + s.height <= height as f32 + 1.0,
                "{label} must be reachable at {width}×{height}"
            );
        }
        button(&app, "Replay ending…").mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(events.borrow().last().unwrap(), "replay");
        key(&app, slint::platform::Key::Back);
        assert!(!sheet.get_open());
        app.set_player_open(true);
        sheet.set_confirmation(true);
        sheet.set_date("0:40:00".into());
        sheet.set_open(true);
        settle(&app);
        button(&app, "Replay final four minutes")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(events.borrow().last().unwrap(), "confirm");
        key(&app, slint::platform::Key::Back);
        assert!(!sheet.get_open());
        app.set_player_open(false);
    }
}

// Evaluate each frame: the headless backend has no renderer to force lazy
// animated geometry/opacity bindings. Keep this in the existing Home binary.
fn motion_frames(app: &nova::AppWindow, ms: u64) {
    for _ in 0..ms {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        for id in ["LastTimeSheet::panel", "LastTimeSheet::scrim"] {
            for e in ElementHandle::find_by_element_id(app, id) {
                let _ = (e.absolute_position(), e.size(), e.computed_opacity());
            }
        }
    }
}
fn reminder_panel(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "LastTimeSheet::panel")
        .next()
        .expect("retained reminder panel")
}
fn raw_key(app: &nova::AppWindow, key: slint::platform::Key) {
    for event in [
        slint::platform::WindowEvent::KeyPressed { text: key.into() },
        slint::platform::WindowEvent::KeyReleased { text: key.into() },
    ] {
        app.window().dispatch_event(event);
    }
}

#[test]
fn reminder_motion_retains_safe_paint_and_input_until_exit_and_invalidates_immediately() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::reminder_motion_retains_safe_paint_and_input_until_exit_and_invalidates_immediately"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(true);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    let sheet = app.global::<LastTimeBridge>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    let weak = app.as_weak();
    sheet.on_closed(move || {
        captured.borrow_mut().push("closed");
        if let Some(app) = weak.upgrade() {
            let sheet = app.global::<LastTimeBridge>();
            sheet.set_open(false);
            // Later model writes must not replace the outgoing synopsis/mode.
            sheet.set_title("Later title must stay hidden".into());
            sheet.set_synopsis("Later synopsis must stay hidden".into());
            sheet.set_confirmation(true);
        }
    });
    let captured = events.clone();
    let weak = app.as_weak();
    sheet.on_visual_closed(move || {
        captured.borrow_mut().push("visual-closed");
        if let Some(app) = weak.upgrade() {
            app.invoke_preserve_back_focus();
            app.invoke_focus_mounted_page();
        }
    });
    let captured = events.clone();
    app.on_exit_to_background(move || captured.borrow_mut().push("background"));
    let captured = events.clone();
    app.on_home_search_picked(move || captured.borrow_mut().push("search"));
    app.show().unwrap();
    for (width, height, tv) in [(390, 900, false), (1280, 900, false), (1920, 1080, true)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        sheet.set_confirmation(false);
        sheet.set_title("Safe completed episode".into());
        sheet.set_synopsis("Safe completed synopsis".into());
        sheet.set_can_replay(false);
        sheet.set_open(true);
        motion_frames(&app, 1);
        let panel = reminder_panel(&app);
        let start_y = panel.absolute_position().y;
        assert!(sheet.get_visual_retained());
        motion_frames(&app, 90);
        let entering_y = panel.absolute_position().y;
        assert!(entering_y < start_y - 1.0, "entrance must visibly slide");
        let scrim = ElementHandle::find_by_element_id(&app, "LastTimeSheet::scrim")
            .next()
            .unwrap();
        assert!(
            scrim.computed_opacity() > 0.01 && scrim.computed_opacity() < 0.99,
            "scrim must fade independently of slide"
        );
        motion_frames(&app, 200);
        let settled_y = panel.absolute_position().y;
        assert!(settled_y < entering_y - 1.0);
        let before_closed = events.borrow().iter().filter(|e| **e == "closed").count();
        let before_visual = events
            .borrow()
            .iter()
            .filter(|e| **e == "visual-closed")
            .count();
        app.set_system_back_request(app.get_system_back_request() + 1);
        motion_frames(&app, 1);
        assert!(!sheet.get_open() && sheet.get_visual_retained());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Safe completed synopsis")
                .next()
                .is_some()
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Later synopsis must stay hidden")
                .next()
                .is_none()
        );
        motion_frames(&app, 70);
        assert!(
            panel.absolute_position().y > settled_y + 1.0,
            "exit must retain and slide painted panel"
        );
        raw_key(&app, slint::platform::Key::Back);
        raw_key(&app, slint::platform::Key::Return);
        app.set_system_back_request(app.get_system_back_request() + 1);
        // A pointer click aimed at the underlying Home search icon is swallowed.
        let search = ElementHandle::find_by_accessible_label(&app, "Search Nova")
            .next()
            .unwrap();
        let p = search.absolute_position();
        let size = search.size();
        let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
        motion_frames(&app, 1);
        assert_eq!(
            events.borrow().iter().filter(|e| **e == "closed").count(),
            before_closed + 1
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| **e == "visual-closed")
                .count(),
            before_visual
        );
        assert!(
            !events
                .borrow()
                .iter()
                .any(|e| *e == "background" || *e == "search")
        );
        // Rapid reopen cancels the old exit timer and reverses from its live position.
        sheet.set_confirmation(false);
        sheet.set_title("New safe completed episode".into());
        sheet.set_synopsis("New safe synopsis".into());
        sheet.set_open(true);
        motion_frames(&app, 300);
        assert!(sheet.get_open() && sheet.get_visual_retained());
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| **e == "visual-closed")
                .count(),
            before_visual
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "New safe synopsis")
                .next()
                .is_some()
        );
        // A normal complete exit restores focus exactly once, after the 260ms slide.
        sheet.set_open(false);
        motion_frames(&app, 200);
        assert!(sheet.get_visual_retained());
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| **e == "visual-closed")
                .count(),
            before_visual
        );
        motion_frames(&app, 80);
        assert!(!sheet.get_visual_retained());
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|e| **e == "visual-closed")
                .count(),
            before_visual + 1
        );
        // Both independent motion switches settle a pending exit immediately.
        for master_off in [false, true] {
            app.global::<nova::Anim>().set_enabled(true);
            app.global::<nova::Anim>().set_transitions(true);
            sheet.set_confirmation(false);
            sheet.set_open(true);
            motion_frames(&app, 60);
            if master_off {
                app.global::<nova::Anim>().set_enabled(false);
            } else {
                app.global::<nova::Anim>().set_transitions(false);
            }
            motion_frames(&app, 1);
            let shown = reminder_panel(&app);
            assert!(
                (shown.absolute_position().y - (height as f32 - shown.size().height)).abs() < 1.0,
                "disabling motion must settle a live entrance immediately: width={width}, master_off={master_off}, y={}, panel_h={}, expected_y={}, enabled={}, transitions={}",
                shown.absolute_position().y,
                shown.size().height,
                height as f32 - shown.size().height,
                app.global::<nova::Anim>().get_enabled(),
                app.global::<nova::Anim>().get_transitions()
            );
            app.global::<nova::Anim>().set_enabled(true);
            app.global::<nova::Anim>().set_transitions(true);
            motion_frames(&app, 300);
            sheet.set_open(false);
            motion_frames(&app, 60);
            assert!(sheet.get_visual_retained());
            if master_off {
                app.global::<nova::Anim>().set_enabled(false);
            } else {
                app.global::<nova::Anim>().set_transitions(false);
            }
            motion_frames(&app, 1);
            assert!(!sheet.get_visual_retained());
            app.global::<nova::Anim>().set_enabled(true);
            app.global::<nova::Anim>().set_transitions(true);
            motion_frames(&app, 300);
            assert!(
                !sheet.get_open() && !sheet.get_visual_retained(),
                "old animations must not revive"
            );
        }
        // Profile invalidation wipes paint immediately instead of retaining its
        // synopsis in the next profile's exit frames. Navigation does the same.
        sheet.set_confirmation(false);
        sheet.set_synopsis("Old profile synopsis".into());
        sheet.set_open(true);
        motion_frames(&app, 100);
        sheet.set_dismiss_generation(sheet.get_dismiss_generation() + 1);
        motion_frames(&app, 1);
        assert!(!sheet.get_open() && !sheet.get_visual_retained());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Old profile synopsis")
                .next()
                .is_none()
        );
        sheet.set_confirmation(false);
        sheet.set_synopsis("Old destination synopsis".into());
        sheet.set_open(true);
        motion_frames(&app, 100);
        app.set_show_home(false);
        app.set_show_library(true);
        motion_frames(&app, 1);
        assert!(!sheet.get_open() && !sheet.get_visual_retained());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Old destination synopsis")
                .next()
                .is_none()
        );
        app.set_show_library(false);
        app.set_show_home(true);
        motion_frames(&app, 300);
        assert!(!sheet.get_open() && !sheet.get_visual_retained());
    }
}

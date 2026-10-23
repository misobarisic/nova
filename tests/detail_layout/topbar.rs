//! Detail library controls: save state in the hero, saved-only sync/options,
//! and the same watched/status/removal actions as a library card.

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
    settle();
}

fn click_label(app: &nova::AppWindow, label: &str) {
    click(
        app,
        ElementHandle::find_by_accessible_label(app, label)
            .last()
            .expect(label),
    );
}

#[test]
fn detail_library_controls_follow_saved_state_and_dispatch_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "detail_library_controls_follow_saved_state_and_dispatch_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_touch_menus(true);
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_selected_title("A series title".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 1".into()])).into());
    app.set_category_rows(
        Rc::new(VecModel::from(vec![nova::CategoryRow {
            name: "Anime".into(),
        }]))
        .into(),
    );
    let weak = app.as_weak();
    app.on_add_to_library(move || {
        let app = weak.upgrade().unwrap();
        app.set_in_library(!app.get_in_library());
    });
    let watches = Rc::new(RefCell::new(0));
    let recorded_watches = watches.clone();
    app.on_watch_now(move || *recorded_watches.borrow_mut() += 1);
    let tracking = Rc::new(RefCell::new(0));
    let recorded_tracking = tracking.clone();
    app.on_tracking_show(move || *recorded_tracking.borrow_mut() += 1);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded_actions = actions.clone();
    let weak = app.as_weak();
    app.on_detail_library_action(move |action| {
        recorded_actions.borrow_mut().push(action);
        if action == 4 {
            weak.upgrade().unwrap().set_in_library(false);
        }
    });
    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    app.window().show().unwrap();
    settle();

    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "TopIconButton").count(),
        1
    );
    assert_eq!(
        element(&app, "DetailPage::library_state_button")
            .accessible_label()
            .as_deref(),
        Some("Add to library")
    );
    // The sole save control lives beside Watch Now, not in the top bar.
    click(&app, element(&app, "DetailPage::library_hero"));
    assert!(app.get_in_library());
    assert_eq!(
        element(&app, "DetailPage::library_state_button")
            .accessible_label()
            .as_deref(),
        Some("Remove from library")
    );

    for width in [320, 390, 900, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        settle();
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "TopIconButton").count(),
            3
        );
        for label in ["Start S1 E1", "Continue S12 E123", "Nastavite S12 E123"] {
            app.set_detail_watch_label(label.into());
            settle();
            let action = element(&app, "DetailPage::watch_action_button");
            assert_eq!(action.accessible_label().as_deref(), Some(label));
            let p = action.absolute_position();
            assert!(p.x >= 0.0 && p.x + action.size().width <= width as f32);
            assert!(
                action
                    .query_descendants()
                    .match_type_name("IcPlay")
                    .find_first()
                    .is_some()
            );
        }
        click(&app, element(&app, "DetailPage::watch_hero"));
        let sync = element(&app, "DetailPage::tracking_button");
        let options = element(&app, "DetailPage::library_options_button");
        assert!((sync.absolute_position().y - options.absolute_position().y).abs() < 0.5);
        assert!(sync.absolute_position().x + sync.size().width <= options.absolute_position().x);
        assert!(options.absolute_position().x + options.size().width <= width as f32);
    }
    assert_eq!(*watches.borrow(), 4);
    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    settle();
    click(&app, element(&app, "DetailPage::tracking_button"));
    assert_eq!(*tracking.borrow(), 1);

    for (label, action) in [
        ("Mark watched", 0),
        ("Mark as Plan to Watch", 7),
        ("Mark on hold", 1),
        ("Mark dropped", 2),
        ("Back to automatic", 3),
    ] {
        click(&app, element(&app, "DetailPage::library_options_button"));
        click_label(&app, label);
        assert_eq!(actions.borrow().last(), Some(&action));
    }
    app.set_detail_library_watched(true);
    click(&app, element(&app, "DetailPage::library_options_button"));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark unwatched")
            .next()
            .is_some()
    );
    // Android Back dismisses the sheet without leaving details or taking an action.
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle();
    assert!(app.get_modal_visible());
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark unwatched")
            .next()
            .is_none()
    );
    assert_eq!(actions.borrow().len(), 5);

    click(&app, element(&app, "DetailPage::library_options_button"));
    click_label(&app, "Categories");
    assert!(app.get_categories_modal());
    app.set_categories_modal(false);
    settle();
    click(&app, element(&app, "DetailPage::library_options_button"));
    click_label(&app, "Remove from library");
    assert_eq!(actions.borrow().last(), Some(&4));
    assert!(!app.get_in_library());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "TopIconButton").count(),
        1
    );
    assert_eq!(app.get_detail_kb_top(), 0);

    // Hidden top-right controls are skipped by directional navigation.
    app.set_detail_kb_zone(0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::RightArrow.into(),
        });
    settle();
    assert_eq!(app.get_detail_kb_top(), 0);

    // Movies retain both actions, with directional access to saving.
    app.set_detail_is_movie(true);
    app.set_season_names(Default::default());
    settle();
    assert_eq!(
        element(&app, "DetailPage::library_state_button")
            .accessible_label()
            .as_deref(),
        Some("Add to library")
    );
    app.set_detail_kb_zone(2);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::RightArrow.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Return.into(),
        });
    settle();
    assert!(
        app.get_in_library(),
        "movies retain keyboard access to saving"
    );
}

#[test]
fn local_library_saves_pop_in_place_and_do_not_replay_on_open_or_sync() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "local_library_saves_pop_in_place_and_do_not_replay_on_open_or_sync"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_selected_title("Save feedback".into());
    app.set_detail_is_movie(true);
    app.set_touch_menus(true);
    // A new page consumes the existing receipt without replaying it.
    app.set_in_library(true);
    app.set_detail_library_save_request(5);
    app.show().unwrap();
    let icon = || element(&app, "DetailPage::library_saved_icon");
    let advance = |ms| {
        for _ in 0..ms {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
            // Closing Detail deliberately unmounts its icon; render only the
            // currently mounted page while checking receipt replay on reopen.
            if let Some(icon) =
                ElementHandle::find_by_element_id(&app, "DetailPage::library_saved_icon").next()
            {
                let _ = icon.size();
            }
        }
    };
    let weak = app.as_weak();
    let adds = Rc::new(RefCell::new(0));
    let recorded = adds.clone();
    app.on_add_to_library(move || {
        let app = weak.upgrade().unwrap();
        if app.get_in_library() {
            app.set_in_library(false);
        } else {
            app.set_in_library(true);
            app.set_detail_library_save_request(app.get_detail_library_save_request() + 1);
            *recorded.borrow_mut() += 1;
        }
    });
    for width in [390, 820, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1400));
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_hover(true);
        advance(350);
        assert_eq!(icon().size().width, 26.0, "opening a saved title is idle");
        app.set_in_library(false);
        advance(1);
        app.set_in_library(true);
        advance(60);
        assert_eq!(
            icon().size().width,
            26.0,
            "projection without a local receipt is idle"
        );
        app.set_in_library(false);
        advance(1);
        let button = element(&app, "DetailPage::library_state_button");
        let button_origin = button.absolute_position();
        let button_size = button.size();
        let watch_origin = element(&app, "DetailPage::watch_action_button").absolute_position();
        // Sample the animation instead of the click helper, which settles it.
        let area = element(&app, "DetailPage::library_hero");
        let p = area.absolute_position();
        let s = area.size();
        let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
        advance(1);
        assert!(app.get_in_library());
        assert!(
            icon().size().width < 26.0,
            "bookmark starts small during the crossfade"
        );
        let mut peak = 0.0_f32;
        for _ in 0..300 {
            advance(1);
            peak = peak.max(icon().size().width);
            assert_eq!(button.absolute_position(), button_origin);
            assert_eq!(button.size(), button_size);
            assert_eq!(
                element(&app, "DetailPage::watch_action_button").absolute_position(),
                watch_origin
            );
            let center_x = icon().absolute_position().x + icon().size().width / 2.0;
            assert!((center_x - button_origin.x - button_size.width / 2.0).abs() < 0.5);
        }
        assert!(
            peak > 28.0 && peak < 31.0,
            "bookmark should pop gently: {peak}"
        );
        assert!((icon().size().width - 26.0).abs() < 0.1);
        // Removal interrupts the pulse immediately; a rapid re-add restarts it.
        app.set_in_library(false);
        app.invoke_add_to_library();
        advance(110);
        assert!(
            icon().size().width > 26.0,
            "rapid add should be in its pop at width {width}: {}",
            icon().size().width
        );
        app.invoke_add_to_library();
        advance(1);
        assert_eq!(
            element(&app, "DetailPage::library_add_icon").size().width,
            26.0
        );
        app.invoke_add_to_library();
        advance(1);
        assert!(icon().size().width < 26.0);
        app.global::<nova::Anim>().set_hover(false);
        advance(1);
        assert_eq!(
            icon().size().width,
            26.0,
            "disabling motion settles an active save"
        );
        for (master, hover) in [(false, true), (true, false)] {
            app.global::<nova::Anim>().set_enabled(master);
            app.global::<nova::Anim>().set_hover(hover);
            app.set_in_library(false);
            app.invoke_add_to_library();
            advance(1);
            assert_eq!(
                icon().size().width,
                26.0,
                "motion-off saves snap to the bookmark"
            );
        }
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_hover(true);
        app.set_modal_visible(false);
        advance(50);
        app.set_modal_visible(true);
        advance(70);
        assert_eq!(
            icon().size().width,
            26.0,
            "returning to Detail must not replay the receipt"
        );
    }
    assert_eq!(*adds.borrow(), 15);
}

#[test]
fn release_reminders_live_in_options_with_explicit_modes_and_safe_back() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::release_reminders_live_in_options_with_explicit_modes_and_safe_back"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, tv) in [(390, false), (1280, false), (960, true)] {
        let app = nova::AppWindow::new().unwrap();
        app.set_touch_menus(true);
        app.set_animations(false);
        app.set_anim_transitions(false);
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.set_show_home(false);
        app.set_modal_visible(true);
        app.set_in_library(true);
        app.set_selected_title("A series".into());
        let tracking = Rc::new(RefCell::new(0));
        let tracked = tracking.clone();
        app.on_tracking_show(move || *tracked.borrow_mut() += 1);
        let modes = Rc::new(RefCell::new(Vec::new()));
        let recorded = modes.clone();
        let calendar = app.global::<nova::CalendarBridge>();
        calendar.on_set_detail_reminder(move |mode| recorded.borrow_mut().push(mode));
        app.show().unwrap();
        settle();
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Remind me")
                .next()
                .is_none(),
            "hero reminder must be removed"
        );
        click(&app, element(&app, "DetailPage::library_options_button"));
        click_label(&app, "Release reminders");
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Each new episode")
                .next()
                .is_some()
        );
        // The new sheet owns keyboard input on desktop as well as TV. Its
        // safe initial selection is Cancel, followed by Full season above it.
        for text in [slint::platform::Key::UpArrow, slint::platform::Key::Return] {
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.into() });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: text.into() });
        }
        settle();
        assert_eq!(*modes.borrow(), vec![2]);
        assert!(app.get_modal_visible());
        for text in [
            slint::platform::Key::LeftArrow,
            slint::platform::Key::Return,
        ] {
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.into() });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: text.into() });
        }
        assert_eq!(
            *tracking.borrow(),
            1,
            "closing the mode chooser restores Detail keyboard navigation"
        );
        for mode in 0..=2 {
            calendar.set_detail_reminder_mode(mode);
            click(&app, element(&app, "DetailPage::library_options_button"));
            click_label(&app, "Release reminders");
            for (index, label) in ["Off", "Each new episode", "Full season released"]
                .into_iter()
                .enumerate()
            {
                let row = ElementHandle::find_by_accessible_label(&app, label)
                    .find(|row| row.accessible_role() == Some(AccessibleRole::RadioButton))
                    .expect("reminder mode is a labelled radio choice");
                assert_eq!(row.accessible_checked(), Some(index as i32 == mode));
            }
            app.set_system_back_request(app.get_system_back_request() + 1);
            settle();
        }
        assert!(app.get_modal_visible());
        assert_eq!(
            *modes.borrow(),
            vec![2],
            "Back cannot change the subscription"
        );
        app.set_detail_is_movie(true);
        click(&app, element(&app, "DetailPage::library_options_button"));
        click_label(&app, "Release reminders");
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Each new episode")
                .next()
                .is_none(),
            "movies cannot open episode reminders"
        );
        app.hide().unwrap();
    }
}

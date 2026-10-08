//! Detail library controls: save state in the hero, saved-only sync/options,
//! and the same watched/status/removal actions as a library card.

use i_slint_backend_testing::ElementHandle;
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
        for label in [
            "Start watching S1 E1",
            "Continue watching S12 E123",
            "Nastavite gledati S12 E123",
        ] {
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
        ("Mark series as watched", 0),
        ("Mark as On Hold", 1),
        ("Mark as Dropped", 2),
        ("Back to automatic", 3),
    ] {
        click(&app, element(&app, "DetailPage::library_options_button"));
        click_label(&app, label);
        assert_eq!(actions.borrow().last(), Some(&action));
    }
    app.set_detail_library_watched(true);
    click(&app, element(&app, "DetailPage::library_options_button"));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark series as unwatched")
            .next()
            .is_some()
    );
    // Android Back dismisses the sheet without leaving details or taking an action.
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle();
    assert!(app.get_modal_visible());
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark series as unwatched")
            .next()
            .is_none()
    );
    assert_eq!(actions.borrow().len(), 4);

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

    // Movies also need an add control when no episode Watch Now action exists.
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
            text: slint::platform::Key::Return.into(),
        });
    settle();
    assert!(
        app.get_in_library(),
        "movies retain keyboard access to saving"
    );
}

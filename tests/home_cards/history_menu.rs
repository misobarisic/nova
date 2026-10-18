//! History cards reuse Continue Watching's native popup and touch sheet.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn pointer(app: &nova::AppWindow, position: LogicalPosition, pressed: bool, right: bool) {
    let button = if right {
        slint::platform::PointerEventButton::Right
    } else {
        slint::platform::PointerEventButton::Left
    };
    app.window().dispatch_event(if pressed {
        slint::platform::WindowEvent::PointerPressed { position, button }
    } else {
        slint::platform::WindowEvent::PointerReleased { position, button }
    });
    settle(1);
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let p = element.absolute_position();
    let s = element.size();
    LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0)
}

fn card(app: &nova::AppWindow) -> LogicalPosition {
    center(
        &ElementHandle::find_by_element_type_name(app, "HomeCatalogPosterCard")
            .find(|element| {
                let p = element.absolute_position();
                let width = element.size().width;
                p.x >= 10.0
                    && p.y > 40.0
                    && p.y < 700.0
                    && if app.get_home_view() == 0 {
                        width < 160.0
                    } else {
                        width >= 160.0
                    }
            })
            .expect("visible history card"),
    )
}

fn row(app: &nova::AppWindow, label: &str) {
    let element = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap_or_else(|| panic!("missing {label}"));
    let position = center(&element);
    pointer(app, position, true, false);
    pointer(app, position, false, false);
    settle(250);
}

fn back(app: &nova::AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle(100);
}

#[test]
fn history_menu_holds_right_clicks_and_routes_stable_title_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::history_menu_holds_right_clicks_and_routes_stable_title_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.set_touch_menus(true);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.set_home_catalog_sections(
        Rc::new(VecModel::from(vec![nova::HomeCatalogSection {
            title: "Watch history".into(),
            is_history: true,
            first_card: 0,
            card_count: 1,
        }]))
        .into(),
    );
    let cards = Rc::new(VecModel::from(vec![nova::HomeCatalogCard {
        id: "show-a".into(),
        media_type: "series".into(),
        title: "Original title".into(),
        history_action: "Resume".into(),
        ..Default::default()
    }]));
    app.set_home_catalog_cards(cards.clone().into());
    app.set_home_catalog_order(Rc::new(VecModel::from(vec![0])).into());
    app.set_home_catalog_positions(Rc::new(VecModel::from(vec![-1, -1, 0])).into());
    let actions = Rc::new(RefCell::new(Vec::new()));
    let actions_callback = actions.clone();
    app.on_home_history_action(move |type_, id, action| {
        actions_callback
            .borrow_mut()
            .push((type_.to_string(), id.to_string(), action))
    });
    let picks = Rc::new(RefCell::new(Vec::new()));
    let picks_callback = picks.clone();
    app.on_home_catalog_card_picked(move |index| picks_callback.borrow_mut().push(index));
    app.window().show().unwrap();
    settle(300);

    // Landing hold: opening and releasing a menu must not also play.
    let position = card(&app);
    pointer(&app, position, true, false);
    settle(850);
    pointer(&app, position, false, false);
    settle(250);
    for label in ["Play", "Enter series", "Remove from watch history"] {
        assert!(
            ElementHandle::find_by_accessible_label(&app, label)
                .next()
                .is_some(),
            "missing {label}"
        );
    }
    assert!(picks.borrow().is_empty());
    // A sync refresh can replace the delegate while the sheet stays open.
    let mut replacement = cards.row_data(0).unwrap();
    replacement.id = "show-b".into();
    cards.set_row_data(0, replacement);
    row(&app, "Remove from watch history");
    assert_eq!(
        actions.borrow().as_slice(),
        [("series".into(), "show-a".into(), 2)]
    );

    // Grid uses the same menu and routes Play / Enter independently.
    app.set_home_view(3);
    settle(250);
    for (label, action) in [("Play", 0), ("Enter series", 1)] {
        let position = card(&app);
        pointer(&app, position, true, true);
        pointer(&app, position, false, true);
        settle(250);
        row(&app, label);
        assert_eq!(
            actions.borrow().last().unwrap(),
            &("series".into(), "show-b".into(), action)
        );
    }
    assert!(picks.borrow().is_empty());

    // Cancel leaves the grid intact and emits no action.
    let before = actions.borrow().len();
    let position = card(&app);
    pointer(&app, position, true, true);
    pointer(&app, position, false, true);
    settle(250);
    back(&app);
    assert_eq!(actions.borrow().len(), before);
    assert_eq!(app.get_home_view(), 3);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "MenuSheet")
            .next()
            .is_none()
    );

    // Desktop right-click uses the native popup, never the touch sheet.
    app.set_touch_menus(false);
    let position = card(&app);
    pointer(&app, position, true, true);
    pointer(&app, position, false, true);
    settle(250);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "MenuSheet")
            .next()
            .is_none()
    );
    assert_eq!(actions.borrow().len(), before);
    assert!(picks.borrow().is_empty());
}

#[test]
fn archive_remains_reachable_with_only_completed_items_and_supports_keyboard() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::archive_remains_reachable_with_only_completed_items_and_supports_keyboard"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.set_home_history_archive_cards(
        Rc::new(VecModel::from(vec![nova::HomeCatalogCard {
            id: "finished".into(),
            media_type: "movie".into(),
            title: "Finished movie".into(),
            history_action: "Play again".into(),
            history_date: "Oct 17, 2026".into(),
            ..Default::default()
        }]))
        .into(),
    );
    // Production publishes a compact order with empty rails omitted.
    app.set_home_catalog_order(Rc::new(VecModel::from(Vec::<i32>::new())).into());
    app.set_home_catalog_positions(Rc::new(VecModel::from(Vec::<i32>::new())).into());
    let actions = Rc::new(RefCell::new(Vec::new()));
    let callback_actions = actions.clone();
    app.on_home_history_action(move |type_, id, action| {
        callback_actions
            .borrow_mut()
            .push((type_.to_string(), id.to_string(), action))
    });
    app.window().show().unwrap();
    settle(300);
    app.invoke_focus_mounted_page();
    assert_eq!(app.get_home_catalog_cards().row_count(), 0);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "View full watch history")
            .next()
            .is_some()
    );
    let key = |key: slint::platform::Key| {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
        settle(100);
    };
    key(slint::platform::Key::DownArrow);
    assert_eq!(app.get_home_kb_zone(), 5);
    key(slint::platform::Key::Return);
    assert_eq!(app.get_home_view(), -4);
    key(slint::platform::Key::Return);
    assert_eq!(
        actions.borrow().as_slice(),
        [("movie".into(), "finished".into(), 0)]
    );
    key(slint::platform::Key::Menu);
    row(&app, "Enter series");
    assert_eq!(
        actions.borrow().last().unwrap(),
        &("movie".into(), "finished".into(), 1)
    );
    app.set_home_history_archive_cards(
        Rc::new(VecModel::from(Vec::<nova::HomeCatalogCard>::new())).into(),
    );
    settle(100);
    assert!(
        ElementHandle::find_by_element_id(&app, "HomePage::history_empty")
            .next()
            .is_some()
    );
    back(&app);
    assert_eq!(app.get_home_view(), 0);
}

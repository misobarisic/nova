//! Returning shows have their own rail, grid, keyboard route and removal menu.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
}

fn click(
    app: &nova::AppWindow,
    element: &ElementHandle,
    button: slint::platform::PointerEventButton,
) {
    let point = element.absolute_position();
    let size = element.size();
    let position = LogicalPosition::new(point.x + size.width / 2.0, point.y + size.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
}

#[test]
fn returning_episodes_have_independent_navigation_and_removal() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.set_touch_menus(true);
    app.set_home_new_episodes(
        Rc::new(VecModel::from(vec![nova::ContinueRow {
            id: "returning".into(),
            title: "Returning show".into(),
            badge: 2,
            remaining: "2 new episodes".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 850));
    app.window().show().unwrap();
    let picks = Rc::new(RefCell::new(Vec::new()));
    let captured = picks.clone();
    app.on_new_episode_picked(move |index| captured.borrow_mut().push(index));
    let removes = Rc::new(RefCell::new(Vec::new()));
    let captured = removes.clone();
    app.on_new_episode_remove(move |index| captured.borrow_mut().push(index));
    app.on_continue_picked(|_| panic!("returning episode routed to Continue Watching"));
    app.on_continue_remove(|_| panic!("new episode removal used Continue hide"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let section = ElementHandle::find_by_element_id(&app, "HomePage::new_episode_section")
        .next()
        .unwrap();
    assert!(section.size().height > 200.0);

    app.set_home_kb_zone(4);
    key(&app, slint::platform::Key::Return);
    assert_eq!(*picks.borrow(), [0]);
    app.set_home_view(-3);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_element_id(&app, "HomePage::continue_grid")
            .next()
            .is_some()
    );
    assert_eq!(app.get_home_kb_zone(), 4);
    key(&app, slint::platform::Key::Return);
    assert_eq!(*picks.borrow(), [0, 0]);
    let card = ElementHandle::find_by_element_type_name(&app, "ContinueCard")
        .find(|card| card.size().width > 140.0 && card.absolute_position().y > 40.0)
        .unwrap();
    click(&app, &card, slint::platform::PointerEventButton::Right);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let remove = ElementHandle::find_by_accessible_label(&app, "Remove from New episodes")
        .next()
        .unwrap();
    click(&app, &remove, slint::platform::PointerEventButton::Left);
    assert_eq!(*removes.borrow(), [0]);
    key(&app, slint::platform::Key::Back);
    assert_eq!(app.get_home_view(), 0);
    app.set_home_new_episodes(Rc::new(VecModel::from(Vec::<nova::ContinueRow>::new())).into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert_eq!(
        section.size().height,
        0.0,
        "empty rails must not reserve space"
    );
}

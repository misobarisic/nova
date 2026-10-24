//! Device summaries stay in their own horizontal rail and obey Home visibility.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{rc::Rc, time::Duration};

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing element {id}"))
}

#[test]
fn home_devices_scroll_horizontally_and_toggle_without_clearing_devices() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::home_devices_scroll_horizontally_and_toggle_without_clearing_devices"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.window().set_size(slint::PhysicalSize::new(320, 720));
    app.window().show().unwrap();
    let assert_hidden = || {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert!(
            ElementHandle::find_by_element_id(&app, "RemotePlayingSection::device_rail")
                .next()
                .is_none()
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Devices")
                .next()
                .is_none()
        );
    };
    assert_hidden(); // No paired peers.
    let devices = Rc::new(VecModel::from(
        (0..6)
            .map(|i| nova::RemoteDeviceRow {
                id: format!("device-{i}").into(),
                name: format!("Living room {i}").into(),
                playing_title: "A long movie title on a paired device".into(),
                ready: true,
                has_session: false,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    app.set_remote_devices(devices.clone().into());
    assert_hidden(); // Ready but idle paired peers.
    use slint::Model;
    let mut first = devices.row_data(5).unwrap();
    first.has_session = true;
    devices.set_row_data(5, first);
    app.set_remote_has_playing_devices(true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        element(&app, "RemotePlayingSection::device_rail")
            .size()
            .width
            > 0.0
    );
    let devices_button = ElementHandle::find_by_accessible_label(&app, "Devices")
        .next()
        .unwrap();
    let playing_button = ElementHandle::find_by_accessible_label(
        &app,
        "Living room 5 · A long movie title on a paired device",
    )
    .next()
    .unwrap();
    let gap = playing_button.absolute_position().x
        - devices_button.absolute_position().x
        - devices_button.size().width;
    assert!(
        (gap - 8.0).abs() <= 1.0,
        "idle peers left blank Home rail gaps: {gap}"
    );
    for i in 0..devices.row_count() {
        let mut row = devices.row_data(i).unwrap();
        row.has_session = true;
        devices.set_row_data(i, row);
    }
    // Discovery publishes this derived flag alongside the model on the UI thread.
    app.set_remote_has_playing_devices(true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let rail = element(&app, "RemotePlayingSection::device_rail");
    let choices = element(&app, "RemotePlayingSection::device_choices");
    assert!(rail.size().width <= 320.0);
    assert!(rail.absolute_position().x >= 0.0);
    assert!(rail.absolute_position().x + rail.size().width <= 320.5);
    assert!(choices.size().width > rail.size().width * 2.0);
    assert!((rail.size().height - 44.0).abs() < 1.0);
    let before = choices.absolute_position().x;
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: LogicalPosition::new(
                rail.absolute_position().x + 40.0,
                rail.absolute_position().y + 22.0,
            ),
            delta_x: -150.0,
            delta_y: 0.0,
        });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(choices.absolute_position().x < before - 1.0);
    for _ in 0..30 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    let saved_x = choices.absolute_position().x;
    app.set_home_devices_enabled(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        ElementHandle::find_by_element_id(&app, "RemotePlayingSection::device_rail")
            .next()
            .is_none()
    );
    assert_eq!(app.get_remote_devices().row_count(), 6);
    app.set_home_devices_enabled(true);
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    assert!(
        element(&app, "RemotePlayingSection::device_rail")
            .size()
            .width
            > 0.0
    );
    assert!(
        (element(&app, "RemotePlayingSection::device_choices")
            .absolute_position()
            .x
            - saved_x)
            .abs()
            < 1.0,
        "device row lost its position when toggled"
    );
    app.set_show_settings(true);
    app.set_show_home(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
    app.set_show_settings(false);
    app.set_show_home(true);
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    assert!(
        (element(&app, "RemotePlayingSection::device_choices")
            .absolute_position()
            .x
            - saved_x)
            .abs()
            < 1.0,
        "device row lost its position when Home was recreated"
    );
    // Native Tab focus must reveal off-screen pills and Return must open that receiver.
    let opened = Rc::new(std::cell::RefCell::new(String::new()));
    let recorded = opened.clone();
    app.on_remote_device_open(move |id| *recorded.borrow_mut() = id.to_string());
    let key = |key: slint::platform::Key| {
        let text: slint::SharedString = key.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    };
    let rail = element(&app, "RemotePlayingSection::device_rail");
    let choices = element(&app, "RemotePlayingSection::device_choices");
    for _ in 0..32 {
        key(slint::platform::Key::Tab);
        if choices.absolute_position().x < rail.absolute_position().x - 1.0 {
            break;
        }
    }
    assert!(
        choices.absolute_position().x < rail.absolute_position().x - 1.0,
        "Tab must scroll an off-screen receiver into view"
    );
    key(slint::platform::Key::Return);
    assert!(opened.borrow().starts_with("device-"));
    for i in 0..devices.row_count() {
        let mut row = devices.row_data(i).unwrap();
        row.has_session = false;
        devices.set_row_data(i, row);
    }
    app.set_remote_has_playing_devices(false);
    assert_hidden(); // The last receiver session ended.
    assert_eq!(app.get_remote_devices().row_count(), 6);
    app.set_remote_devices_open(true);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(
        app.get_remote_devices_open(),
        "idle peers remain accessible in Devices"
    );
}

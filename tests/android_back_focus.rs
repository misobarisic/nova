//! Android Back clears keyboard focus marks without changing desktop behavior.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, platform::WindowEvent};
use std::time::Duration;

fn frame(app: &nova::AppWindow) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    let _ = app.window().size();
}

fn back_sequence(app: &nova::AppWindow) {
    let before = app.get_system_back_request();
    for event in [
        WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        WindowEvent::KeyPressRepeated {
            text: slint::platform::Key::Back.into(),
        },
        WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        assert!(matches!(
            app.window().dispatch_event_with_result(event).unwrap(),
            slint::platform::WindowEventDispatchResult::Accepted
        ));
    }
    assert_eq!(
        app.get_system_back_request(),
        before + 1,
        "a held Back sequence must route exactly one navigation request"
    );
    frame(app);
}

#[test]
fn android_back_clears_both_modes_and_desktop_keyboard_remains_available() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.window().set_size(slint::PhysicalSize::new(390, 840));
    app.set_touch_menus(true);
    app.set_is_android(true);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.show().unwrap();
    frame(&app);

    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsLink").count() > 0,
        "Settings must be mounted before testing Back"
    );

    // Android Back must clear both the page marker and Settings' shared mode.
    app.set_kb_active(true);
    app.global::<nova::SettingsStyle>().set_keyboard_mode(true);
    back_sequence(&app);
    assert!(!app.get_kb_active(), "Android Back clears root marker mode");
    assert!(
        !app.global::<nova::SettingsStyle>().get_keyboard_mode(),
        "Android Back clears Settings' global marker mode"
    );

    // Recreate the stale-global case with Settings absent and root mode false.
    app.set_show_settings(false);
    app.set_show_home(true);
    frame(&app);
    app.set_kb_active(false);
    app.global::<nova::SettingsStyle>().set_keyboard_mode(true);
    back_sequence(&app);
    assert!(
        !app.global::<nova::SettingsStyle>().get_keyboard_mode(),
        "Android Back clears stale Settings mode while Settings is unmounted"
    );
    app.set_show_home(false);
    app.set_show_settings(true);
    frame(&app);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "SettingsLink").count() > 0,
        "Settings must be mounted again after Back"
    );
    assert!(!app.get_kb_active());
    assert!(!app.global::<nova::SettingsStyle>().get_keyboard_mode());

    // Discover used to turn markers back on after the shared capture cleared
    // them. Exercise its search Back handler with the page actually mounted.
    app.set_show_settings(false);
    app.set_discover_search_focused(true);
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "DiscoverPage").count() > 0);
    back_sequence(&app);
    assert!(
        !app.get_kb_active(),
        "Android Discover Back leaves markers off"
    );

    // Detail has its own Back handler as well; its return path must preserve
    // the same pointer-style visual state after the callback settles.
    app.set_discover_search_focused(false);
    app.set_modal_visible(true);
    app.set_selected_title("Back focus test".into());
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "DetailPage").count() > 0);
    back_sequence(&app);
    assert!(
        !app.get_kb_active(),
        "Android Detail Back leaves markers off"
    );
    app.set_modal_visible(false);

    // Back first dismisses the Player settings popup, then closes Player.
    let weak = app.as_weak();
    app.on_close_player(move || weak.upgrade().unwrap().set_player_open(false));
    app.set_player_open(true);
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "PlayerOverlay").count() > 0);
    let gear = ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .expect("Player settings gear must be present");
    let pos = gear.absolute_position();
    let size = gear.size();
    let position = slint::LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
    for event in [
        WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count() > 0);
    back_sequence(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count() == 0);
    assert!(
        app.get_player_open(),
        "first Player Back dismisses only its popup"
    );
    assert!(
        !app.get_kb_active(),
        "Android Player popup Back leaves markers off"
    );
    back_sequence(&app);
    assert!(!app.get_player_open(), "Android Back closes the player");
    assert!(
        !app.get_kb_active(),
        "Android Player Back leaves markers off"
    );

    // Desktop Back preserves an already active keyboard mode, and an arrow
    // still enables it when the user navigates with the keyboard.
    app.set_is_android(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "SettingsLink").count() > 0);
    app.set_touch_menus(false);
    app.set_kb_active(true);
    app.global::<nova::SettingsStyle>().set_keyboard_mode(true);
    back_sequence(&app);
    assert!(
        app.get_kb_active(),
        "desktop Back preserves root marker mode"
    );
    assert!(
        app.global::<nova::SettingsStyle>().get_keyboard_mode(),
        "desktop Back preserves Settings marker mode"
    );

    // Back from Settings landing routes to Home. Reopen Settings before
    // asserting its keyboard mode; Home owns a different marker policy.
    assert!(app.get_show_home() && !app.get_show_settings());
    app.set_show_home(false);
    app.set_show_settings(true);
    frame(&app);
    assert!(ElementHandle::find_by_element_type_name(&app, "SettingsLink").count() > 0);
    frame(&app);
    app.set_kb_active(false);
    app.global::<nova::SettingsStyle>().set_keyboard_mode(false);
    assert!(matches!(
        app.window()
            .dispatch_event_with_result(WindowEvent::KeyPressed {
                text: slint::platform::Key::RightArrow.into(),
            })
            .unwrap(),
        slint::platform::WindowEventDispatchResult::Accepted
    ));
    assert!(app.get_kb_active(), "desktop arrows enable root markers");
    assert!(app.global::<nova::SettingsStyle>().get_keyboard_mode());
}

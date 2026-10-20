//! List-service mode selection remains a draft until the explicit start action.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..16 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn control(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label).find(|element| {
        matches!(
            element.accessible_role(),
            Some(AccessibleRole::Button | AccessibleRole::Combobox)
        )
    })
}

fn reveal(app: &nova::AppWindow, label: &str) -> ElementHandle {
    for attempt in 0..18 {
        if let Some(target) = control(app, label) {
            let position = target.absolute_position();
            let size = target.size();
            if position.y >= 160.0 && position.y + size.height <= 650.0 {
                assert!(size.height >= 44.0);
                assert!(position.x >= 0.0);
                assert!(position.x + size.width <= app.window().size().width as f32 + 0.5);
                return target;
            }
            app.set_settings_scroll_y((app.get_settings_scroll_y() + 300.0 - position.y).min(0.0));
        } else {
            app.set_settings_scroll_y(if attempt == 0 {
                0.0
            } else {
                app.get_settings_scroll_y() - 180.0
            });
        }
        settle(app);
    }
    panic!("cannot reveal list-service control {label}");
}

fn click(app: &nova::AppWindow, label: &str) {
    reveal(app, label).mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}

fn choose(app: &nova::AppWindow, mode_label: &str, index: usize) {
    click(app, mode_label);
    ElementHandle::find_by_element_id(app, "Dropdown::item_touch")
        .nth(index)
        .expect("sync mode popup option")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}

#[test]
fn list_service_modes_require_explicit_start_and_reset_pending_consent() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::list_service_modes_require_explicit_start_and_reset_pending_consent"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    let connected = Rc::new(VecModel::from(vec![true, true]));
    let whole_library = Rc::new(VecModel::from(vec![false, false]));
    let busy = Rc::new(VecModel::from(vec![false, false]));
    let accounts = Rc::new(VecModel::from(vec![
        "simkl-viewer".into(),
        "mdblist-viewer".into(),
    ]));
    app.set_list_services_connected(connected.clone().into());
    app.set_list_services_whole_library(whole_library.clone().into());
    app.set_list_services_busy(busy.clone().into());
    app.set_list_services_account(accounts.clone().into());
    app.window().set_size(slint::PhysicalSize::new(360, 900));
    app.window().show().unwrap();
    settle(&app);

    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let scopes = whole_library.clone();
    app.on_list_services_scope_changed(move |service, full| {
        recorded
            .borrow_mut()
            .push(format!("scope:{service}:{full}"));
        scopes.set_row_data(service as usize, full);
    });
    let recorded = events.clone();
    app.on_list_services_sync(move |service| {
        recorded.borrow_mut().push(format!("sync:{service}"));
    });
    let recorded = events.clone();
    let pending = busy.clone();
    app.on_list_services_cancel(move |service| {
        recorded.borrow_mut().push(format!("cancel:{service}"));
        pending.set_row_data(service as usize, false);
    });
    let recorded = events.clone();
    app.on_list_services_disconnect(move |service| {
        recorded.borrow_mut().push(format!("disconnect:{service}"));
    });

    for (language, mode, start, sync_now, cancel, disconnect) in [
        (
            "en",
            "Sync mode",
            "Start full-library sync",
            "Sync now",
            "Cancel",
            "Disconnect",
        ),
        (
            "hr",
            "Način sinkronizacije",
            "Pokreni sinkronizaciju cijele zbirke",
            "Sinkronizirajte sada",
            "Odustanite",
            "Prekinite vezu",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [360, 1280] {
            app.set_is_android(width == 360);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            for (service, name) in [(0, "Simkl"), (1, "MDBList")] {
                let mode_label = format!("{name} · {mode}");
                let before = events.borrow().len();
                click(&app, name);
                reveal(&app, &mode_label);
                assert_eq!(events.borrow().len(), before, "viewing mode is not consent");

                choose(&app, &mode_label, 0);
                assert_eq!(events.borrow().len(), before, "full mode is only a draft");
                assert!(!whole_library.row_data(service).unwrap());
                click(&app, start);
                assert_eq!(
                    &events.borrow()[before..],
                    &[format!("scope:{service}:true")]
                );
                choose(&app, &mode_label, 1);
                assert_eq!(
                    events.borrow().last(),
                    Some(&format!("scope:{service}:false"))
                );
                assert!(!whole_library.row_data(service).unwrap());

                // Reverting an unstarted draft does not touch durable scope.
                let before = events.borrow().len();
                choose(&app, &mode_label, 0);
                choose(&app, &mode_label, 1);
                click(&app, sync_now);
                assert_eq!(&events.borrow()[before..], &[format!("sync:{service}")]);

                // A new account must not inherit an unstarted full-library grant.
                choose(&app, &mode_label, 0);
                accounts.set_row_data(
                    service,
                    format!("{language}:{width}:{service}:new-account").into(),
                );
                settle(&app);
                click(&app, sync_now);
                assert_eq!(events.borrow().last(), Some(&format!("sync:{service}")));

                // The mode popup may already be open when the actor becomes
                // busy; completing that gesture must not revoke effective full
                // mode. Cancellation is the dedicated escape hatch.
                whole_library.set_row_data(service, true);
                settle(&app);
                click(&app, &mode_label);
                busy.set_row_data(service, true);
                settle(&app);
                let before = events.borrow().len();
                ElementHandle::find_by_element_id(&app, "Dropdown::item_touch")
                    .nth(1)
                    .expect("previously opened selected-titles option")
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert!(whole_library.row_data(service).unwrap());
                assert_eq!(events.borrow().len(), before);
                assert_eq!(reveal(&app, &mode_label).accessible_enabled(), Some(false));
                click(&app, cancel);
                whole_library.set_row_data(service, false);
                settle(&app);

                choose(&app, &mode_label, 0);
                busy.set_row_data(service, true);
                settle(&app);
                click(&app, cancel);
                click(&app, sync_now);
                assert_eq!(events.borrow().last(), Some(&format!("sync:{service}")));

                // Clear before dispatch as well as on actor publications: a
                // delayed disconnect result cannot leave a start grant behind.
                choose(&app, &mode_label, 0);
                click(&app, disconnect);
                click(&app, sync_now);
                assert_eq!(events.borrow().last(), Some(&format!("sync:{service}")));
                connected.set_row_data(service, false);
                settle(&app);
                assert_eq!(reveal(&app, &mode_label).accessible_enabled(), Some(false));
                connected.set_row_data(service, true);
                settle(&app);

                // Keep only the service under test expanded; the generic
                // action labels then always resolve to that account's button.
                click(&app, name);
            }
        }
    }
}

#[test]
fn integrations_keeps_empty_notices_compact_and_hosts_the_synced_tmdb_editor() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::integrations_keeps_empty_notices_compact_and_hosts_the_synced_tmdb_editor"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    app.window().set_size(slint::PhysicalSize::new(390, 4000));
    app.show().unwrap();
    settle(&app);
    let edited = Rc::new(RefCell::new(Vec::new()));
    let events = edited.clone();
    app.on_settings_edited(move |key| events.borrow_mut().push(key.to_string()));
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 4000));
        app.set_settings_scroll_y(0.0);
        settle(&app);
        let header = control(&app, "Simkl").unwrap();
        header.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        let header_bottom = header.absolute_position().y + header.size().height;
        let mode = control(&app, "Simkl · Sync mode").unwrap();
        let gap = mode.absolute_position().y - header_bottom;
        assert!(
            gap < 100.0,
            "empty notices must not reserve a line above the sync control: {width}: {gap}"
        );
        let key = ElementHandle::find_by_accessible_label(&app, "TMDB API key (v3)")
            .find(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
            .expect("TMDB key field under Integrations");
        assert!(key.size().height >= 44.0);
        let position = key.absolute_position();
        assert!(position.x >= 0.0 && position.x + key.size().width <= width as f32 + 0.5);
        key.set_accessible_value("0123456789abcdef0123456789abcdef");
        settle(&app);
        assert_eq!(app.get_tmdb_api_key(), "0123456789abcdef0123456789abcdef");
        assert_eq!(
            app.global::<nova::SettingsBridge>().get_tmdb_api_key(),
            app.get_tmdb_api_key()
        );
        assert_eq!(
            edited.borrow().last().map(String::as_str),
            Some("tmdb_api_key")
        );
        header.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
    }
}

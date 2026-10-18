//! Service switching and sync consent use the real compiled leaves and pointer
//! gestures, including the horizontally clipped selector on compact phones.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

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

fn button(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
}

fn scroll(app: &nova::AppWindow, viewport: &ElementHandle, dx: f32, dy: f32) {
    let position = viewport.absolute_position();
    let size = viewport.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(
                position.x + size.width / 2.0,
                position.y + size.height / 2.0,
            ),
            delta_x: dx,
            delta_y: dy,
        });
    settle(app);
}

fn service_button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    let rail = ElementHandle::find_by_element_id(app, "TrackingPanel::service_rail")
        .next()
        .expect("horizontal service selector");
    scroll(app, &rail, 10_000.0, 0.0);
    for _ in 0..8 {
        if let Some(target) = button(app, label) {
            let left = rail.absolute_position().x;
            let right = left + rail.size().width;
            let position = target.absolute_position();
            let size = target.size();
            if position.x >= left - 0.5 && position.x + size.width <= right + 0.5 {
                assert!(size.width >= 44.0 && size.height >= 44.0);
                assert!(position.y >= 0.0);
                return target;
            }
        }
        scroll(app, &rail, -100.0, 0.0);
    }
    panic!("service {label} cannot be revealed by horizontal scrolling");
}

fn tracking_fixture() -> nova::AppWindow {
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_selected_title("A release with native episode identities".into());
    app.set_tracking_open(true);
    app.window().set_size(slint::PhysicalSize::new(360, 900));
    app.window().show().unwrap();
    settle(&app);
    app
}

#[test]
fn service_rail_reveals_every_destination_and_routes_its_own_callback() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::service_rail_reveals_every_destination_and_routes_its_own_callback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = tracking_fixture();
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.on_tracking_search(move |service, query| {
        recorded
            .borrow_mut()
            .push(format!("anime:{service}:{query}"));
    });
    let recorded = events.clone();
    app.on_stremio_title_search(move |query| {
        recorded.borrow_mut().push(format!("stremio:{query}"));
    });
    let recorded = events.clone();
    app.on_list_services_title_open(move |service| {
        recorded.borrow_mut().push(format!("list:{service}"));
    });

    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        for width in [360, 1280] {
            app.set_is_android(width == 360);
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            // Start elsewhere so the first MAL click is also a real change.
            app.set_tracking_service(1);
            settle(&app);
            events.borrow_mut().clear();
            for (service, label, expected) in [
                (0, "MyAnimeList", "anime:0:"),
                (1, "AniList", "anime:1:"),
                (2, "Stremio", "stremio:"),
                (3, "Simkl", "list:0"),
                (4, "MDBList", "list:1"),
            ] {
                let before = events.borrow().len();
                let target = service_button(&app, label);
                assert_eq!(
                    events.borrow().len(),
                    before,
                    "scrolling must not select a service"
                );
                target.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(
                    app.get_tracking_service(),
                    service,
                    "{language} {width}: {label}"
                );
                assert_eq!(&events.borrow()[before..], &[expected.to_owned()]);
            }
            service_button(&app, "MyAnimeList")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert_eq!(app.get_tracking_service(), 0);
            assert_eq!(events.borrow().last().unwrap(), "anime:0:");
        }
    }
}

fn scope_dropdown(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "StremioSettings")
        .next()?
        .query_descendants()
        .match_inherits("Dropdown")
        .find_first()
}

fn reveal_scope_dropdown(app: &nova::AppWindow) -> ElementHandle {
    for attempt in 0..12 {
        if let Some(control) = scope_dropdown(app) {
            let position = control.absolute_position();
            let size = control.size();
            if position.y >= 140.0 && position.y + size.height < 760.0 {
                return control;
            }
            app.set_settings_scroll_y((app.get_settings_scroll_y() + 320.0 - position.y).min(0.0));
        } else {
            app.set_settings_scroll_y(if attempt == 0 {
                0.0
            } else {
                app.get_settings_scroll_y() - 160.0
            });
        }
        settle(app);
    }
    panic!("Stremio sync mode cannot be reached");
}

#[test]
fn both_scope_options_are_explicit_preferences_and_never_start_a_merge() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::both_scope_options_are_explicit_preferences_and_never_start_a_merge"
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
    app.set_stremio_connected(true);
    app.set_stremio_account("viewer@example.invalid".into());
    app.set_stremio_preview("Review the initial library merge".into());
    app.set_stremio_merge_ready(true);
    app.window().set_size(slint::PhysicalSize::new(360, 900));
    app.window().show().unwrap();
    settle(&app);
    let modes = Rc::new(RefCell::new(Vec::new()));
    let recorded = modes.clone();
    let weak = app.as_weak();
    app.on_stremio_mode_changed(move |mode| {
        recorded.borrow_mut().push(mode);
        weak.upgrade().unwrap().set_stremio_mode_index(mode);
    });
    let starts = Rc::new(Cell::new(0));
    let recorded = starts.clone();
    app.on_stremio_start(move || recorded.set(recorded.get() + 1));
    let syncs = Rc::new(Cell::new(0));
    let recorded = syncs.clone();
    app.on_stremio_sync(move || recorded.set(recorded.get() + 1));

    // Opening an account disclosure exposes preferences without granting sync.
    button(&app, "Stremio")
        .expect("Stremio account disclosure")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        for width in [360, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            settle(&app);
            for index in [1, 0] {
                let dropdown = reveal_scope_dropdown(&app);
                let position = dropdown.absolute_position();
                let size = dropdown.size();
                assert!(position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5);
                assert!(size.height >= 44.0);
                dropdown.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                // IDs stay stable when translated option labels change.
                ElementHandle::find_by_element_id(&app, "Dropdown::item_touch")
                    .nth(index)
                    .expect("scope popup option")
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(modes.borrow().last().copied(), Some(index as i32));
                assert_eq!(app.get_stremio_mode_index(), index as i32);
                assert_eq!(
                    starts.get(),
                    0,
                    "changing scope must not start the initial merge"
                );
                assert_eq!(
                    syncs.get(),
                    0,
                    "changing scope must not request an account sync"
                );
            }
        }
    }
    assert_eq!(&*modes.borrow(), &[1, 0, 1, 0, 1, 0, 1, 0]);
}

#[test]
fn title_sync_requires_the_enable_button_for_each_service() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::title_sync_requires_the_enable_button_for_each_service"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = tracking_fixture();
    app.set_stremio_connected(true);
    app.set_stremio_mode_index(1);
    let stremio = Rc::new(RefCell::new(Vec::new()));
    let recorded = stremio.clone();
    app.on_stremio_title_confirm(move |history| recorded.borrow_mut().push(history));
    let lists = Rc::new(RefCell::new(Vec::new()));
    let recorded = lists.clone();
    app.on_list_services_title_select(move |service, enabled, history| {
        recorded.borrow_mut().push((service, enabled, history));
    });
    for width in [360, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.set_tracking_service(2);
        app.set_stremio_title_can_confirm(false);
        app.set_stremio_title_match("Confirmed Stremio identity".into());
        settle(&app);
        assert!(button(&app, "Enable Stremio for this title").is_none());
        let before = stremio.borrow().len();
        app.set_stremio_title_can_confirm(true);
        settle(&app);
        assert_eq!(
            stremio.borrow().len(),
            before,
            "a match alone is not consent"
        );
        button(&app, "Enable Stremio for this title")
            .expect("explicit Stremio enable action")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(&stremio.borrow()[before..], &[false]);

        for service in [0, 1] {
            app.set_tracking_service(service + 3);
            app.set_list_services_title_connected(true);
            app.set_list_services_title_selected(false);
            app.set_list_services_title_resolved(false);
            settle(&app);
            let action = button(&app, "Start syncing this title").expect("explicit list enable");
            assert_eq!(action.accessible_enabled(), Some(false));
            let before = lists.borrow().len();
            app.set_list_services_title_resolved(true);
            settle(&app);
            assert_eq!(
                lists.borrow().len(),
                before,
                "resolving identity must not opt in"
            );
            let action = button(&app, "Start syncing this title").unwrap();
            assert_eq!(action.accessible_enabled(), Some(true));
            action.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert_eq!(&lists.borrow()[before..], &[(service, true, false)]);
        }
    }
}

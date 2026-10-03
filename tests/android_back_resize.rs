//! Android Back must remain routed after resizing a focused editor.

// Built-in interpreter widgets omit the element metadata used to prove
// input focus here. Exercise the compiled UI shipped on Android instead.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let mut scheduled = false;
    std::future::poll_fn(move |cx| {
        if ready.get() {
            return std::task::Poll::Ready(());
        }
        if !scheduled {
            scheduled = true;
            let ready = ready.clone();
            let waker = cx.waker().clone();
            slint::Timer::single_shot(Duration::from_millis(350), move || {
                ready.set(true);
                waker.wake();
            });
        }
        std::task::Poll::Pending
    })
    .await;
}

fn check(failures: &RefCell<Vec<String>>, condition: bool, message: &str) {
    if !condition {
        failures.borrow_mut().push(message.to_string());
    }
}

async fn type_text(app: &nova::AppWindow, text: &str) -> bool {
    let mut accepted = true;
    for character in text.chars() {
        let text: SharedString = character.to_string().into();
        accepted &= matches!(
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
                    text: text.clone()
                }),
            Ok(slint::platform::WindowEventDispatchResult::Accepted)
        );
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    }
    settle().await;
    accepted
}

async fn back(app: &nova::AppWindow, failures: &RefCell<Vec<String>>, phase: &str) {
    let before = app.get_system_back_request();
    for (event, kind) in [
        (
            slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            },
            "press",
        ),
        (
            slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            },
            "release",
        ),
    ] {
        let result = app.window().dispatch_event_with_result(event);
        check(
            failures,
            matches!(
                &result,
                Ok(slint::platform::WindowEventDispatchResult::Accepted)
            ),
            &format!("{phase}: Back {kind} must be accepted, got {result:?}"),
        );
    }
    settle().await;
    check(
        failures,
        app.get_system_back_request() == before + 1,
        &format!("{phase}: one Back must dispatch exactly one navigation request"),
    );
}

async fn open_settings(
    app: &nova::AppWindow,
    title: &str,
    failures: &RefCell<Vec<String>>,
) -> bool {
    for _ in 0..3 {
        let link = ElementHandle::find_by_element_type_name(app, "SettingsLink").find(|link| {
            let title = title.to_owned();
            link.query_descendants()
                .match_predicate(move |element| {
                    element.accessible_label().as_deref() == Some(title.as_str())
                })
                .find_first()
                .is_some()
        });
        if let Some(link) = link {
            link.single_click(slint::platform::PointerEventButton::Left)
                .await;
            settle().await;
            let opened = if title == "Tracking" {
                ElementHandle::find_by_element_type_name(app, "TrackingSettings").count() == 1
            } else {
                ElementHandle::find_by_element_id(app, "SettingsPage::addon_url_narrow")
                    .next()
                    .is_some()
            };
            check(
                failures,
                opened,
                &format!("{title} settings row must open its subpage"),
            );
            return opened;
        }
        let from = slint::LogicalPosition::new(220.0, 680.0);
        let to = slint::LogicalPosition::new(220.0, 180.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: from,
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position: to });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: to,
                button: slint::platform::PointerEventButton::Left,
            });
        settle().await;
    }
    failures.borrow_mut().push(format!(
        "missing visible {title} settings row after scrolling"
    ));
    false
}

#[test]
fn focused_editors_keep_back_navigation_after_rotation() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.set_is_android(true);
    app.set_discover_scroll_page(true);
    app.set_show_home(false);
    app.set_searchable(true);
    let names = |name: &str| Rc::new(VecModel::from(vec![SharedString::from(name)])).into();
    app.set_search_addon_names(names("All addons"));
    app.set_search_type_names(names("All types"));
    app.set_search_catalog_names(names("All catalogs"));
    app.set_search_genre_names(names("All genres"));
    app.set_tracking_accounts(
        Rc::new(VecModel::from(vec![nova::TrackingAccountRow {
            service: 0,
            name: "MyAnimeList account".into(),
            client_id: "public-client".into(),
            redirect_uri: "http://127.0.0.1:53926/callback".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let weak = app.as_weak();
    app.on_search_activated(move || {
        weak.upgrade()
            .unwrap()
            .set_discover_search_filters_open(true);
    });
    let search_backs = Rc::new(Cell::new(0));
    let recorded = search_backs.clone();
    let weak = app.as_weak();
    app.on_search_back_picked(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_discover_search_open(false);
        app.set_discover_search_filters_open(false);
        app.set_discover_search_focused(false);
    });
    let homes = Rc::new(Cell::new(0));
    let recorded = homes.clone();
    let weak = app.as_weak();
    app.on_home_picked(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_show_settings(false);
        app.set_show_home(true);
    });
    let backgrounds = Rc::new(Cell::new(0));
    let recorded = backgrounds.clone();
    app.on_exit_to_background(move || recorded.set(recorded.get() + 1));
    app.show().unwrap();

    let failures = Rc::new(RefCell::new(Vec::new()));
    let recorded_failures = failures.clone();
    let weak = app.as_weak();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        let failures = &recorded_failures;
        settle().await;
        let Some(field) = ElementHandle::find_by_element_type_name(&app, "SearchField").next()
        else {
            failures.borrow_mut().push("missing Discover SearchField".into());
            slint::quit_event_loop().unwrap();
            return;
        };
        let Some(input) = field
            .query_descendants()
            .match_predicate(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .find_first()
        else {
            failures.borrow_mut().push("missing Discover TextInput".into());
            slint::quit_event_loop().unwrap();
            return;
        };
        input
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        let typed = type_text(&app, "xy").await;
        let focused = typed && app.get_search_text() == "xy" && app.get_discover_search_focused();
        check(failures, focused, "Discover editor must genuinely receive typed text before rotation");
        if !focused {
            slint::quit_event_loop().unwrap();
            return;
        }
        app.window().set_size(slint::PhysicalSize::new(900, 390));
        settle().await;
        check(failures, app.get_search_text() == "xy", "rotation must retain the Discover query");
        back(&app, failures, "Discover after rotation").await;
        check(
            failures,
            search_backs.get() == 1 && homes.get() == 0 && backgrounds.get() == 0
                && app.get_search_text().is_empty() && !app.get_discover_search_filters_open(),
            "first Discover Back after rotation must dismiss search without leaving the app",
        );
        back(&app, failures, "Discover landing after rotation").await;
        check(failures, homes.get() == 1 && backgrounds.get() == 0 && app.get_show_home(), "next Discover Back must return to Home only");

        app.window().set_size(slint::PhysicalSize::new(390, 900));
        app.set_show_home(false);
        app.set_show_settings(true);
        settle().await;
        if !open_settings(&app, "Tracking", failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        let Some(input) = ElementHandle::find_by_accessible_label(&app, "Public client ID")
            .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
        else {
            failures.borrow_mut().push("missing Public client ID TextInput".into());
            slint::quit_event_loop().unwrap();
            return;
        };
        let before = input.accessible_value();
        input
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        let typed = type_text(&app, "x").await;
        let after = input.accessible_value();
        let focused = typed && before != after && after.as_ref().is_some_and(|value| value.contains('x'));
        check(failures, focused, &format!("Public client ID must genuinely receive text before rotation: before={before:?}, after={after:?}"));
        if !focused {
            slint::quit_event_loop().unwrap();
            return;
        }
        app.window().set_size(slint::PhysicalSize::new(900, 390));
        settle().await;
        back(&app, failures, "Tracking input after rotation").await;
        check(failures, ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 1 && homes.get() == 1 && backgrounds.get() == 0, "first Settings Back must blur the editor and retain Tracking");
        back(&app, failures, "Tracking subpage after rotation").await;
        check(failures, ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 0 && app.get_show_settings() && homes.get() == 1 && backgrounds.get() == 0, "next Settings Back must return to its landing without leaving the app");
        back(&app, failures, "Settings landing after rotation").await;
        check(failures, homes.get() == 2 && app.get_show_home() && backgrounds.get() == 0, "next Settings Back must return to Home only");
        back(&app, failures, "Home root after rotation").await;
        check(failures, backgrounds.get() == 1, "only a separate Back on Home root may background the app");

        // Addons uses distinct input instances in its stacked/wide layouts.
        // A normal touch edit (not keyboard edit mode) must survive that swap.
        app.window().set_size(slint::PhysicalSize::new(390, 900));
        app.set_show_home(false);
        app.set_show_settings(true);
        settle().await;
        if !open_settings(&app, "Addons", failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        let field = ElementHandle::find_by_element_id(&app, "SettingsPage::addon_url_narrow").next().unwrap();
        field.single_click(slint::platform::PointerEventButton::Left).await;
        settle().await;
        let focused = type_text(&app, "xy").await && app.get_addon_url() == "xy";
        check(failures, focused, "touch-focused addon URL must receive text before rotation");
        if !focused {
            slint::quit_event_loop().unwrap();
            return;
        }
        app.window().set_size(slint::PhysicalSize::new(900, 390));
        settle().await;
        check(failures, app.get_addon_url() == "xy", "rotation must retain the addon URL");
        back(&app, failures, "Addon input after rotation").await;
        check(failures, app.get_show_settings() && homes.get() == 2 && backgrounds.get() == 1, "Back after addon input replacement must remain inside Settings");
        back(&app, failures, "Addon landing after rotation").await;
        check(failures, app.get_show_home() && homes.get() == 3 && backgrounds.get() == 1, "the next Back after Addons closes must return to Home only");
        back(&app, failures, "Home after Addons").await;
        check(failures, backgrounds.get() == 2, "only a separate Home Back may background after Addons");
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "Back resize failures:\n  {}",
        failures.borrow().join("\n  ")
    );
}

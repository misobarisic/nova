//! Settings Back must survive removing a blurred or focused tracking input.

// Built-in interpreter widgets omit the element metadata used to prove
// input focus here. Exercise the compiled UI shipped on Android instead.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let timer_ready = ready.clone();
    let timer_waker = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(350), move || {
        timer_ready.set(true);
        if let Some(waker) = timer_waker.borrow_mut().take() {
            waker.wake();
        }
    });
    std::future::poll_fn(|cx| {
        if ready.get() {
            std::task::Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
    .await;
}

fn check(failures: &RefCell<Vec<String>>, condition: bool, message: &str) {
    if !condition {
        failures.borrow_mut().push(message.to_string());
    }
}

fn tracking_link(app: &nova::AppWindow) -> Option<ElementHandle> {
    // SettingsLink has no button role yet; identify the actual row through
    // its title descendant, then click the row instead of its Text caption.
    ElementHandle::find_by_element_type_name(app, "SettingsLink").find(|link| {
        link.query_descendants()
            .match_predicate(|element| element.accessible_label().as_deref() == Some("Tracking"))
            .find_first()
            .is_some()
    })
}

async fn open_tracking(app: &nova::AppWindow, failures: &RefCell<Vec<String>>) -> bool {
    // Tracking follows Downloads in the landing list; keep phone dimensions
    // and reveal it by scrolling instead of enlarging the test viewport.
    for _ in 0..3 {
        if let Some(link) = tracking_link(app) {
            link.single_click(slint::platform::PointerEventButton::Left)
                .await;
            settle().await;
            let opened =
                ElementHandle::find_by_element_type_name(app, "TrackingSettings").count() == 1;
            check(
                failures,
                opened,
                "the Tracking settings row must open its subpage",
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
    failures
        .borrow_mut()
        .push("missing visible Tracking settings row after scrolling".to_string());
    false
}

async fn focus_client_input(app: &nova::AppWindow, failures: &RefCell<Vec<String>>) -> bool {
    let Some(input) = ElementHandle::find_by_accessible_label(app, "Public client ID")
        .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
    else {
        failures
            .borrow_mut()
            .push("missing Public client ID text-input control".to_string());
        return false;
    };
    let before = input.accessible_value();
    input
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
    // A text key must reach the editor; this guards against accidentally
    // clicking the nearby caption rather than the LineEdit itself.
    let typed = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed { text: "x".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
    settle().await;
    let after = input.accessible_value();
    let focused = matches!(
        &typed,
        Ok(slint::platform::WindowEventDispatchResult::Accepted)
    ) && before != after
        && after.as_ref().is_some_and(|value| value.contains('x'));
    if !focused {
        failures.borrow_mut().push(format!(
            "Public client ID must accept typed text before Back: result={typed:?}, before={before:?}, after={after:?}, position={:?}, size={:?}",
            input.absolute_position(),
            input.size(),
        ));
    }
    focused
}

async fn back(app: &nova::AppWindow, failures: &RefCell<Vec<String>>) {
    let before = app.get_system_back_request();
    for (event, message) in [
        (
            slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            },
            "Back press must be accepted before Android considers finishing the activity",
        ),
        (
            slint::platform::WindowEvent::KeyPressRepeated {
                text: slint::platform::Key::Back.into(),
            },
            "held Back repeat must be accepted without leaving another Settings layer",
        ),
        (
            slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            },
            "Back release must be accepted after a Settings input loses focus",
        ),
    ] {
        let result = app.window().dispatch_event_with_result(event);
        check(
            failures,
            matches!(
                result,
                Ok(slint::platform::WindowEventDispatchResult::Accepted)
            ),
            message,
        );
    }
    settle().await;
    check(
        failures,
        app.get_system_back_request() == before + 1,
        "each held Back must dispatch exactly one navigation request",
    );
}

#[test]
fn tracking_settings_back_blurs_then_returns_to_landing_and_home() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
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

    let home_picks = Rc::new(Cell::new(0usize));
    let recorded_home_picks = home_picks.clone();
    let weak = app.as_weak();
    app.on_home_picked(move || {
        recorded_home_picks.set(recorded_home_picks.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_show_settings(false);
        app.set_show_home(true);
    });
    let backgrounds = Rc::new(Cell::new(0usize));
    let recorded_backgrounds = backgrounds.clone();
    app.on_exit_to_background(move || recorded_backgrounds.set(recorded_backgrounds.get() + 1));

    let failures = Rc::new(RefCell::new(Vec::new()));
    let recorded_failures = failures.clone();
    let weak = app.as_weak();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        let failures = &recorded_failures;
        settle().await;
        if !open_tracking(&app, failures).await || !focus_client_input(&app, failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }

        back(&app, failures).await;
        check(
            failures,
            ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 1
                && app.get_show_settings()
                && home_picks.get() == 0
                && backgrounds.get() == 0,
            "first Back must blur the tracking input while retaining its Settings subpage",
        );
        back(&app, failures).await;
        check(
            failures,
            ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 0
                && app.get_show_settings()
                && home_picks.get() == 0
                && backgrounds.get() == 0,
            "second Back must close Tracking to Settings landing without leaving the app",
        );
        back(&app, failures).await;
        check(
            failures,
            !app.get_show_settings()
                && app.get_show_home()
                && home_picks.get() == 1
                && backgrounds.get() == 0,
            "Back after the tracking blur scope disappears must return Settings landing to Home",
        );
        back(&app, failures).await;
        check(
            failures,
            backgrounds.get() == 1,
            "only a separate Back on Home root may background the app",
        );

        // Pointer Back follows the same nearest-editor rule as system Back.
        app.set_show_home(false);
        app.set_show_settings(true);
        settle().await;
        if !open_tracking(&app, failures).await || !focus_client_input(&app, failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        let Some(top_back) = ElementHandle::find_by_accessible_label(&app, "Back")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
        else {
            failures
                .borrow_mut()
                .push("missing Settings subpage top-left Back".to_string());
            slint::quit_event_loop().unwrap();
            return;
        };
        top_back
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        check(
            failures,
            ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 1,
            "top-left Back must blur the input before closing Tracking",
        );
        back(&app, failures).await;
        check(
            failures,
            ElementHandle::find_by_element_type_name(&app, "TrackingSettings").count() == 0
                && app.get_show_settings()
                && home_picks.get() == 1
                && backgrounds.get() == 1,
            "Back after pointer blur must close Tracking while retaining Settings landing",
        );
        back(&app, failures).await;
        check(
            failures,
            app.get_show_home() && home_picks.get() == 2 && backgrounds.get() == 1,
            "Back after top-left page dismissal must return to Home without another screen tap",
        );
        back(&app, failures).await;
        check(
            failures,
            backgrounds.get() == 2,
            "Home root Back must still work after top-left Settings dismissal",
        );
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "Settings input Back failures:\n  {}",
        failures.borrow().join("\n  ")
    );
}

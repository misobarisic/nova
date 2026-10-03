//! Android Back must keep working after focused tracking controls disappear.
//!
//! Dispatch real key events: incrementing `system_back_request` directly skips
//! the window capture path and cannot catch focus lost when a sheet closes.

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

async fn click(app: &nova::AppWindow, label: &str, failures: &RefCell<Vec<String>>) -> bool {
    let role = if matches!(label, "First source row" | "Tracker search") {
        AccessibleRole::TextInput
    } else {
        AccessibleRole::Button
    };
    let Some(element) = ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(role))
    else {
        failures
            .borrow_mut()
            .push(format!("missing tracking control: {label}"));
        return false;
    };
    let before = element.accessible_value();
    element
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
    if role == AccessibleRole::TextInput {
        let typed =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
                    text: "x".into(),
                });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
        settle().await;
        let after = element.accessible_value();
        let focused = matches!(
            &typed,
            Ok(slint::platform::WindowEventDispatchResult::Accepted)
        ) && before != after
            && after.as_ref().is_some_and(|value| value.contains('x'));
        if !focused {
            failures.borrow_mut().push(format!(
                "{label} must accept typed text before Back: result={typed:?}, before={before:?}, after={after:?}, position={:?}, size={:?}",
                element.absolute_position(),
                element.size(),
            ));
        }
        return focused;
    }
    true
}

async fn back(app: &nova::AppWindow, repeat: bool, failures: &RefCell<Vec<String>>) {
    let request_before = app.get_system_back_request();
    let pressed =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            });
    check(
        failures,
        matches!(
            pressed,
            Ok(slint::platform::WindowEventDispatchResult::Accepted)
        ),
        "Back press must be accepted so Android cannot run its native finish fallback",
    );
    if repeat {
        let repeated = app.window().dispatch_event_with_result(
            slint::platform::WindowEvent::KeyPressRepeated {
                text: slint::platform::Key::Back.into(),
            },
        );
        check(
            failures,
            matches!(
                repeated,
                Ok(slint::platform::WindowEventDispatchResult::Accepted)
            ),
            "Back repeat must be accepted without popping another layer or finishing the activity",
        );
    }
    let released =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            });
    check(
        failures,
        matches!(
            released,
            Ok(slint::platform::WindowEventDispatchResult::Accepted)
        ),
        "Back release must remain accepted after the focused sheet or page disappears",
    );
    settle().await;
    check(
        failures,
        app.get_system_back_request() == request_before + 1,
        "each Back press must reach window capture once; repeats/releases must not add requests",
    );
}

#[test]
fn back_keeps_popping_layers_after_tracking_focus_is_removed() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.window().show().unwrap();
    app.set_in_library(true);
    app.set_selected_title("An anime title".into());
    app.set_modal_episodes(true);
    app.set_tracking_setup_active(true);
    app.set_tracking_setup_revision("proposal-1".into());
    app.set_tracking_setup_summary("1 release · 12 episodes to link".into());
    app.set_tracking_setup(
        Rc::new(VecModel::from(vec![nova::TrackingSetupRow {
            title: "A suggested release".into(),
            season: "Season 1".into(),
            coverage: "Episodes 1–12 → tracker episodes 1–12".into(),
            source_range: "1–12".into(),
            target_range: "1–12".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_tracking_range_first("1".into());
    app.set_tracking_range_last("12".into());
    app.set_tracking_range_start("1".into());

    let tracking_closes = Rc::new(Cell::new(0usize));
    let recorded_closes = tracking_closes.clone();
    let weak = app.as_weak();
    app.on_tracking_close(move || {
        recorded_closes.set(recorded_closes.get() + 1);
        weak.upgrade().unwrap().set_tracking_open(false);
    });
    let cancelled_adjustments = Rc::new(Cell::new(0usize));
    let recorded_cancellations = cancelled_adjustments.clone();
    let weak = app.as_weak();
    app.on_tracking_cancel_adjust(move || {
        recorded_cancellations.set(recorded_cancellations.get() + 1);
        weak.upgrade()
            .unwrap()
            .set_tracking_candidate_title("".into());
    });
    let detail_closes = Rc::new(Cell::new(0usize));
    let recorded_detail_closes = detail_closes.clone();
    app.on_modal_closed(move || recorded_detail_closes.set(recorded_detail_closes.get() + 1));
    let backgrounds = Rc::new(Cell::new(0usize));
    let recorded_backgrounds = backgrounds.clone();
    app.on_exit_to_background(move || recorded_backgrounds.set(recorded_backgrounds.get() + 1));
    let home_picks = Rc::new(Cell::new(0usize));
    let recorded_home_picks = home_picks.clone();
    let weak = app.as_weak();
    app.on_home_picked(move || {
        recorded_home_picks.set(recorded_home_picks.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_show_library(false);
        app.set_show_settings(false);
        app.set_show_home(true);
    });

    let failures = Rc::new(RefCell::new(Vec::new()));
    let recorded_failures = failures.clone();
    let weak = app.as_weak();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        let failures = &recorded_failures;
        settle().await;

        // Opening focuses the sheet's Close button. Hiding it must restore
        // focus to details, even though no pointer event follows dismissal.
        app.set_modal_visible(true);
        settle().await;
        app.set_tracking_open(true);
        settle().await;
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_tracking_open() && app.get_modal_visible(),
            "Back must dismiss tracking before details",
        );
        check(
            failures,
            tracking_closes.get() == 1 && detail_closes.get() == 0 && backgrounds.get() == 0,
            "a held Back in tracking must pop exactly one layer",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 1 && backgrounds.get() == 0,
            "the next Back must close details without backgrounding newly recreated Home",
        );
        back(&app, true, failures).await;
        check(
            failures,
            backgrounds.get() == 1,
            "only a separate Back at Home root may background the app",
        );

        // Clicking Close explicitly focuses that button before it disappears.
        app.set_modal_visible(true);
        settle().await;
        app.set_tracking_open(true);
        settle().await;
        if !click(&app, "Close", failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        check(
            failures,
            !app.get_tracking_open() && app.get_modal_visible() && tracking_closes.get() == 2,
            "Close must dismiss tracking while leaving details open",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 2 && backgrounds.get() == 1,
            "Back after clicking tracking Close must close details, not leave the app",
        );
        back(&app, false, failures).await;
        check(
            failures,
            backgrounds.get() == 2,
            "Home root Back must still work after Close",
        );

        // Alignment cancellation removes the focused LineEdit. Subsequent
        // Back presses must still reach the review sheet and then details.
        app.set_modal_visible(true);
        settle().await;
        app.set_tracking_open(true);
        app.set_tracking_candidate_title("A suggested release".into());
        settle().await;
        if !click(&app, "First source row", failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        back(&app, true, failures).await;
        check(
            failures,
            app.get_tracking_candidate_title().is_empty()
                && app.get_tracking_open()
                && app.get_modal_visible(),
            "Back in alignment must return to review while retaining tracking and details",
        );
        check(
            failures,
            cancelled_adjustments.get() == 1
                && tracking_closes.get() == 2
                && detail_closes.get() == 2
                && backgrounds.get() == 2,
            "a held Back on the alignment input must only cancel its adjustment",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_tracking_open()
                && app.get_modal_visible()
                && tracking_closes.get() == 3
                && backgrounds.get() == 2,
            "Back after alignment input removal must dismiss tracking only",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 3 && backgrounds.get() == 2,
            "Back after alignment review closes must dismiss details only",
        );
        back(&app, false, failures).await;
        check(
            failures,
            backgrounds.get() == 3,
            "Home root Back must work after alignment cancellation",
        );

        // Library and Home are destroyed/recreated when navigation flags
        // change. Verify automatic page focus without a rescuing screen tap.
        app.set_show_home(false);
        app.set_show_library(true);
        settle().await;
        back(&app, true, failures).await;
        check(
            failures,
            home_picks.get() == 1 && app.get_show_home() && !app.get_show_library(),
            "Back in newly created Library must return to Home without pointer interaction",
        );
        check(
            failures,
            backgrounds.get() == 3,
            "a held Back returning from Library must not background Home",
        );
        back(&app, false, failures).await;
        check(
            failures,
            backgrounds.get() == 4,
            "Back must reach recreated Home without a touch restoring focus",
        );

        // A suggestions response replaces the manual-search input. Its
        // removal must preserve the sheet's own Back capture path too.
        app.set_modal_visible(true);
        settle().await;
        app.set_tracking_open(true);
        settle().await;
        if !click(&app, "Choose another release", failures).await
            || !click(&app, "Tracker search", failures).await
        {
            slint::quit_event_loop().unwrap();
            return;
        }
        app.set_tracking_setup_revision("proposal-2".into());
        settle().await;
        check(
            failures,
            ElementHandle::find_by_accessible_label(&app, "Tracker search")
                .next()
                .is_none(),
            "new suggestions must dismiss the manual-search input",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_tracking_open()
                && app.get_modal_visible()
                && tracking_closes.get() == 4
                && detail_closes.get() == 3
                && backgrounds.get() == 4,
            "Back after suggestions remove the focused search input must dismiss tracking only",
        );
        back(&app, true, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 4 && backgrounds.get() == 4,
            "Back after the refreshed sheet closes must dismiss details only",
        );
        back(&app, false, failures).await;
        check(
            failures,
            backgrounds.get() == 5,
            "only the following Home root Back may background after suggestions reload",
        );
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "tracking Back failures:\n  {}",
        failures.borrow().join("\n  ")
    );
}

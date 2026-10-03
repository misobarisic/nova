//! Back must survive episode search disappearing on tab/stream navigation.

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

async fn click_id(app: &nova::AppWindow, id: &str, failures: &RefCell<Vec<String>>) -> bool {
    let Some(control) = ElementHandle::find_by_element_id(app, id).next() else {
        failures
            .borrow_mut()
            .push(format!("missing detail control: {id}"));
        return false;
    };
    control
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
    true
}

async fn focus_episode_search(app: &nova::AppWindow, failures: &RefCell<Vec<String>>) -> bool {
    if !click_id(app, "DetailPage::episode_search_toggle", failures).await {
        return false;
    }
    // The magnifier's deferred focus lands on SearchField's actual TextInput.
    // Verify input delivery and its bound value, rather than a label click.
    let before = app.get_episode_filter();
    let typed = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed { text: "x".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
    settle().await;
    let after = app.get_episode_filter();
    let focused = matches!(
        &typed,
        Ok(slint::platform::WindowEventDispatchResult::Accepted)
    ) && before != after
        && after.contains('x');
    if !focused {
        failures.borrow_mut().push(format!(
            "episode search must accept text before teardown: result={typed:?}, before={before:?}, after={after:?}"
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
            "Back press must be accepted after episode search disappears",
        ),
        (
            slint::platform::WindowEvent::KeyPressRepeated {
                text: slint::platform::Key::Back.into(),
            },
            "Back repeat must be accepted without popping a second detail layer",
        ),
        (
            slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            },
            "Back release must be accepted so Android cannot finish the activity",
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
        "one held Back must produce exactly one detail navigation request",
    );
}

#[test]
fn back_survives_focused_episode_search_tab_and_stream_teardown() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.window().show().unwrap();
    app.set_selected_title("Series".into());
    app.set_detail_tab(3);
    app.set_modal_episodes(true);
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 1".into()])).into());
    app.set_episode_rows(
        Rc::new(VecModel::from(vec![nova::EpisodeRow {
            text: "Pilot".into(),
            ep_no: "S1 E1".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_episode_total(1);
    app.set_episode_page_count(1);

    let tab_picks = Rc::new(Cell::new(0usize));
    let recorded_tab_picks = tab_picks.clone();
    let weak = app.as_weak();
    app.on_detail_tab_picked(move |tab| {
        recorded_tab_picks.set(recorded_tab_picks.get() + 1);
        weak.upgrade().unwrap().set_detail_tab(tab);
    });
    let episode_picks = Rc::new(Cell::new(0usize));
    let recorded_episode_picks = episode_picks.clone();
    let weak = app.as_weak();
    app.on_episode_picked(move |_| {
        recorded_episode_picks.set(recorded_episode_picks.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_episode_context("S1 E1".into());
        app.set_detail_deep_stream(false);
        app.set_modal_episodes(false);
    });
    let episodes_backs = Rc::new(Cell::new(0usize));
    let recorded_episodes_backs = episodes_backs.clone();
    let weak = app.as_weak();
    app.on_episodes_back(move || {
        recorded_episodes_backs.set(recorded_episodes_backs.get() + 1);
        weak.upgrade().unwrap().set_modal_episodes(true);
    });
    let detail_closes = Rc::new(Cell::new(0usize));
    let recorded_detail_closes = detail_closes.clone();
    app.on_modal_closed(move || recorded_detail_closes.set(recorded_detail_closes.get() + 1));
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
        app.set_modal_visible(true);
        settle().await;
        if !focus_episode_search(&app, failures).await {
            slint::quit_event_loop().unwrap();
            return;
        }
        let Some(overview) = ElementHandle::find_by_accessible_label(&app, "Overview")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Text))
        else {
            failures
                .borrow_mut()
                .push("missing Overview tab".to_string());
            slint::quit_event_loop().unwrap();
            return;
        };
        overview
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        check(
            failures,
            app.get_detail_tab() == 0
                && tab_picks.get() == 1
                && ElementHandle::find_by_element_id(&app, "DetailPage::filter_line")
                    .next()
                    .is_none(),
            "tapping Overview must remove the focused episode search subtree",
        );
        back(&app, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 1 && backgrounds.get() == 0,
            "Back after tab switching must close details without leaving the app",
        );
        back(&app, failures).await;
        check(
            failures,
            backgrounds.get() == 1,
            "only the following Home root Back may background after tab switching",
        );

        app.set_detail_tab(3);
        app.set_modal_episodes(true);
        app.set_episode_filter("".into());
        app.set_episode_context("".into());
        app.set_modal_visible(true);
        settle().await;
        if !focus_episode_search(&app, failures).await
            || !click_id(&app, "DetailPage::touch_ep", failures).await
        {
            slint::quit_event_loop().unwrap();
            return;
        }
        check(
            failures,
            episode_picks.get() == 1
                && !app.get_modal_episodes()
                && app.get_modal_visible()
                && ElementHandle::find_by_element_id(&app, "DetailPage::filter_line")
                    .next()
                    .is_none(),
            "tapping an episode must remove the focused search subtree and open its streams",
        );
        back(&app, failures).await;
        check(
            failures,
            app.get_modal_episodes()
                && app.get_modal_visible()
                && episodes_backs.get() == 1
                && detail_closes.get() == 1
                && backgrounds.get() == 1,
            "Back after opening streams must return to episodes before closing details",
        );
        back(&app, failures).await;
        check(
            failures,
            !app.get_modal_visible() && detail_closes.get() == 2 && backgrounds.get() == 1,
            "the next Back must close details without backgrounding Home",
        );
        back(&app, failures).await;
        check(
            failures,
            backgrounds.get() == 2,
            "only a separate Home root Back may background after streams close",
        );
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "detail input Back failures:\n  {}",
        failures.borrow().join("\n  ")
    );
}

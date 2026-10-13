//! First navbar Back after selector entry must navigate rather than restore focus.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn frame(app: &nova::AppWindow, component: &str) {
    assert!(
        ElementHandle::find_by_element_type_name(app, component)
            .next()
            .unwrap()
            .size()
            .height
            > 0.0
    );
    // Run the next UI tick after constructing the new page, without waiting
    // for its entrance animation or tapping a control to recover focus.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
}

#[test]
fn first_navbar_back_returns_selector_to_home_or_episodes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "first_navbar_back_returns_selector_to_home_or_episodes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.window().set_size(slint::PhysicalSize::new(390, 800));
    app.set_selected_title("Series".into());
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
    app.show().unwrap();
    let backgrounds = Rc::new(Cell::new(0));
    let count = backgrounds.clone();
    app.on_exit_to_background(move || count.set(count.get() + 1));
    let returns = Rc::new(Cell::new(0));
    let count = returns.clone();
    let weak = app.as_weak();
    app.on_stream_selector_back(move || {
        count.set(count.get() + 1);
        let app = weak.upgrade().unwrap();
        if app.get_detail_deep_stream() {
            app.set_modal_visible(false);
            app.set_stream_selector_open(false);
            app.set_detail_deep_stream(false);
            app.set_episode_context(Default::default());
        } else {
            app.set_modal_episodes(true);
            app.set_stream_selector_open(false);
            app.set_episode_context(Default::default());
            app.set_detail_tab(3);
        }
    });
    let weak = app.as_weak();
    app.on_continue_picked(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_modal_visible(true);
        app.set_modal_episodes(false);
        app.set_episode_context("S1 E1 · Pilot".into());
        app.set_stream_selector_open(true);
        app.set_detail_deep_stream(true);
    });
    let weak = app.as_weak();
    app.on_episode_picked(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_modal_episodes(false);
        app.set_episode_context("S1 E1 · Pilot".into());
        app.set_stream_selector_open(true);
        app.set_detail_deep_stream(false);
    });
    for (deep, animated) in [(true, false), (false, false), (true, true), (false, true)] {
        app.set_animations(animated);
        app.global::<nova::Anim>().set_enabled(animated);
        app.set_modal_visible(false);
        app.set_stream_selector_open(false);
        app.set_show_home(true);
        frame(&app, "HomePage");
        if deep {
            app.invoke_continue_picked(0);
        } else {
            app.set_modal_visible(true);
            app.set_modal_episodes(true);
            app.set_detail_tab(3);
            frame(&app, "DetailPage");
            app.invoke_episode_picked(0);
        }
        frame(&app, "StreamSelector");
        let before = returns.get();
        for (event_index, event) in [
            WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            },
            WindowEvent::KeyPressRepeated {
                text: slint::platform::Key::Back.into(),
            },
            WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                matches!(
                    app.window().dispatch_event_with_result(event).unwrap(),
                    slint::platform::WindowEventDispatchResult::Accepted
                ),
                "navbar Back event {event_index} must remain accepted during entry/teardown (deep={deep}, animated={animated})"
            );
        }
        assert_eq!(
            returns.get(),
            before + 1,
            "first Back must return exactly one layer"
        );
        assert!(!app.get_stream_selector_open());
        assert_eq!(app.get_modal_visible(), !deep);
        if !deep {
            assert!(app.get_modal_episodes());
            assert_eq!(app.get_detail_tab(), 3);
        }
        assert_eq!(
            backgrounds.get(),
            0,
            "selector Back must never background Nova"
        );
        frame(&app, if deep { "HomePage" } else { "DetailPage" });
    }
}

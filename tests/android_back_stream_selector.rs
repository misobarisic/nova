//! Android navbar Back from a library title's episode selector returns one layer.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn frame(app: &nova::AppWindow, component: &str) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    // The headless backend never paints, so explicitly lay out the visible screen.
    assert!(
        ElementHandle::find_by_element_type_name(app, component)
            .next()
            .expect(component)
            .size()
            .height
            > 0.0
    );
}

fn back(app: &nova::AppWindow) {
    for event in [
        WindowEvent::KeyPressed {
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
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
}

#[test]
fn navbar_back_returns_from_library_episode_streams_to_details_then_library() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_touch_menus(true);
    app.set_is_android(true);
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_detail_is_movie(false);
    app.set_in_library(true);
    app.set_selected_title("Library series".into());
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
    app.window().set_size(slint::PhysicalSize::new(390, 800));
    app.show().unwrap();

    let returns = Rc::new(Cell::new(0));
    let recorded = returns.clone();
    let weak = app.as_weak();
    app.on_stream_selector_back(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        // Matches detail.rs::stream_selector_back / episodes_back for a manual pick.
        app.set_stream_selector_open(false);
        app.set_stream_action_open(false);
        app.set_modal_episodes(true);
        app.set_episode_context(Default::default());
        app.set_detail_tab(3);
    });
    let closed = Rc::new(Cell::new(0));
    let recorded = closed.clone();
    let weak = app.as_weak();
    app.on_modal_closed(move || {
        recorded.set(recorded.get() + 1);
        weak.upgrade().unwrap().set_modal_visible(false);
    });
    let backgrounds = Rc::new(Cell::new(0));
    let recorded = backgrounds.clone();
    app.on_exit_to_background(move || recorded.set(recorded.get() + 1));
    frame(&app, "LibraryPage");
    let weak = app.as_weak();
    app.on_library_item_picked(move |_| {
        // open_library_item keeps My Library underneath Detail.
        weak.upgrade().unwrap().set_modal_visible(true);
    });
    app.invoke_library_item_picked(0);
    frame(&app, "DetailPage");

    // Select an episode through the same callback as a tapped episode card.
    let weak = app.as_weak();
    app.on_episode_picked(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_detail_deep_stream(false);
        app.set_episode_context("S1 E1 · Pilot".into());
        app.set_modal_episodes(false);
        app.set_stream_selector_open(true);
    });
    app.invoke_episode_picked(0);
    frame(&app, "StreamSelector");
    // The action sheet is the nearest layer, even with no focused text input.
    app.set_stream_action_open(true);
    frame(&app, "StreamSelector");
    back(&app);
    assert!(!app.get_stream_action_open());
    assert!(app.get_stream_selector_open());
    assert_eq!(returns.get(), 0);
    back(&app);
    assert_eq!(returns.get(), 1);
    assert!(!app.get_stream_selector_open());
    assert!(app.get_modal_visible());
    assert!(app.get_modal_episodes());
    assert_eq!(app.get_detail_tab(), 3);
    assert!(app.get_show_library());
    assert!(!app.get_show_home());
    assert_eq!(closed.get(), 0);
    assert_eq!(backgrounds.get(), 0);
    frame(&app, "DetailPage");
    back(&app);
    assert_eq!(closed.get(), 1);
    assert!(!app.get_modal_visible());
    assert!(app.get_show_library());
    assert!(!app.get_show_home());
    assert_eq!(
        backgrounds.get(),
        0,
        "returning to My Library must not background Nova"
    );
}

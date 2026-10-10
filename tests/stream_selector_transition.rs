//! Stream selection enters from Home and Detail without delaying navbar Back.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, VecModel, platform::WindowEvent};
use std::{rc::Rc, time::Duration};

#[derive(Clone, Copy, Debug)]
enum Origin {
    Home,
    Episode,
    Movie,
}

fn selector(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_type_name(app, "StreamSelector")
        .next()
        .expect("stream selector")
}

fn tick(app: &nova::AppWindow, ms: u64) {
    for _ in 0..ms.div_ceil(10) {
        // The testing backend does not draw. Evaluate the same geometry and
        // opacity bindings a renderer reads before advancing its frame clock.
        let page = selector(app);
        let _ = (
            page.size(),
            page.absolute_position(),
            page.computed_opacity(),
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

fn open(app: &nova::AppWindow, origin: Origin) {
    match origin {
        Origin::Home => app.invoke_continue_picked(0),
        Origin::Episode => app.invoke_episode_picked(0),
        Origin::Movie => app.invoke_watch_now(),
    }
}

fn back(app: &nova::AppWindow, origin: Origin) {
    for event in [
        WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        let press = matches!(event, WindowEvent::KeyPressed { .. });
        let result = app.window().dispatch_event_with_result(event).unwrap();
        // Android's native finish fallback depends on the press being accepted.
        // A Home return destroys this focus owner before the next frame/Up.
        if press {
            assert!(
                matches!(result, slint::platform::WindowEventDispatchResult::Accepted),
                "{origin:?}: navbar Back press returned {result:?}"
            );
        }
    }
    assert!(!app.get_stream_selector_open());
    assert_eq!(app.get_modal_visible(), !matches!(origin, Origin::Home));
    assert!(app.get_show_home());
    // Materialize the returned screen, including rapid close/reopen paths.
    let component = if matches!(origin, Origin::Home) {
        "HomePage"
    } else {
        "DetailPage"
    };
    assert!(
        ElementHandle::find_by_element_type_name(app, component)
            .next()
            .unwrap()
            .size()
            .height
            > 0.0
    );
    assert!(
        ElementHandle::find_by_element_type_name(app, "StreamSelector")
            .next()
            .is_none()
    );
}

fn assert_settled(app: &nova::AppWindow) {
    let page = selector(app);
    assert!(page.absolute_position().x.abs() < 0.1);
    assert!(page.computed_opacity() > 0.99);
}

#[test]
fn stream_selection_motion_preserves_rows_and_navbar_back() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    entrance_slides_and_fades_for_each_origin_and_respects_motion_settings();
    new_streams_stagger_upward_while_progress_and_retained_rows_stay_visible();
}

fn entrance_slides_and_fades_for_each_origin_and_respects_motion_settings() {
    for origin in [Origin::Home, Origin::Episode, Origin::Movie] {
        for width in [390, 1280] {
            let app = nova::AppWindow::new().unwrap();
            app.set_is_android(true);
            app.set_touch_menus(true);
            app.set_show_home(true);
            app.set_modal_visible(!matches!(origin, Origin::Home));
            app.set_detail_is_movie(matches!(origin, Origin::Movie));
            app.set_selected_title("Transition fixture".into());
            app.set_stream_total(1);
            app.set_stream_page_count(1);
            app.set_stream_filter(1);
            app.global::<nova::Anim>().set_enabled(true);
            app.global::<nova::Anim>().set_transitions(true);
            app.window().set_size(slint::PhysicalSize::new(width, 800));
            app.show().unwrap();

            // Mirror the existing run.rs/backend callbacks: Home mounts
            // Detail with streams already open; manual picks reuse Detail.
            let enter = move |weak: slint::Weak<nova::AppWindow>| {
                let app = weak.upgrade().unwrap();
                app.set_detail_deep_stream(matches!(origin, Origin::Home));
                app.set_modal_visible(true);
                app.set_stream_selector_open(true);
            };
            let weak = app.as_weak();
            app.on_continue_picked(move |_| enter(weak.clone()));
            let weak = app.as_weak();
            app.on_episode_picked(move |_| enter(weak.clone()));
            let weak = app.as_weak();
            app.on_watch_now(move || enter(weak.clone()));
            let weak = app.as_weak();
            app.on_stream_selector_back(move || {
                let app = weak.upgrade().unwrap();
                app.set_stream_selector_open(false);
                app.set_modal_visible(!matches!(origin, Origin::Home));
                app.set_modal_episodes(matches!(origin, Origin::Episode));
                app.set_detail_tab(if matches!(origin, Origin::Episode) {
                    3
                } else {
                    0
                });
            });
            if let Some(page) = ElementHandle::find_by_element_type_name(&app, "DetailPage").next()
            {
                let _ = page.size();
            }

            open(&app, origin);
            let start = selector(&app);
            let start_x = start.absolute_position().x;
            assert!(start_x > 0.0, "{origin:?}: entrance starts to the right");
            assert!(start.computed_opacity() < 0.01);
            let mut slid = false;
            let mut faded = false;
            for frame in 0..35 {
                tick(&app, 10);
                let page = selector(&app);
                let x = page.absolute_position().x;
                let opacity = page.computed_opacity();
                slid |= x > 0.1 && x < start_x - 0.1;
                faded |= opacity > 0.01 && opacity < 0.99;
                if frame == 8 {
                    // Addons deliver rows during entry; replacing the model
                    // must not restart the animation or strand a hidden page.
                    app.set_streams(
                        Rc::new(VecModel::from(vec![nova::StreamRow {
                            id: "result".into(),
                            text: "Source · 1080p".into(),
                            ..Default::default()
                        }]))
                        .into(),
                    );
                }
            }
            assert!(
                slid && faded,
                "{origin:?}: intermediate slide and fade frames"
            );
            assert_settled(&app);
            app.window().set_size(slint::PhysicalSize::new(900, 480));
            assert_settled(&app);
            assert_eq!(app.get_stream_filter(), 1);

            back(&app, origin);
            open(&app, origin);
            assert!(
                selector(&app).computed_opacity() < 0.01,
                "reopening replays entry"
            );
            tick(&app, 50);
            assert!(selector(&app).computed_opacity() < 0.99);
            back(&app, origin); // Navbar Back also works during the entrance.

            for master_off in [true, false] {
                app.global::<nova::Anim>().set_enabled(!master_off);
                app.global::<nova::Anim>().set_transitions(master_off);
                open(&app, origin);
                assert_settled(&app); // No deferred blank frame with motion off.
                back(&app, origin);
            }
            app.hide().unwrap();
        }
    }
}

fn row(index: usize) -> nova::StreamRow {
    nova::StreamRow {
        id: format!("row-{index}").into(),
        text: format!("Source {index} · 1080p").into(),
        is_download: index == 0,
        download_action: 1,
        ..Default::default()
    }
}

fn row_element(app: &nova::AppWindow, index: usize) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, &format!("Source {index} · 1080p"))
        .next()
        .expect("stream row")
}

fn row_frames(app: &nova::AppWindow, model: &VecModel<nova::StreamRow>, ms: u64) {
    for _ in 0..ms.div_ceil(10) {
        for data in model.iter() {
            let element = ElementHandle::find_by_accessible_label(app, data.text.as_str())
                .next()
                .unwrap();
            let _ = (
                element.size(),
                element.absolute_position(),
                element.computed_opacity(),
            );
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

fn new_streams_stagger_upward_while_progress_and_retained_rows_stay_visible() {
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    app.set_touch_menus(true);
    app.set_modal_visible(true);
    app.set_stream_selector_open(true);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    tick(&app, 400);
    assert_settled(&app);
    let model = Rc::new(VecModel::from((0..4).map(row).collect::<Vec<_>>()));
    app.set_streams(model.clone().into());
    app.set_stream_total(4);
    assert!(row_element(&app, 0).computed_opacity() < 0.01);
    let mut faded = false;
    let mut moved = false;
    let mut staggered = false;
    for frame in 0..60 {
        row_frames(&app, &model, 10);
        let first = row_element(&app, 0);
        let last = row_element(&app, 3);
        let opacity = first.computed_opacity();
        faded |= opacity > 0.01 && opacity < 0.99;
        staggered |= opacity > last.computed_opacity() + 0.05;
        let slot = ElementHandle::find_by_element_id(&app, "StreamList::stream_row")
            .next()
            .unwrap();
        let offset = first.absolute_position().y - slot.absolute_position().y;
        moved |= offset > 0.1 && offset < 11.9;
        if frame % 4 == 0 {
            // The backend sends the same in-place progress notifications.
            let mut download = model.row_data(0).unwrap();
            download.download_progress = frame as f32 / 60.0;
            model.set_row_data(0, download);
        }
    }
    assert!(
        faded && moved && staggered,
        "new rows fade, rise and stagger"
    );
    for index in 0..4 {
        assert!(row_element(&app, index).computed_opacity() > 0.99);
    }
    // A later addon can sort before prior results. Inserting it must preserve
    // those delegates, including a pinned download, rather than re-fading them.
    model.insert(1, row(4));
    assert!(row_element(&app, 4).computed_opacity() < 0.01);
    assert!(row_element(&app, 0).computed_opacity() > 0.99);
    assert!(row_element(&app, 1).computed_opacity() > 0.99);
    row_frames(&app, &model, 600);
    assert!(row_element(&app, 4).computed_opacity() > 0.99);
    model.remove(1);
    assert!(row_element(&app, 1).computed_opacity() > 0.99);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    assert!(row_element(&app, 0).computed_opacity() > 0.99);
    for master_off in [true, false] {
        app.global::<nova::Anim>().set_enabled(!master_off);
        app.global::<nova::Anim>().set_transitions(master_off);
        let index = if master_off { 5 } else { 6 };
        model.push(row(index));
        assert!(
            row_element(&app, index).computed_opacity() > 0.99,
            "motion-off arrival is immediate"
        );
    }
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    for data in model.iter() {
        assert!(
            ElementHandle::find_by_accessible_label(&app, data.text.as_str())
                .next()
                .unwrap()
                .computed_opacity()
                > 0.99,
            "enabling motion must not hide rows already shown"
        );
    }
    app.hide().unwrap();
}

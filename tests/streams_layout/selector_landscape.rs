//! Touch landscape scrolls context and sources together, retaining sticky addons.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn idle() {
    for _ in 0..2 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn scroll(app: &nova::AppWindow, delta: f32) {
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(600.0, 300.0),
        delta_x: 0.0,
        delta_y: delta,
    });
    idle();
}
fn drag(app: &nova::AppWindow) {
    let from = LogicalPosition::new(600.0, 390.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: from,
        button: slint::platform::PointerEventButton::Left,
    });
    for i in 1..=10 {
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(from.x, from.y - i as f32 * 25.0),
        });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(from.x, from.y - 250.0),
        button: slint::platform::PointerEventButton::Left,
    });
    idle();
}

fn assert_sticky_cover(app: &nova::AppWindow) {
    let cover = element(app, "StreamSelector::sticky_backdrop");
    let bar = element(app, "StreamSelector::sticky_filters");
    let fade = element(app, "StreamSelector::sticky_header_fade");
    let pos = cover.absolute_position();
    assert!(pos.x.abs() < 1.0 && pos.y.abs() < 1.0);
    assert!((cover.size().width - app.window().size().width as f32).abs() < 1.0);
    assert!(
        (cover.size().height - bar.absolute_position().y - bar.size().height).abs() < 1.0,
        "the opaque cover includes the safe area above the pills"
    );
    assert!((fade.absolute_position().y - cover.size().height).abs() < 1.0);
    assert_eq!(fade.size().height, 8.0);
    assert_eq!(fade.size().width, cover.size().width);
}

#[test]
fn touch_landscape_scrolls_context_pins_addons_and_keeps_bottom_paging() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "touch_landscape_scrolls_context_pins_addons_and_keeps_bottom_paging"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_modal_visible(true);
    app.set_detail_is_movie(false);
    app.set_selected_title("Series".into());
    app.set_episode_context("Season 1 · Episode 2".into());
    app.set_stream_episode_number("S1 E2".into());
    app.set_stream_episode_title("Episode title".into());
    app.set_streams_hint("Status and source information".into());
    app.set_stream_addons(Rc::new(VecModel::from(vec![SharedString::from("Addon")])).into());
    app.set_stream_filter(1);
    app.set_stream_page_count(2);
    app.set_stream_total(30);
    app.set_streams(
        Rc::new(VecModel::from(
            (0..25)
                .map(|i| nova::StreamRow {
                    id: format!("source-{i}").into(),
                    text: format!("Addon · 1080p\nSource {i}").into(),
                    lines: 2,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_stream_selector_open(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 576));
    app.window().show().unwrap();
    idle();

    // The identical short desktop window keeps its context and top pager.
    let title_y = element(&app, "StreamSelector::compact_context")
        .absolute_position()
        .y;
    assert!(
        ElementHandle::find_by_element_id(&app, "StreamSelector::top_pager")
            .next()
            .is_some()
    );
    scroll(&app, -160.0);
    assert_eq!(
        element(&app, "StreamSelector::compact_context")
            .absolute_position()
            .y,
        title_y
    );

    app.set_is_android(true);
    app.set_touch_menus(true);
    idle();
    assert!(
        ElementHandle::find_by_element_id(&app, "StreamSelector::top_pager")
            .next()
            .is_none()
    );
    // The bottom pager is initially below the fold; the testing queries omit
    // clipped elements. Retain context/list handles before scrolling them out.
    let title = element(&app, "StreamSelector::compact_context");
    let list = element(&app, "StreamSelector::stream_list");
    let title_y = title.absolute_position().y;
    let list_y = list.absolute_position().y;
    let status = ElementHandle::find_by_accessible_label(&app, "Status and source information")
        .next()
        .unwrap();
    let status_y = status.absolute_position().y;
    drag(&app);
    assert!(
        status.absolute_position().y < status_y - 100.0,
        "status scrolls with the title and cards"
    );
    assert!(title.absolute_position().y < title_y - 100.0);
    assert!(list.absolute_position().y < list_y - 100.0);
    let filters = element(&app, "StreamSelector::sticky_filters");
    assert!(
        filters.absolute_position().y.abs() < 1.0,
        "addons stick to the safe top"
    );
    assert_sticky_cover(&app);
    scroll(&app, -300.0);
    assert!(
        element(&app, "StreamSelector::sticky_filters")
            .absolute_position()
            .y
            .abs()
            < 1.0
    );
    scroll(&app, -20000.0);
    let pager = element(&app, "StreamSelector::bottom_pager");
    assert!(pager.absolute_position().y >= 0.0);
    assert!(pager.absolute_position().y + pager.size().height <= 576.0);
    let picked = Rc::new(Cell::new(-1));
    let recorded = picked.clone();
    app.on_stream_page_picked(move |page| recorded.set(page));
    let next = ElementHandle::find_by_accessible_label(&app, "Next page")
        .next()
        .unwrap();
    let p = next.absolute_position();
    let size = next.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
    idle();
    assert_eq!(picked.get(), 1);

    // Rotation retains models and filter choice while returning to fixed portrait chrome.
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    idle();
    assert_eq!(app.get_stream_filter(), 1);
    assert_eq!(app.get_stream_page_count(), 2);
    let title_y = element(&app, "StreamSelector::compact_context")
        .absolute_position()
        .y;
    let viewport = element(&app, "StreamSelector::result_scroll");
    let p = viewport.absolute_position();
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(200.0, p.y + 50.0),
        delta_x: 0.0,
        delta_y: -160.0,
    });
    idle();
    assert_eq!(
        element(&app, "StreamSelector::compact_context")
            .absolute_position()
            .y,
        title_y
    );
    app.window().set_size(slint::PhysicalSize::new(1280, 576));
    idle();
    assert!(
        element(&app, "StreamSelector::sticky_filters")
            .absolute_position()
            .y
            > 60.0
    );
    assert_eq!(app.get_stream_filter(), 1);
    assert!(
        ElementHandle::find_by_element_id(&app, "StreamSelector::sticky_backdrop")
            .next()
            .is_none(),
        "the context header remains visible before the filters pin"
    );
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(16.0, 12.0, 8.0, 10.0),
    );
    idle();
    scroll(&app, -350.0);
    assert!(
        (element(&app, "StreamSelector::sticky_filters")
            .absolute_position()
            .y
            - 16.0)
            .abs()
            < 1.0
    );
    assert_sticky_cover(&app);
    let stream_pick = Rc::new(Cell::new(-1));
    let recorded = stream_pick.clone();
    app.on_stream_picked(move |index| recorded.set(index));
    let position = LogicalPosition::new(600.0, 8.0);
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
    idle();
    assert_eq!(
        stream_pick.get(),
        -1,
        "covered cards cannot play through the safe-area backdrop"
    );
}

#[test]
fn tv_selector_retains_fixed_context_on_short_touch_landscape() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "tv_selector_retains_fixed_context_on_short_touch_landscape"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_touch_menus(true);
    app.set_modal_visible(true);
    app.set_selected_title("TV movie".into());
    app.set_detail_is_movie(true);
    app.set_stream_total(8);
    app.set_streams(
        Rc::new(VecModel::from(
            (0..8)
                .map(|i| nova::StreamRow {
                    id: format!("tv-source-{i}").into(),
                    text: format!("TV source {i}").into(),
                    lines: 1,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_stream_selector_open(true);
    // A short logical display is common on televisions even at high physical
    // resolution. Android touch capability must not select the phone scroller.
    app.window().set_size(slint::PhysicalSize::new(960, 540));
    app.window().show().unwrap();
    idle();
    let before = element(&app, "StreamSelector::compact_context")
        .absolute_position()
        .y;
    scroll(&app, -500.0);
    assert!(
        (element(&app, "StreamSelector::compact_context")
            .absolute_position()
            .y
            - before)
            .abs()
            < 1.0
    );
    assert!(element(&app, "StreamSelector::result_scroll").size().height > 100.0);
}

//! Stream filters use a fixed horizontal viewport above independently scrolling
//! results. Quick taps on faded arrows must leave the next filter/button
//! press intact, and stream drags must leave the filter header in place.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn tap(app: &nova::AppWindow, item: &ElementHandle) {
    let p = item.absolute_position();
    let size = item.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    idle(200);
}

fn addons(count: usize) -> slint::ModelRc<SharedString> {
    Rc::new(VecModel::from(
        (0..count)
            .map(|i| format!("Addon Number {i}").into())
            .collect::<Vec<_>>(),
    ))
    .into()
}

fn chevron(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label)
}

fn visible_pills(app: &nova::AppWindow) -> Vec<ElementHandle> {
    let viewport = element(app, "StreamFilterBar::pill-flick");
    let left = viewport.absolute_position().x;
    let right = left + viewport.size().width;
    ElementHandle::find_by_element_type_name(app, "StreamFilterPill")
        .filter(|pill| {
            let x = pill.absolute_position().x + pill.size().width / 2.0;
            x >= left && x < right
        })
        .collect()
}

#[test]
fn pill_chevrons_step_and_the_touch_bar_stays_above_the_streams() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(900, 900));
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_selected_title("Movie".into());
    app.set_stream_addons(addons(10));
    app.set_streams_hint("hint".into());
    app.set_stream_total(12);
    app.set_streams(
        Rc::new(VecModel::from(
            (0..12)
                .map(|i| nova::StreamRow {
                    id: format!("stream-{i}").into(),
                    text: "Torrentio\nMovie 2160p".into(),
                    lines: 2,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.window().show().unwrap();
    idle(400);
    let filters = Rc::new(RefCell::new(Vec::new()));
    let filters2 = filters.clone();
    app.on_stream_filter_picked(move |i| filters2.borrow_mut().push(i));

    tap(&app, &visible_pills(&app)[0]);
    assert_eq!(filters.borrow().as_slice(), &[0], "initial All tap");
    filters.borrow_mut().clear();

    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "PageButton").count(),
        2
    );
    let tracked = ElementHandle::find_by_element_type_name(&app, "StreamFilterPill")
        .last()
        .unwrap();
    let initial = tracked.absolute_position();
    tap(&app, &chevron(&app, "Scroll filters left"));
    assert_eq!(
        tracked.absolute_position(),
        initial,
        "faded left arrow is inert"
    );
    let width = element(&app, "StreamFilterBar::pill-flick").size().width;
    let content_width = element(&app, "StreamFilterBar::pill-row").size().width;
    let step = width.min(content_width - width);
    tap(&app, &chevron(&app, "Scroll filters right"));
    let panned = tracked.absolute_position();
    assert!(
        (panned.x - initial.x + step).abs() < 1.0,
        "step by one viewport"
    );
    assert_eq!(
        panned.y, initial.y,
        "horizontal steps cannot drift vertically"
    );
    tap(&app, &chevron(&app, "Scroll filters left"));
    assert_eq!(tracked.absolute_position(), initial);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::DownArrow.into(),
        });
    idle(100);
    assert_eq!(
        app.get_detail_kb_s(),
        1,
        "faded-control taps retain keyboard focus"
    );
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::UpArrow.into(),
        });
    idle(100);

    let pills = visible_pills(&app);
    assert!(pills.len() >= 3);
    for pill in &pills {
        tap(&app, pill);
    }
    assert_eq!(
        filters.borrow().as_slice(),
        &(0..pills.len() as i32).collect::<Vec<_>>()
    );
    assert_eq!(
        tracked.absolute_position(),
        initial,
        "filtering cannot pan the bar"
    );

    // Fitting bars keep inert slots, so toggling overflow never reflows them.
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.set_stream_addons(addons(2));
    idle(400);
    let first = visible_pills(&app).remove(0);
    let initial = first.absolute_position();
    tap(&app, &chevron(&app, "Scroll filters right"));
    assert_eq!(first.absolute_position(), initial);

    app.set_touch_menus(true);
    app.set_stream_addons(addons(10));
    idle(300);
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "PageButton").count(),
        0
    );
    assert_eq!(
        ElementHandle::find_by_element_id(&app, "StreamFilterBar::pill-row").count(),
        1
    );
    filters.borrow_mut().clear();
    let pills = visible_pills(&app);
    for pill in &pills {
        tap(&app, pill);
    }
    assert_eq!(
        filters.borrow().as_slice(),
        &(0..pills.len() as i32).collect::<Vec<_>>()
    );
    filters.borrow_mut().clear();

    let header_y = pills[0].absolute_position().y;
    let row = element(&app, "StreamList::stream_row");
    let row_y = row.absolute_position().y;
    let start = LogicalPosition::new(550.0, 700.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: start,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(30);
    for step in 1..=10 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                position: LogicalPosition::new(start.x, start.y - step as f32 * 20.0),
            });
        idle(16);
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: LogicalPosition::new(start.x, start.y - 200.0),
            button: slint::platform::PointerEventButton::Left,
        });
    idle(200);
    assert_eq!(pills[0].absolute_position().y, header_y);
    assert!(
        row.absolute_position().y < row_y - 100.0,
        "results must scroll"
    );
    assert!(
        filters.borrow().is_empty(),
        "result drags cannot pick filters"
    );

    // The phone design hides chevrons even in a resized desktop window.
    app.set_touch_menus(false);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    idle(300);
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "PageButton").count(),
        0
    );
    tap(&app, &visible_pills(&app)[0]);
    assert_eq!(filters.borrow().as_slice(), &[0]);
}

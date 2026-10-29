use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{
    ComponentHandle, LogicalPosition, VecModel,
    platform::{PointerEventButton, WindowEvent},
};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow, frames: usize) {
    for _ in 0..frames {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn setup() -> (nova::AppWindow, Rc<RefCell<Vec<(i32, i32)>>>) {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_show_library(true);
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.set_library(
        Rc::new(VecModel::from(
            (0..600)
                .map(|number| nova::MediaCard {
                    id: format!("title-{number}").into(),
                    title: format!("Title {number}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let windows = Rc::new(RefCell::new(Vec::new()));
    let recorded = windows.clone();
    app.on_library_visible_range(move |first, last| recorded.borrow_mut().push((first, last)));
    app.on_library_item_picked(|_| panic!("scrolling must cancel the card press"));
    app.window().show().unwrap();
    settle(&app, 16);
    (app, windows)
}

#[test]
fn fast_mouse_and_touch_flicks_decelerate_without_opening_cards() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::fast_mouse_and_touch_flicks_decelerate_without_opening_cards"
    )) {
        return;
    }
    use i_slint_core::{input::TouchPhase, platform::InternalEvent};
    fn pointer(app: &nova::AppWindow, position: LogicalPosition, phase: TouchPhase, touch: bool) {
        app.window().dispatch_event(if touch {
            WindowEvent::internal(InternalEvent::Touch {
                id: 7,
                position: i_slint_core::lengths::logical_point_from_api(position),
                phase,
            })
        } else {
            match phase {
                TouchPhase::Started => WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                },
                TouchPhase::Moved => WindowEvent::PointerMoved { position },
                TouchPhase::Ended => WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                },
                TouchPhase::Cancelled => WindowEvent::PointerExited,
            }
        });
    }
    let (app, _) = setup();
    let scroll = ElementHandle::find_by_element_id(&app, "LibraryPage::lib_scroll")
        .next()
        .unwrap();
    let p = scroll.absolute_position();
    let start = LogicalPosition::new(p.x + scroll.size().width / 2.0, 550.0);
    for touch in [false, true] {
        app.set_library_scroll_y(0.0);
        settle(&app, 8);
        pointer(&app, start, TouchPhase::Started, touch);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(32));
        for step in 1..=5 {
            pointer(
                &app,
                LogicalPosition::new(start.x, start.y - step as f32 * 60.0),
                TouchPhase::Moved,
                touch,
            );
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(8));
        }
        let at_release = app.get_library_scroll_y();
        assert!(at_release < -250.0, "dragging follows the finger");
        pointer(
            &app,
            LogicalPosition::new(start.x, start.y - 300.0),
            TouchPhase::Ended,
            touch,
        );
        settle(&app, 60);
        let after = app.get_library_scroll_y();
        assert!(after < at_release - 10.0, "a quick flick still coasts");
        assert!(
            at_release - after < 800.0,
            "a flick must settle within about one screen, not hundreds of entries"
        );
        settle(&app, 10);
        assert!(
            (app.get_library_scroll_y() - after).abs() < 0.5,
            "released momentum must stop"
        );
    }
    app.set_library_scroll_y(-1000.0);
    settle(&app, 8);
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: start,
        delta_x: 0.0,
        delta_y: -120.0,
    });
    settle(&app, 8);
    assert!(
        (app.get_library_scroll_y() + 1072.0).abs() < 1.0,
        "wheel scrolling is damped without adding fling acceleration"
    );
}

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
fn short_scroll_and_stationary_return_report_posters() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::short_scroll_and_stationary_return_report_posters"
    )) {
        return;
    }
    let (app, windows) = setup();
    assert!(
        !windows.borrow().is_empty(),
        "initial viewport must load without scrolling"
    );
    let before = windows.borrow().len();
    app.set_library_scroll_y(-1600.0);
    settle(&app, 8);
    assert!(
        windows.borrow().len() > before,
        "a scroll of fewer than 48 cards must still report"
    );
    assert!(windows.borrow().last().unwrap().0 > 0);
    app.set_library_scroll_y(0.0);
    settle(&app, 8);
    assert_eq!(
        windows.borrow().last().unwrap().0,
        0,
        "stationary return must restore unloaded posters"
    );
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
    for (touch_profile, touch) in [(false, false), (true, false), (true, true)] {
        app.set_touch_menus(touch_profile);
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
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        }
        let at_release = app.get_library_scroll_y();
        assert!(at_release < -250.0, "dragging follows the finger");
        pointer(
            &app,
            LogicalPosition::new(start.x, start.y - 300.0),
            TouchPhase::Ended,
            touch,
        );
        settle(&app, 180);
        let after = app.get_library_scroll_y();
        assert!(after < at_release - 10.0, "a quick flick still coasts");
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
    settle(&app, 16);
    assert!(
        (app.get_library_scroll_y() + 1120.0).abs() < 1.0,
        "wheel scrolling uses its full delta without adding fling acceleration"
    );
}

#[test]
fn desktop_wheel_uses_native_smoothing_and_full_delta() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::desktop_wheel_uses_native_smoothing_and_full_delta"
    )) {
        return;
    }
    let (app, _) = setup();
    app.set_touch_menus(false);
    let scroll = ElementHandle::find_by_element_id(&app, "LibraryPage::lib_scroll")
        .next()
        .unwrap();
    let position = LogicalPosition::new(
        scroll.absolute_position().x + scroll.size().width / 2.0,
        550.0,
    );
    app.set_library_scroll_y(-1000.0);
    settle(&app, 8);
    for delta in [-120.0, -37.5, 60.0] {
        let before = app.get_library_scroll_y();
        let expected = before + delta;
        // Public PointerScrolled events use the immediate/cancelled phase;
        // desktop backends deliver wheel movement with its native phase.
        app.window().dispatch_event(WindowEvent::internal(
            i_slint_core::platform::InternalEvent::Mouse(
                i_slint_core::input::BackendMouseEvent::Wheel {
                    position: i_slint_core::lengths::logical_point_from_api(position),
                    delta_x: 0.0,
                    delta_y: delta,
                    phase: i_slint_core::input::TouchPhase::Moved,
                },
            ),
        ));
        settle(&app, 3);
        let moved = app.get_library_scroll_y() - before;
        assert!(
            moved * delta > 0.0 && moved.abs() < delta.abs(),
            "native wheel smoothing must pass through an intermediate offset: before={before}, moved={moved}, delta={delta}"
        );
        settle(&app, 12);
        assert!(
            (app.get_library_scroll_y() - expected).abs() < 0.5,
            "native smoothing must deliver the complete wheel delta"
        );
    }
}

slint::slint! {
    import { LibraryScroll } from "../../crates/ui/library-scroll.slint";
    export component NativeScrollHarness inherits Window {
        width: 760px; height: 500px;
        in-out property <length> library_y <=> library.content-y;
        out property <length> reference_y: reference.content-y;
        public function reset() { library.content-y = -1000px; reference.content-y = -1000px; }
        public function stop_scroll() { library.stop(); }
        library := LibraryScroll {
            x: 0px; y: 0px;
            width: 360px; height: 500px; content-height: 6000px;
            Rectangle { width: 360px; height: 6000px; }
        }
        reference := Flickable {
            x: 400px; y: 0px; width: 360px; height: 500px;
            content-width: 360px; content-height: 6000px;
            Rectangle { width: 360px; height: 6000px; }
        }
    }
}

#[test]
fn library_momentum_matches_native_flickable_and_repositioning_stops_it() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::library_momentum_matches_native_flickable_and_repositioning_stops_it"
    )) {
        return;
    }
    use i_slint_core::{input::TouchPhase, platform::InternalEvent};
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = NativeScrollHarness::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(760, 500));
    app.show().unwrap();
    let tick = || {
        // This small macro harness has no generated element-debug metadata.
        // Reading its viewport properties drives the native physics directly.
        let _ = (app.get_library_y(), app.get_reference_y());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    };
    let pointer = |position: LogicalPosition, phase: TouchPhase, touch: bool| {
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
    };
    for (touch, held) in [(false, false), (true, false), (true, true)] {
        let mut traces = Vec::new();
        for reference in [false, true] {
            app.invoke_reset();
            tick();
            let x = if reference { 580.0 } else { 180.0 };
            pointer(LogicalPosition::new(x, 400.0), TouchPhase::Started, touch);
            for step in 1..=5 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
                pointer(
                    LogicalPosition::new(x, 400.0 - step as f32 * 40.0),
                    TouchPhase::Moved,
                    touch,
                );
            }
            let end = LogicalPosition::new(x, 200.0);
            // Drivers may duplicate the lift-off position. The native engine
            // alone decides its release velocity and whether a hold cancels it.
            pointer(end, TouchPhase::Moved, touch);
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(if held {
                160
            } else {
                10
            }));
            pointer(end, TouchPhase::Ended, touch);
            let mut trace = Vec::new();
            for _ in 0..125 {
                tick();
                trace.push(if reference {
                    app.get_reference_y()
                } else {
                    app.get_library_y()
                });
            }
            traces.push(trace);
        }
        for (library, native) in traces[0].iter().zip(&traces[1]) {
            assert!(
                (library - native).abs() < 0.5,
                "Library must preserve native momentum: touch={touch}, held={held}, library={library}, native={native}"
            );
        }
        if !held {
            assert!(
                traces[0][10] < traces[0][0] - 10.0,
                "the native coast must remain active"
            );
        }
    }
    // A retained offset written while a native coast is active must replace
    // that animation, rather than being overwritten by its next frame.
    for reposition in [false, true] {
        app.invoke_reset();
        pointer(
            LogicalPosition::new(180.0, 400.0),
            TouchPhase::Started,
            true,
        );
        for step in 1..=5 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
            pointer(
                LogicalPosition::new(180.0, 400.0 - step as f32 * 40.0),
                TouchPhase::Moved,
                true,
            );
        }
        pointer(LogicalPosition::new(180.0, 200.0), TouchPhase::Ended, true);
        tick();
        let expected = if reposition {
            app.set_library_y(-2200.0);
            -2200.0
        } else {
            app.invoke_stop_scroll();
            app.get_library_y()
        };
        for _ in 0..30 {
            tick();
        }
        assert!(
            (app.get_library_y() - expected).abs() < 0.5,
            "explicit stop/restore must cancel the native coast: reposition={reposition}, expected={expected}, actual={}",
            app.get_library_y()
        );
    }
}

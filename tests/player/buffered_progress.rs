//! Estimate passes through the player bridge and stays within the unplayed bar.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use slint::{LogicalPosition, platform::WindowEvent};
use std::time::Duration;
use std::{cell::RefCell, rc::Rc};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

#[test]
fn pointer_scrubbing_previews_without_seeking_until_release() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::pointer_scrubbing_previews_without_seeking_until_release"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(100.0);
    app.set_buffered_until(60.0);
    app.window().show().unwrap();
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let recorded = seeks.clone();
    app.on_seek(move |value| recorded.borrow_mut().push(value));
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));

    for (width, height, android) in [(900, 360, true), (390, 844, true), (1280, 720, false)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        app.set_osd_visible(true);
        app.set_position(25.0);
        settle(&app);
        let track = element(&app, "PlayerOverlay::seek_touch");
        let origin = track.absolute_position();
        let size = track.size();
        let point = |fraction: f32| {
            LogicalPosition::new(
                origin.x + size.width * fraction,
                origin.y + size.height / 2.0,
            )
        };
        let before = seeks.borrow().len();
        // Hovering is only a tooltip; pressing and dragging are previews too.
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: point(0.4),
        });
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: point(0.4),
            button: slint::platform::PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: point(0.7),
        });
        settle(&app);
        assert_eq!(seeks.borrow().len(), before);
        assert_eq!(app.get_position(), 25.0);
        assert!(
            (element(&app, "PlayerOverlay::played_track").size().width - size.width * 0.7).abs()
                < 1.0
        );
        assert_eq!(
            element(&app, "PlayerOverlay::buffered_track").size().width,
            0.0
        );
        // Simulate a native progress tick while the finger is held still.
        app.set_position(26.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(4));
        settle(&app);
        assert!(
            (element(&app, "PlayerOverlay::played_track").size().width - size.width * 0.7).abs()
                < 1.0
        );
        assert_eq!(seeks.borrow().len(), before);
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: point(0.8),
            button: slint::platform::PointerEventButton::Left,
        });
        settle(&app);
        assert_eq!(seeks.borrow().len(), before + 1);
        assert!((seeks.borrow()[before] - 80.0).abs() < 0.1);
        assert!((app.get_position() - 80.0).abs() < 0.1);

        // A tap also commits exactly once, after release.
        track.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(seeks.borrow().len(), before + 2);
        assert!((seeks.borrow()[before + 1] - 50.0).abs() < 0.1);
    }
}

#[test]
fn pointer_seek_cancellation_and_unavailable_controls_never_commit() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::pointer_seek_cancellation_and_unavailable_controls_never_commit"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(900, 360));
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_position(25.0);
    app.on_seek(|_| panic!("cancelled or unavailable timeline sought"));
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.window().show().unwrap();

    for case in 0..7 {
        app.set_duration(100.0);
        app.set_playback_started(true);
        app.set_party_active(false);
        app.set_party_busy(false);
        app.set_osd_visible(case != 0);
        settle(&app);
        let track = element(&app, "PlayerOverlay::seek_touch");
        let origin = track.absolute_position();
        let size = track.size();
        let point = LogicalPosition::new(origin.x + size.width * 0.7, origin.y + size.height / 2.0);
        if case == 1 {
            app.set_duration(0.0);
        }
        if case == 2 {
            app.set_playback_started(false);
        }
        settle(&app);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        });
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position: point });
        match case {
            3 => app.window().dispatch_event(WindowEvent::PointerExited),
            4 => app.set_playback_started(false), // source replacement
            5 => app.set_party_busy(true),        // lost timeline authority
            6 => app.set_duration(0.0),
            _ => (),
        }
        settle(&app);
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        });
        settle(&app);
        assert_eq!(
            app.get_position(),
            25.0,
            "case {case} overwrote playback position"
        );
    }
}

#[test]
fn buffer_fits_the_unplayed_bar_and_hides_when_unavailable() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "buffer_fits_the_unplayed_bar_and_hides_when_unavailable"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_osd_visible(true);
    app.set_playback_started(true);
    app.window().show().unwrap();
    for true_black in [false, true] {
        nova_ui::apply_theme(&app, true_black);
        for (width, height, android) in [(390, 844, true), (900, 360, true), (1280, 720, false)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            app.set_is_android(android);
            app.set_duration(100.0);
            app.set_position(25.0);
            app.set_buffered_until(60.0);
            app.set_playback_started(true);
            for paused in [false, true] {
                app.set_is_paused(paused);
                settle(&app);
                let track = element(&app, "PlayerOverlay::unbuffered_track");
                let played = element(&app, "PlayerOverlay::played_track");
                let buffer = element(&app, "PlayerOverlay::buffered_track");
                assert!((played.size().width - track.size().width * 0.25).abs() < 0.5);
                assert!((buffer.size().width - track.size().width * 0.35).abs() < 0.5);
                assert!(
                    (buffer.absolute_position().x
                        - played.absolute_position().x
                        - played.size().width)
                        .abs()
                        < 0.5
                );
                assert!(
                    buffer.absolute_position().x + buffer.size().width
                        <= track.absolute_position().x + track.size().width + 0.5
                );
            }
            app.set_buffered_until(150.0); // estimates must not overflow duration
            settle(&app);
            let track = element(&app, "PlayerOverlay::unbuffered_track");
            assert!(
                (element(&app, "PlayerOverlay::buffered_track").size().width
                    - track.size().width * 0.75)
                    .abs()
                    < 0.5
            );
            for (position, end, duration, started) in [
                (70.0, 60.0, 100.0, true),  // scrub beyond the estimate
                (25.0, 0.0, 100.0, true),   // missing cache properties
                (25.0, 60.0, 0.0, true),    // unknown/live duration
                (25.0, 60.0, 100.0, false), // resolving/loading a new file
            ] {
                app.set_position(position);
                app.set_buffered_until(end);
                app.set_duration(duration);
                app.set_playback_started(started);
                settle(&app);
                assert_eq!(
                    element(&app, "PlayerOverlay::buffered_track").size().width,
                    0.0
                );
            }
        }
    }
}

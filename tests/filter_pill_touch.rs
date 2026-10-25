//! Android touch feedback must clear even when the last pointer stays over a pill.
use i_slint_core::{input::TouchPhase, platform::InternalEvent};
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};

slint::slint! {
    import { LibraryFilterChip } from "../crates/ui/library.slint";
    import { StreamFilterPill } from "../crates/ui/stream-selector.slint";
    export { Theme, Anim } from "@NovaUiShared";

    export component PillHarness inherits Window {
        width: 280px;
        height: 180px;
        in property <bool> touch_feedback: true;
        in property <bool> selected: false;
        in property <bool> stream_loading: false;
        in property <bool> refresh_enabled: true;
        out property <angle> refresh_rotation: refresh.refresh_rotation;
        out property <bool> refresh_ready: refresh.activation_ready;
        out property <int> refresh_picks: 0;
        out property <brush> library_fill: library.background;
        out property <length> library_width: library.width;
        out property <brush> stream_fill: stream.background;
        out property <int> picks: 0;
        library := LibraryFilterChip {
            x: 20px; y: 20px;
            label: "Watching";
            touch_feedback: root.touch_feedback;
            selected: root.selected;
            picked => { root.picks += 1; }
        }
        stream := StreamFilterPill {
            x: 20px; y: 100px; width: 180px;
            label: "Torrentio";
            loading: root.stream_loading;
            touch_feedback: root.touch_feedback;
            selected: root.selected;
            picked => { root.picks += 1; }
        }
        refresh := StreamFilterPill {
            x: 220px; y: 100px;
            label: "Refresh all sources"; refresh_icon: true;
            enabled: root.refresh_enabled; touch_feedback: root.touch_feedback;
            picked => { root.refresh_picks += 1; }
        }
    }
}

fn touch(app: &PillHarness, position: LogicalPosition, phase: TouchPhase) {
    // These are the events Android's MotionAction Up/Cancel adapter delivers.
    app.window()
        .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
            id: 1,
            position: i_slint_core::lengths::logical_point_from_api(position),
            phase,
        }));
}

#[test]
fn touch_release_and_cancel_clear_both_pills_while_mouse_hover_still_works() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = PillHarness::new().unwrap();
    app.show().unwrap();
    let original = app.global::<Theme>().get_current();
    // Selection fades without changing natural pill width, and rapid changes
    // retarget from the current fill instead of completing a stale selection.
    let width = app.get_library_width();
    let base = app.get_library_fill();
    let advance = |ms| {
        for _ in 0..ms {
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
            let _ = app.get_library_fill();
        }
    };
    app.set_selected(true);
    advance(50);
    assert_ne!(app.get_library_fill(), base);
    assert_ne!(app.get_library_fill(), original.primary_button);
    assert_eq!(app.get_library_width(), width);
    app.set_selected(false);
    advance(150);
    assert_eq!(app.get_library_fill(), base);
    for (master, hover) in [(true, false), (false, true)] {
        app.global::<Anim>().set_enabled(master);
        app.global::<Anim>().set_hover(hover);
        app.set_selected(true);
        advance(1);
        assert_eq!(app.get_library_fill(), original.primary_button);
        app.set_selected(false);
        advance(1);
        assert_eq!(app.get_library_fill(), base);
    }
    app.global::<Anim>().set_enabled(false);
    for black in [false, true] {
        let mut palette = original.clone();
        if black {
            palette.hero_control = slint::Color::from_rgb_u8(0, 0, 0);
            palette.control_hover = slint::Color::from_rgb_u8(24, 24, 24);
        }
        app.global::<Theme>().set_current(palette.clone());
        for library in [true, false] {
            let position = LogicalPosition::new(80.0, if library { 40.0 } else { 120.0 });
            let fill = || {
                if library {
                    app.get_library_fill()
                } else {
                    app.get_stream_fill()
                }
            };
            app.set_touch_feedback(true);
            app.window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            assert_eq!(
                fill(),
                palette.hero_control.into(),
                "hover alone must not tint an Android pill"
            );
            for _ in 0..3 {
                // Retain the pointer inside the pill after Up, reproducing
                // the sticky has-hover state reported on Android.
                app.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
                assert_eq!(fill(), palette.control_hover.into());
                let picks = app.get_picks();
                app.window().dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
                assert_eq!(app.get_picks(), picks + 1);
                assert_eq!(
                    fill(),
                    palette.hero_control.into(),
                    "release without pointer exit clears feedback"
                );
            }
            touch(&app, position, TouchPhase::Started);
            assert_eq!(fill(), palette.control_hover.into());
            touch(&app, position, TouchPhase::Cancelled);
            assert_eq!(fill(), palette.hero_control.into());
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            let picks = app.get_picks();
            // A scroll grab or pointer exit cancels the pill's pressed state.
            app.window().dispatch_event(WindowEvent::PointerExited);
            assert_eq!(fill(), palette.hero_control.into());
            assert_eq!(app.get_picks(), picks);

            app.set_selected(true);
            touch(&app, position, TouchPhase::Started);
            touch(&app, position, TouchPhase::Ended);
            assert_eq!(
                fill(),
                palette.primary_button,
                "the selected pill keeps its gradient"
            );
            app.set_selected(false);

            app.set_touch_feedback(false);
            app.window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            assert_eq!(
                fill(),
                palette.control_hover.into(),
                "desktop hover is retained"
            );
            app.window().dispatch_event(WindowEvent::PointerExited);
            assert_eq!(fill(), palette.hero_control.into());
        }
    }
    // Refresh shares the pill's hit testing, but blocks repeated pointer events
    // immediately, before a backend can publish its own longer cooldown.
    let tap_refresh = || {
        for event in [
            WindowEvent::PointerPressed {
                position: LogicalPosition::new(242.0, 120.0),
                button: slint::platform::PointerEventButton::Left,
            },
            WindowEvent::PointerReleased {
                position: LogicalPosition::new(242.0, 120.0),
                button: slint::platform::PointerEventButton::Left,
            },
        ] {
            app.window().dispatch_event(event);
        }
    };
    app.global::<Anim>().set_enabled(true);
    app.global::<Anim>().set_transitions(true);
    app.global::<Anim>().set_hover(true);
    let start_rotation = app.get_refresh_rotation();
    tap_refresh();
    assert_eq!(app.get_refresh_picks(), 1);
    assert!(!app.get_refresh_ready());
    for _ in 0..8 {
        tap_refresh();
    }
    assert_eq!(
        app.get_refresh_picks(),
        1,
        "rapid taps cannot repeat refresh"
    );
    advance(100);
    let turning = app.get_refresh_rotation();
    assert!(
        turning > start_rotation && turning < start_rotation + 360.0,
        "refresh turns smoothly after activation: {turning}"
    );
    app.set_refresh_enabled(false);
    advance(500);
    assert!(!app.get_refresh_ready());
    tap_refresh();
    assert_eq!(
        app.get_refresh_picks(),
        1,
        "backend disabled state outlives local feedback"
    );
    app.set_refresh_enabled(true);
    assert!(app.get_refresh_ready());

    app.set_stream_loading(true);
    // Loading-sweep geometry and accessibility are checked on the compiled
    // StreamSelector in ui_streams_layout. This inline fixture needs no
    // element inspection metadata: its checks use public state and input.
    for (master, transitions, hover) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        app.global::<Anim>().set_enabled(master);
        app.global::<Anim>().set_transitions(transitions);
        app.global::<Anim>().set_hover(hover);
        advance(1);
        let before = app.get_refresh_picks();
        tap_refresh();
        assert_eq!(app.get_refresh_picks(), before + 1);
        let instant = app.get_refresh_rotation();
        advance(100);
        assert_eq!(
            app.get_refresh_rotation(),
            instant,
            "reduced motion disables the click turn"
        );
        advance(500);
    }
    app.set_stream_loading(false);
}

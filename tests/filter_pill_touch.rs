//! Android touch feedback must clear even when the last pointer stays over a pill.
use i_slint_core::{input::TouchPhase, platform::InternalEvent};
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};

slint::slint! {
    import { LibraryFilterChip } from "../crates/ui/library.slint";
    import { StreamFilterPill } from "../crates/ui/stream-selector.slint";
    export { Theme } from "../crates/ui/theme.slint";

    export component PillHarness inherits Window {
        width: 280px;
        height: 180px;
        in property <bool> touch_feedback: true;
        in property <bool> selected: false;
        out property <brush> library_fill: library.background;
        out property <brush> stream_fill: stream.background;
        out property <int> picks: 0;
        library := LibraryFilterChip {
            x: 20px; y: 20px; width: 180px;
            label: "Watching";
            touch_feedback: root.touch_feedback;
            selected: root.selected;
            picked => { root.picks += 1; }
        }
        stream := StreamFilterPill {
            x: 20px; y: 100px; width: 180px;
            label: "Torrentio";
            touch_feedback: root.touch_feedback;
            selected: root.selected;
            picked => { root.picks += 1; }
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
}

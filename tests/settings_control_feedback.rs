//! Touch scroll feedback clears without breaking desktop hover or selection.
use i_slint_core::{input::TouchPhase, platform::InternalEvent};
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};

slint::slint! {
    import { SettingsLink } from "../crates/ui/settings-controls.slint";
    export { Theme, Anim, ControlInput } from "@NovaUiShared";
    export component HoverProbe inherits Window {
        width: 320px; height: 320px;
        out property <brush> first_fill: first.background;
        out property <brush> second_fill: second.background;
        out property <brush> third_fill: third.background;
        out property <int> picks: 0;
        Flickable {
            width: 100%; height: 100%; content-height: 900px;
            first := SettingsLink {
                width: 320px; y: 20px; title: "Addons";
                picked => { root.picks += 1; }
            }
            second := SettingsLink {
                width: 320px; y: 105px; title: "Sync";
                picked => { root.picks += 1; }
            }
            third := SettingsLink {
                width: 320px; y: 190px; title: "Downloads";
                picked => { root.picks += 1; }
            }
        }
    }
}

fn touch(app: &HoverProbe, y: f32, phase: TouchPhase) {
    app.window()
        .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
            id: 0,
            position: i_slint_core::lengths::logical_point_from_api(LogicalPosition::new(160.0, y)),
            phase,
        }));
}

#[test]
fn touch_scroll_release_and_cancel_do_not_strand_row_highlights() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = HoverProbe::new().unwrap();
    app.global::<Anim>().set_enabled(false);
    app.global::<ControlInput>().set_touch_feedback(true);
    app.show().unwrap();
    let palette = app.global::<Theme>().get_current();
    let base: slint::Brush = palette.card.into();
    let hover: slint::Brush = palette.settings_control.into();
    assert_ne!(base, hover);
    assert_eq!(app.get_first_fill(), base);
    touch(&app, 50.0, TouchPhase::Started);
    // A small wiggle is forwarded while the scroll area's press is delayed.
    touch(&app, 53.0, TouchPhase::Moved);
    touch(&app, 140.0, TouchPhase::Moved);
    touch(&app, 250.0, TouchPhase::Moved);
    touch(&app, 250.0, TouchPhase::Ended);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(250));
    let fills = [
        app.get_first_fill(),
        app.get_second_fill(),
        app.get_third_fill(),
    ];
    let sticky = fills.iter().filter(|fill| **fill == hover).count();
    assert_eq!(sticky, 0, "touch release must not strand row hover");
    assert_eq!(app.get_picks(), 0, "scroll must not activate a row");

    // Repeat over another row to check whether stale washes accumulate.
    touch(&app, 140.0, TouchPhase::Started);
    touch(&app, 143.0, TouchPhase::Moved);
    touch(&app, 250.0, TouchPhase::Moved);
    touch(&app, 250.0, TouchPhase::Cancelled);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(250));
    let fills = [
        app.get_first_fill(),
        app.get_second_fill(),
        app.get_third_fill(),
    ];
    let sticky = fills.iter().filter(|fill| **fill == hover).count();
    assert_eq!(sticky, 0, "cancel must not accumulate hover across rows");
    assert_eq!(app.get_picks(), 0);

    let mouse = HoverProbe::new().unwrap();
    mouse.global::<Anim>().set_enabled(false);
    mouse.show().unwrap();
    mouse.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(160.0, 50.0),
    });
    assert_eq!(mouse.get_first_fill(), hover, "ordinary mouse hover works");
    mouse.window().dispatch_event(WindowEvent::PointerExited);
    assert_eq!(
        mouse.get_first_fill(),
        base,
        "ordinary mouse exit clears hover"
    );
    // Startup sets the existing platform flag; shared controls must see it.
    let root = nova::AppWindow::new().unwrap();
    root.set_touch_menus(true);
    assert!(root.global::<nova::ControlInput>().get_touch_feedback());
    root.set_touch_menus(false);
    assert!(!root.global::<nova::ControlInput>().get_touch_feedback());
}

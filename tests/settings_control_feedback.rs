//! Touch feedback and TV controls, with each case owning a fresh Slint backend.

#[path = "support/isolation.rs"]
mod isolation;
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
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "touch_scroll_release_and_cancel_do_not_strand_row_highlights"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = HoverProbe::new().unwrap();
    app.global::<Anim>().set_enabled(false);
    app.global::<ControlInput>().set_touch_feedback(true);
    app.show().unwrap();
    let palette = app.global::<Theme>().get_current();
    let base: slint::Brush = palette.card;
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

mod tv_controls {
    use slint::ComponentHandle;

    slint::slint! {
        import { Dropdown } from "../crates/ui/dropdown.slint";
        import { MenuSheet } from "../crates/ui/menusheet.slint";
        export { InterfaceStyle, Anim } from "@NovaUiShared";
        export component TvControlsProbe inherits Window {
            width: 900px; height: 700px;
            out property <int> selection: -1;
            out property <int> action: -1;
            in-out property <bool> sheet_open: false;
            public function focus_dropdown() { dropdown.focus(); }
            dropdown := Dropdown {
                width: 300px;
                model: ["zero", "one", "two", "three", "four", "five", "six", "seven"];
                selected(value) => { root.selection = self.current-index; }
            }
            MenuSheet {
                open: root.sheet_open;
                max_h: 350px;
                items: [{ label: "Enabled", enabled: true }, { label: "Disabled", enabled: false }];
                picked(index) => { root.action = index; root.sheet_open = false; }
                closed => { root.sheet_open = false; }
            }
        }
    }

    fn key(app: &TvControlsProbe, key: slint::platform::Key) {
        let text: slint::SharedString = key.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
    }

    #[test]
    fn tv_dropdown_and_sheet_complete_with_directions_ok_and_back() {
        if !crate::isolation::run(concat!(
            module_path!(),
            "::",
            "tv_dropdown_and_sheet_complete_with_directions_ok_and_back"
        )) {
            return;
        }
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = TvControlsProbe::new().unwrap();
        app.global::<InterfaceStyle>().set_tv(true);
        app.global::<Anim>().set_enabled(false);
        app.show().unwrap();
        app.invoke_focus_dropdown();
        key(&app, slint::platform::Key::Return);
        for _ in 0..7 {
            key(&app, slint::platform::Key::DownArrow);
        }
        key(&app, slint::platform::Key::Return);
        assert_eq!(
            app.get_selection(),
            7,
            "the last popup row is reachable without Tab"
        );
        key(&app, slint::platform::Key::Return);
        key(&app, slint::platform::Key::Back);
        assert_eq!(
            app.get_selection(),
            7,
            "Back closes without changing the selection"
        );
        app.set_sheet_open(true);
        key(&app, slint::platform::Key::UpArrow);
        key(&app, slint::platform::Key::Return);
        assert!(
            app.get_sheet_open(),
            "disabled rows cannot dispatch an action"
        );
        key(&app, slint::platform::Key::UpArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(app.get_action(), 0);
        assert!(!app.get_sheet_open());
        app.set_sheet_open(true);
        key(&app, slint::platform::Key::Back);
        assert!(!app.get_sheet_open());
    }

    #[test]
    fn tv_devices_continue_directional_selection_from_the_mouse_clicked_row() {
        if !crate::isolation::run(concat!(
            module_path!(),
            "::",
            "tv_devices_continue_directional_selection_from_the_mouse_clicked_row"
        )) {
            return;
        }
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = nova::AppWindow::new().unwrap();
        app.set_show_home(false);
        app.set_remote_devices_open(true);
        app.window().set_size(slint::PhysicalSize::new(900, 700));
        app.global::<nova::InterfaceStyle>().set_tv(true);
        app.global::<nova::Anim>().set_enabled(false);
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = opened.clone();
        app.on_remote_device_open(move |id| observed.borrow_mut().push(id.to_string()));
        app.set_remote_devices(
            std::rc::Rc::new(slint::VecModel::from(vec![
                nova::RemoteDeviceRow {
                    id: "first".into(),
                    name: "First device".into(),
                    ready: true,
                    has_session: true,
                    ..Default::default()
                },
                nova::RemoteDeviceRow {
                    id: "second".into(),
                    name: "Second device".into(),
                    ready: true,
                    has_session: true,
                    ..Default::default()
                },
            ]))
            .into(),
        );
        app.show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(100));
        let first =
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&app, "Controls")
                .next()
                .unwrap();
        let p = first.absolute_position();
        let size = first.size();
        let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
        assert_eq!(&*opened.borrow(), &["first"]);
        for key in [
            slint::platform::Key::DownArrow,
            slint::platform::Key::Return,
        ] {
            let text: slint::SharedString = key.into();
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        }
        assert_eq!(
            &*opened.borrow(),
            &["first", "second"],
            "Down continues from the mouse-selected device"
        );
    }
}

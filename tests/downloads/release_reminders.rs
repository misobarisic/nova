//! Reminder tools remain reachable at phone widths and with a TV remote.
use i_slint_backend_testing::ElementHandle;
use slint::{
    ComponentHandle, ModelRc, VecModel,
    platform::{Key, WindowEvent},
};
use std::{cell::RefCell, rc::Rc, time::Duration};
fn key(app: &nova::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}
fn tap(app: &nova::AppWindow, element: ElementHandle) {
    let position = element.absolute_position();
    let size = element.size();
    let point =
        slint::LogicalPosition::new(position.x + size.width / 2., position.y + size.height / 2.);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: point,
        button: slint::platform::PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: point,
        button: slint::platform::PointerEventButton::Left,
    });
}
#[test]
fn reminder_tools_fit_phone_and_tv_back_closes_one_layer() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::reminder_tools_fit_phone_and_tv_back_closes_one_layer"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, tv) in [(360, false), (1200, false), (1920, true)] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window().set_size(slint::PhysicalSize::new(width, 1080));
        app.invoke_navigate_section(4);
        let calendar = app.global::<nova::CalendarBridge>();
        calendar.set_reminder_keys(ModelRc::new(VecModel::from(vec![
            "[\"series\",\"one\"]".into(),
        ])));
        calendar.set_reminder_labels(ModelRc::new(VecModel::from(vec![
            "A saved series — paused (On Hold / Dropped)".into(),
        ])));
        calendar.set_reminder_subscribed(ModelRc::new(VecModel::from(vec![true])));
        calendar.set_export_selected(ModelRc::new(VecModel::from(vec![false])));
        calendar.set_export_start("2026-10-23".into());
        calendar.set_export_end("2027-01-21".into());
        let actions = Rc::new(RefCell::new(Vec::new()));
        let captured = actions.clone();
        calendar.on_reminder_action(move |action| captured.borrow_mut().push(action));
        app.show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        let tools = ElementHandle::find_by_element_id(&app, "CalendarPage::tools_button")
            .next()
            .unwrap();
        let pos = tools.absolute_position();
        let size = tools.size();
        assert!(pos.x >= 0. && size.width > 100. && pos.x + size.width <= width as f32 + 1.);
        tap(&app, tools);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Done")
                .next()
                .is_some()
        );
        if tv {
            key(&app, Key::DownArrow.into());
            key(&app, Key::Return.into());
            assert_eq!(*actions.borrow(), vec![0]);
        } else {
            key(&app, Key::Tab.into());
            key(&app, Key::Return.into());
            assert_eq!(*actions.borrow(), vec![0]);
        }
        calendar.set_tools_focus(8);
        key(&app, Key::Return.into());
        key(&app, "0".into());
        assert!(calendar.get_export_start().ends_with('0'));
        key(&app, Key::Back.into());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Done")
                .next()
                .is_some(),
            "Back leaves date editing before closing the panel"
        );
        key(&app, Key::Back.into());
        assert!(app.get_show_calendar());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Done")
                .next()
                .is_none()
        );
        app.hide().unwrap();
    }
}

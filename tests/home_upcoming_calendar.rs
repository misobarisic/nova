//! Home Upcoming opens the full Calendar planner at phone and desktop widths.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, VecModel, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn tap(app: &nova::AppWindow, element: ElementHandle) {
    let p = element.absolute_position();
    let size = element.size();
    let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
}

#[test]
fn upcoming_calendar_shortcut_opens_full_planner() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for width in [360, 1200] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.set_show_home(true);
        app.set_home_view(2);
        app.set_home_upcoming(ModelRc::new(VecModel::from(vec![nova::UpcomingRow {
            id: "series".into(),
            title: "Upcoming series".into(),
            index: 0,
            ..Default::default()
        }])));
        app.window().set_size(slint::PhysicalSize::new(width, 800));
        let opened = Rc::new(Cell::new(0));
        let count = opened.clone();
        let weak = app.as_weak();
        // Mirror the production callback: the shortcut changes destinations,
        // rather than mutating the Home calendar's embedded open state.
        app.on_calendar_picked(move || {
            count.set(count.get() + 1);
            let app = weak.upgrade().unwrap();
            app.set_show_home(false);
            app.set_show_calendar(true);
        });
        let weak = app.as_weak();
        app.on_home_picked(move || {
            let app = weak.upgrade().unwrap();
            app.set_show_calendar(false);
            app.set_show_home(true);
            app.set_home_view(0);
        });
        app.show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        let shortcut = ElementHandle::find_by_element_type_name(&app, "CalToggle")
            .next()
            .unwrap();
        assert!(shortcut.size().width > 0.0 && shortcut.size().height > 0.0);
        tap(&app, shortcut);
        assert_eq!(opened.get(), 1);
        assert!(app.get_show_calendar());
        assert!(!app.get_show_home() && !app.get_home_cal_open());
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "CalendarPage").count(),
            1
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "CalDayCell").count(),
            0
        );
        for event in [
            WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            },
            WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            },
        ] {
            app.window().dispatch_event(event);
        }
        assert!(app.get_show_home() && !app.get_show_calendar() && !app.get_more_open());
    }
}

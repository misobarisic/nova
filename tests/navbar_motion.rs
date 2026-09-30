//! Icon-only feedback on both responsive navbars: highlight snaps, actual
//! clicks, interrupted pops, rebuilds and independent motion switches.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition};
use std::time::Duration;

fn marker(app: &nova::AppWindow, wide: bool) -> ElementHandle {
    let id = if wide {
        "SideNav::marker_dot"
    } else {
        "BottomNav::marker_dot"
    };
    ElementHandle::find_by_element_id(app, id)
        .next()
        .expect("nav marker")
}

fn center(item: &ElementHandle) -> LogicalPosition {
    let p = item.absolute_position();
    let s = item.size();
    LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0)
}

fn icon(app: &nova::AppWindow, wide: bool, index: usize) -> ElementHandle {
    let component = if wide { "SideItem" } else { "BottomNav" };
    let name = [
        "icon_home",
        "icon_discover",
        "icon_library",
        "icon_settings",
    ][index];
    ElementHandle::find_by_element_id(app, &format!("{component}::{name}"))
        .next()
        .expect("nav icon")
}

fn idle(app: &nova::AppWindow, wide: bool, ms: u64) {
    // A renderer normally evaluates the progress binding every frame.
    for _ in 0..ms {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let m = marker(app, wide);
        let _ = (m.absolute_position(), m.size());
        for index in 0..4 {
            let _ = icon(app, wide, index).size();
        }
    }
}

fn switch(app: &nova::AppWindow, next: i32) {
    let from = if app.get_show_home() {
        0
    } else if app.get_show_library() {
        2
    } else if app.get_show_settings() {
        3
    } else {
        1
    };
    if from != next {
        app.global::<nova::NavState>().set_from(from);
    }
    app.set_show_home(next == 0);
    app.set_show_library(next == 2);
    app.set_show_settings(next == 3);
}

fn click(app: &nova::AppWindow, wide: bool, index: usize) {
    let item = if wide {
        ElementHandle::find_by_element_type_name(app, "SideItem")
            .nth(index)
            .unwrap()
    } else {
        let id = [
            "BottomNav::tab_home",
            "BottomNav::tab_discover",
            "BottomNav::tab_library",
            "BottomNav::tab_settings",
        ][index];
        ElementHandle::find_by_element_id(app, id).next().unwrap()
    };
    let position = center(&item);
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
}

#[test]
fn nav_icons_pop_while_highlights_snap() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().show().unwrap();
    let weak = app.as_weak();
    app.on_home_picked(move || switch(&weak.upgrade().unwrap(), 0));
    let weak = app.as_weak();
    app.on_discover_picked(move || switch(&weak.upgrade().unwrap(), 1));
    let weak = app.as_weak();
    app.on_library_picked(move || switch(&weak.upgrade().unwrap(), 2));
    let weak = app.as_weak();
    app.on_settings_picked(move || switch(&weak.upgrade().unwrap(), 3));

    for wide in [true, false] {
        let anim = app.global::<nova::Anim>();
        anim.set_enabled(true);
        anim.set_nav_slide(true);
        switch(&app, 0);
        app.window()
            .set_size(slint::PhysicalSize::new(if wide { 1100 } else { 360 }, 800));
        idle(&app, wide, 400);
        let axis = |p: LogicalPosition| if wide { p.y } else { p.x };
        let home = axis(center(&marker(&app, wide)));
        click(&app, wide, 3);
        // The page switch is immediate, even while feedback is in flight.
        assert!(app.get_show_settings());
        idle(&app, wide, 1);
        let selected = center(&marker(&app, wide));
        assert!(
            axis(selected) > home,
            "highlight snaps to Settings immediately"
        );
        assert_eq!(
            marker(&app, wide).size(),
            slint::LogicalSize::new(48.0, 48.0)
        );
        idle(&app, wide, 90);
        assert_eq!(
            center(&marker(&app, wide)),
            selected,
            "highlight never travels"
        );
        let size = icon(&app, wide, 3).size().width;
        assert!(
            size > 24.5 && size <= 26.0,
            "selected icon pops: wide={wide}, size={size}"
        );
        for index in 0..3 {
            assert_eq!(icon(&app, wide, index).size().width, 24.0);
        }

        // Interrupt a pop with a new click: only the incoming icon animates.
        click(&app, wide, 1);
        assert!(!app.get_show_settings() && !app.get_show_home());
        idle(&app, wide, 1);
        assert_eq!(icon(&app, wide, 3).size().width, 24.0);
        let discover = axis(center(&marker(&app, wide)));
        assert!(discover > home && discover < axis(selected));
        idle(&app, wide, 90);
        assert!(
            icon(&app, wide, 1).size().width > 24.5,
            "incoming icon pops"
        );
        idle(&app, wide, 400);
        let settled = marker(&app, wide);
        assert!((settled.size().width - 48.0).abs() < 0.5);
        assert!((settled.size().height - 48.0).abs() < 0.5);
        assert_eq!(icon(&app, wide, 1).size().width, 24.0);
        // Clicking the already selected item does not replay feedback.
        click(&app, wide, 1);
        idle(&app, wide, 90);
        assert_eq!(icon(&app, wide, 1).size().width, 24.0);

        app.set_modal_visible(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        app.set_modal_visible(false);
        idle(&app, wide, 1);
        assert!(
            (axis(center(&marker(&app, wide))) - discover).abs() < 0.5,
            "returning from detail must not replay motion"
        );
        idle(&app, wide, 90);
        assert_eq!(icon(&app, wide, 1).size().width, 24.0);

        for (enabled, nav) in [(true, false), (false, true)] {
            anim.set_enabled(enabled);
            anim.set_nav_slide(nav);
            click(&app, wide, 0);
            idle(&app, wide, 1);
            idle(&app, wide, 90);
            let m = marker(&app, wide);
            assert!((axis(center(&m)) - home).abs() < 0.5);
            assert_eq!(m.size(), slint::LogicalSize::new(48.0, 48.0));
            assert_eq!(icon(&app, wide, 0).size().width, 24.0);
            click(&app, wide, 1);
            idle(&app, wide, 1);
        }

        anim.set_enabled(true);
        anim.set_nav_slide(true);
        click(&app, wide, 0);
        idle(&app, wide, 90);
        assert!(icon(&app, wide, 0).size().width > 24.5);
        anim.set_nav_slide(false);
        idle(&app, wide, 1);
        assert!(
            (axis(center(&marker(&app, wide))) - home).abs() < 0.5,
            "disabling feedback must not move the highlight"
        );
        assert_eq!(icon(&app, wide, 0).size().width, 24.0);
    }
}

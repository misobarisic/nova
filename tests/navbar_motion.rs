//! Icon feedback on both responsive navbars: highlight snaps, actual
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

fn check_marker_shape(item: &ElementHandle, wide: bool) {
    let size = item.size();
    if wide {
        assert_eq!(size, slint::LogicalSize::new(48.0, 48.0));
    } else {
        assert!(size.width >= 56.0, "phone selection fills its tab slot");
        assert!((size.height - 60.0).abs() < 0.5);
    }
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
        check_marker_shape(&marker(&app, wide), wide);
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
        check_marker_shape(&settled, wide);
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
            check_marker_shape(&m, wide);
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

    // Padded page hosts and Home's edge-to-edge host must produce the same
    // full-width bar. Cutouts protect the labels rather than inset the panel.
    app.global::<nova::Anim>().set_enabled(false);
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(28.0, 16.0, 12.0, 8.0),
    );
    for width in [320, 390, 620] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        for scroll_page in [false, true] {
            app.set_discover_scroll_page(scroll_page);
            for page in 0..4 {
                switch(&app, page);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
                let panel = ElementHandle::find_by_element_id(&app, "BottomNav::pill")
                    .next()
                    .unwrap();
                assert!(panel.absolute_position().x.abs() < 0.5);
                assert!((panel.size().width - width as f32).abs() < 0.5);
                for name in ["home", "discover", "library", "settings"] {
                    let label = ElementHandle::find_by_element_id(
                        &app,
                        &format!("BottomNav::label_{name}"),
                    )
                    .next()
                    .unwrap();
                    let p = label.absolute_position();
                    let size = label.size();
                    assert!(p.x >= 12.0);
                    assert!(p.x + size.width <= width as f32 - 8.0);
                    assert!(p.y + size.height <= panel.absolute_position().y + 76.0);
                }
            }
        }
    }
}

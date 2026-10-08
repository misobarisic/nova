//! Slider drags must not turn vertical pointer wobble into page scrolling.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition};
use std::time::Duration;

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn press(app: &nova::AppWindow, position: LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn moved(app: &nova::AppWindow, position: LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
}

fn release(app: &nova::AppWindow, position: LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
}

fn slider(app: &nova::AppWindow, id: i32, wide: bool) -> ElementHandle {
    app.window()
        .set_size(slint::PhysicalSize::new(if wide { 1600 } else { 560 }, 650));
    app.set_settings_selected_id(id);
    app.set_settings_detail_open(true);
    app.set_settings_scroll_y(0.0);
    idle(500);
    for attempt in 0..30 {
        if let Some(slider) = ElementHandle::find_by_element_type_name(app, "SliderTouchArea")
            .find(|element| element.size().width > 36.0)
        {
            let y = slider.absolute_position().y;
            if y > 350.0 {
                app.set_settings_scroll_y(app.get_settings_scroll_y() - (y - 300.0));
                idle(50);
            }
            return slider;
        }
        app.set_settings_scroll_y(-50.0 * (attempt + 1) as f32);
        idle(50);
    }
    panic!(
        "no slider: section={id}, selected={}, detail={}, scroll={}",
        app.get_settings_selected_id(),
        app.get_settings_detail_open(),
        app.get_settings_scroll_y()
    );
}

#[test]
fn slider_drags_hold_page_scroll_and_release_it_afterwards() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_cache_enabled(true);
    app.window().show().unwrap();

    for wide in [false, true] {
        // Image cache exercises NumericSetting; P2P exercises standalone
        // MiniSlider, and Player uses its separate playback-speed control.
        for (id, hold_ms) in [(2, 0), (2, 120), (4, 0), (4, 120), (5, 0), (5, 120)] {
            app.set_cache_quality(85.0);
            app.set_torrent_max_mb(8192.0);
            app.set_playback_speed(1.0);
            app.set_show_torrents(true);
            let track = slider(&app, id, wide);
            let p = track.absolute_position();
            let size = track.size();
            let from = LogicalPosition::new(p.x + size.width * 0.2, p.y + size.height / 2.0);
            let to = LogicalPosition::new(p.x + size.width * 0.8, from.y - 35.0);
            let before = app.get_settings_scroll_y();
            let value_before = match id {
                2 => app.get_cache_quality(),
                4 => app.get_torrent_max_mb(),
                _ => app.get_playback_speed(),
            };
            moved(&app, from);
            idle(16);
            press(&app, from);
            // Cover both an immediate drag and a press held past the native delay.
            idle(hold_ms);
            for step in 1..=5 {
                let t = step as f32 / 5.0;
                moved(
                    &app,
                    LogicalPosition::new(
                        from.x + (to.x - from.x) * t,
                        from.y + (to.y - from.y) * t,
                    ),
                );
                idle(16);
                assert!(
                    (app.get_settings_scroll_y() - before).abs() < 0.5,
                    "slider moved page: wide={wide}, section={id}"
                );
            }
            release(&app, to);
            idle(50);
            let value_after = match id {
                2 => app.get_cache_quality(),
                4 => app.get_torrent_max_mb(),
                _ => app.get_playback_speed(),
            };
            assert!(
                (value_after - value_before).abs() > 0.01,
                "slider must change its value: section={id}"
            );
            assert!((app.get_settings_scroll_y() - before).abs() < 0.5);

            // A new vertical gesture on the page must work after release.
            let from = LogicalPosition::new(if wide { 490.0 } else { 25.0 }, 480.0);
            moved(&app, from);
            idle(16);
            press(&app, from);
            let direction = if before < -10.0 { 1.0 } else { -1.0 };
            moved(
                &app,
                LogicalPosition::new(from.x, from.y + direction * 40.0),
            );
            idle(16);
            moved(
                &app,
                LogicalPosition::new(from.x, from.y + direction * 80.0),
            );
            release(
                &app,
                LogicalPosition::new(from.x, from.y + direction * 80.0),
            );
            idle(50);
            assert!(
                (app.get_settings_scroll_y() - before).abs() > 10.0,
                "page scroll stayed locked: wide={wide}, section={id}"
            );

            let track = slider(&app, id, wide);
            let p = track.absolute_position();
            press(
                &app,
                LogicalPosition::new(p.x + track.size().width / 2.0, p.y + 15.0),
            );
            idle(120);
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerExited);
            idle(16);
            let before = app.get_settings_scroll_y();
            moved(&app, from);
            idle(16);
            press(&app, from);
            let direction = if before < -10.0 { 1.0 } else { -1.0 };
            moved(
                &app,
                LogicalPosition::new(from.x, from.y + direction * 80.0),
            );
            release(
                &app,
                LogicalPosition::new(from.x, from.y + direction * 80.0),
            );
            idle(50);
            assert!(
                (app.get_settings_scroll_y() - before).abs() > 10.0,
                "cancelled slider left page locked: section={id}, before={before}, after={}",
                app.get_settings_scroll_y()
            );
        }
    }
}

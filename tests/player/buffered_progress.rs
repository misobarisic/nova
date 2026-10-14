//! Estimate passes through the player bridge and stays within the unplayed bar.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::time::Duration;

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

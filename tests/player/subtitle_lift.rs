//! The real OSD publishes control clearance and its live animated opacity.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

fn advance(app: &nova::AppWindow, millis: u64) {
    for _ in 0..millis.div_ceil(10) {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
    let _ = ElementHandle::find_by_element_id(app, "PlayerOverlay::osd")
        .next()
        .unwrap()
        .computed_opacity();
}

#[test]
fn subtitle_clearance_tracks_controls_fade_rotation_and_reduced_motion() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "subtitle_clearance_tracks_controls_fade_rotation_and_reduced_motion"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    let published = Rc::new(Cell::new((0.0_f32, 0.0_f32)));
    let recorded = published.clone();
    app.on_subtitle_clearance_changed(move |fraction, opacity| recorded.set((fraction, opacity)));
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_osd_visible(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.window().show().unwrap();
    for (width, height, android) in [(390, 844, true), (900, 360, true), (1280, 720, false)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        advance(&app, 30);
        let osd = ElementHandle::find_by_element_id(&app, "PlayerOverlay::osd")
            .next()
            .unwrap();
        let (fraction, opacity) = published.get();
        assert!((fraction - (osd.size().height + 8.0) / height as f32).abs() < 0.002);
        assert_eq!(opacity, 1.0);
        app.set_osd_visible(false);
        advance(&app, 30);
        assert_eq!(published.get().1, 0.0);
        app.set_osd_visible(true);
        advance(&app, 30);
    }
    app.global::<nova::Anim>().set_enabled(true);
    app.set_osd_visible(false);
    advance(&app, 60);
    let fading = published.get().1;
    assert!(
        fading > 0.0 && fading < 1.0,
        "clearance must follow the fade: {fading}"
    );
    advance(&app, 200);
    assert_eq!(published.get().1, 0.0);
    app.set_osd_visible(true);
    advance(&app, 60);
    assert!(published.get().1 > 0.0 && published.get().1 < 1.0);
    advance(&app, 200);
    assert_eq!(published.get().1, 1.0);
    // Loading also keeps the controls and their subtitle clearance engaged.
    app.set_osd_visible(false);
    app.set_playback_started(false);
    advance(&app, 220);
    assert_eq!(published.get().1, 1.0);
}

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
    if let Some(osd) = ElementHandle::find_by_element_id(app, "PlayerOverlay::osd").next() {
        let _ = osd.computed_opacity();
    }
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
        let footer = ElementHandle::find_by_element_id(&app, "PlayerOverlay::player_header")
            .next()
            .unwrap();
        let (fraction, opacity) = published.get();
        assert!(
            (fraction
                - ((height as f32 - footer.absolute_position().y + 8.0) / height as f32).min(1.0))
            .abs()
                < 0.002
        );
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
    // Initial loading has no transport OSD or subtitle clearance.
    app.set_osd_visible(false);
    app.set_playback_started(false);
    advance(&app, 220);
    assert_eq!(published.get().1, 0.0);
}

#[test]
fn next_episode_offer_keeps_subtitles_clear_with_hidden_and_fading_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::next_episode_offer_keeps_subtitles_clear_with_hidden_and_fading_controls"
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
    app.set_next_episode_title("The next episode".into());
    app.set_next_episode_context("S1 E2".into());
    app.global::<nova::Anim>().set_enabled(false);
    app.window().show().unwrap();
    for (width, height) in [(390, 844), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for android in [false, true] {
            app.set_is_android(android);
            app.set_osd_visible(false);
            app.set_next_episode_visible(false);
            advance(&app, 30);
            assert_eq!(published.get().1, 0.0);
            app.set_next_episode_visible(true);
            advance(&app, 30);
            let assert_banner_clearance = || {
                let banner =
                    ElementHandle::find_by_element_id(&app, "PlayerOverlay::next_episode_banner")
                        .next()
                        .unwrap();
                let (fraction, opacity) = published.get();
                assert_eq!(
                    opacity, 1.0,
                    "the offer remains visible independently of OSD"
                );
                assert!(fraction > 0.0 && fraction <= 1.0);
                let baseline = height as f32 * (1.0 - fraction);
                assert!(
                    baseline <= banner.absolute_position().y - 7.5,
                    "subtitle baseline {baseline} overlaps the offer at {:?}",
                    banner.absolute_position()
                );
                assert_eq!(app.get_osd_visible(), app.get_osd_opacity() > 0.0);
            };
            assert_banner_clearance();
            app.set_osd_visible(true);
            advance(&app, 30);
            assert_banner_clearance();

            // The independent offer must retain full clearance throughout the
            // control fade; dismissing it restores the ordinary fade contract.
            app.global::<nova::Anim>().set_enabled(true);
            app.set_osd_visible(false);
            advance(&app, 60);
            assert!(app.get_osd_opacity() > 0.0 && app.get_osd_opacity() < 1.0);
            assert_eq!(published.get().1, 1.0);
            advance(&app, 200);
            assert_banner_clearance();
            app.set_next_episode_visible(false);
            advance(&app, 30);
            assert_eq!(published.get().1, 0.0);
            app.global::<nova::Anim>().set_enabled(false);
        }
    }
}

//! Artwork episode cards keep text inside the card and omit empty playback UI.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{rc::Rc, time::Duration};

fn elements(app: &nova::AppWindow, name: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_id(app, &format!("DetailPage::{name}")).collect()
}

#[test]
fn artwork_cards_contain_text_and_only_show_recorded_playback() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Episode cards".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 2".into()])).into());
    app.set_season_combo_idx(0);
    app.set_episode_rows(
        Rc::new(VecModel::from(
            [(false, 0.0), (true, 0.0), (false, 0.45), (true, 1.0)]
                .into_iter()
                .enumerate()
                .map(|(i, (watched, progress))| nova::EpisodeRow {
                    text: "A Friend for This Crimson Demon Girl! With a Very Long Title".into(),
                    details: "As Kazuma notices that Darkness hasn't come back since negotiating with Alderp, Megumin brings in a cat that she found in the street.".into(),
                    ep_no: format!("S2 E{}", i + 1).into(),
                    date: if i == 2 { "" } else { "Apr 19, 2017" }.into(),
                    runtime: if i == 0 || i == 2 { "23m" } else { "" }.into(),
                    watched,
                    progress,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.window().show().unwrap();
    // Keep every card footer visible below the taller search toolbar; the
    // accessibility query excludes children clipped outside the viewport.
    for width in [320, 390, 620, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 2000));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
        let cards = elements(&app, "ep_card");
        assert_eq!(cards.len(), 4);
        assert_eq!(elements(&app, "episode_watched").len(), 2);
        assert_eq!(elements(&app, "episode_progress").len(), 2);
        assert_eq!(elements(&app, "episode_runtime").len(), 2);
        assert_eq!(elements(&app, "episode_progress_track").len(), 2);
        let percentages: Vec<_> = elements(&app, "episode_percentage")
            .into_iter()
            .filter_map(|element| element.accessible_label())
            .collect();
        assert_eq!(percentages, ["45%", "100%"]);
        for name in ["episode_title", "episode_synopsis", "episode_content"] {
            for child in elements(&app, name) {
                let p = child.absolute_position();
                let size = child.size();
                assert!(
                    cards.iter().any(|card| {
                        let origin = card.absolute_position();
                        let bounds = card.size();
                        p.x >= origin.x - 0.5
                            && p.y >= origin.y + 45.0
                            && p.x + size.width <= origin.x + bounds.width + 0.5
                            && p.y + size.height <= origin.y + bounds.height - 10.0
                    }),
                    "{name} must fit below the badges at {width}px"
                );
            }
        }
    }
    // Missing/unpublished artwork should not leave a full-size empty canvas.
    // Mix compact and full cards to cover the wide grid's shared row layout.
    app.set_episode_rows(
        Rc::new(VecModel::from(vec![
            nova::EpisodeRow {
                text: "Episode 1".into(),
                ep_no: "S1 E1".into(),
                date: "Oct 4, 2026".into(),
                ..Default::default()
            },
            nova::EpisodeRow {
                text: "A Very Long Episode Title That Wraps Across Two Lines Without Artwork"
                    .into(),
                ep_no: "S1 E2".into(),
                date: "Oct 4, 2026".into(),
                runtime: "24m".into(),
                progress: 0.45,
                watched: true,
                ..Default::default()
            },
            nova::EpisodeRow {
                text: "An episode with artwork".into(),
                ep_no: "S1 E3".into(),
                has_thumb: true,
                thumb: slint::Image::from_rgba8(slint::SharedPixelBuffer::new(1, 1)),
                ..Default::default()
            },
            nova::EpisodeRow {
                text: "An episode with a synopsis".into(),
                ep_no: "S1 E4".into(),
                details: "Episode information remains readable even without artwork.".into(),
                ..Default::default()
            },
        ]))
        .into(),
    );
    for width in [320, 390, 620, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 2000));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
        let cards = elements(&app, "ep_card");
        assert_eq!(cards.len(), 4);
        assert!(
            cards[0].size().height < 170.0,
            "compact placeholder at {width}px"
        );
        assert!(
            cards[1].size().height < 210.0,
            "long compact footer at {width}px"
        );
        assert!(
            cards[2].size().height >= 240.0,
            "artwork keeps its canvas at {width}px"
        );
        assert!(
            cards[3].size().height >= 240.0,
            "synopsis keeps its canvas at {width}px"
        );
        for child in elements(&app, "episode_content") {
            let p = child.absolute_position();
            let size = child.size();
            assert!(
                cards.iter().any(|card| {
                    let origin = card.absolute_position();
                    let bounds = card.size();
                    p.x >= origin.x - 0.5
                        && p.y >= origin.y + 52.0
                        && p.x + size.width <= origin.x + bounds.width + 0.5
                        && p.y + size.height <= origin.y + bounds.height - 10.0
                }),
                "compact content must clear badges and fit its card at {width}px"
            );
        }
    }
}

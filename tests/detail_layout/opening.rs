//! Incoming Detail shells stay at the top; only user navigation reveals content.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::{rc::Rc, time::Duration};

#[test]
fn tab_underline_retargets_and_resizes_without_moving_content() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "tab_underline_retargets_and_resizes_without_moving_content"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_in_library(true);
    app.set_selected_title("Stable tabs".into());
    app.show().unwrap();
    let underline = || element(&app, "DetailTabs::tab_underline");
    let advance = |ms| {
        for _ in 0..ms {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
            let _ = underline().absolute_position();
        }
    };
    for width in [390, 820, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1400));
        app.set_detail_is_movie(false);
        app.set_detail_tab(0);
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_transitions(true);
        advance(300);
        let start = underline().absolute_position();
        let top = title_y(&app);
        app.set_detail_tab(3);
        advance(80);
        let middle = underline().absolute_position();
        assert!(middle.x > start.x);
        app.set_detail_tab(4);
        advance(300);
        let end = underline().absolute_position();
        assert!(end.x > middle.x);
        assert_eq!(start.y, end.y, "tab content must not move vertically");
        assert_still_at(&app, top);
        for (master, transitions) in [(true, false), (false, true)] {
            app.global::<nova::Anim>().set_enabled(master);
            app.global::<nova::Anim>().set_transitions(transitions);
            app.set_detail_tab(0);
            advance(1);
            assert_eq!(underline().absolute_position().x, start.x);
            app.set_detail_tab(4);
            advance(1);
            assert_eq!(underline().absolute_position().x, end.x);
        }
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_transitions(true);
        app.set_detail_tab(0);
        advance(70);
        app.global::<nova::Anim>().set_enabled(false);
        advance(1);
        assert_eq!(underline().absolute_position().x, start.x);
        // Movies omit Episodes: Tracking uses the second slot, including
        // after a viewport resize, with no delayed position adjustment.
        app.set_detail_is_movie(true);
        app.set_detail_tab(4);
        advance(1);
        let movie_x = underline().absolute_position().x;
        assert!(movie_x > start.x && movie_x < end.x);
        app.window()
            .set_size(slint::PhysicalSize::new(width + 20, 1400));
        advance(1);
        let resized = underline().absolute_position();
        advance(300);
        assert_eq!(underline().absolute_position(), resized);
    }
}

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn title_y(app: &nova::AppWindow) -> f32 {
    element(app, "DetailPage::detail_title")
        .absolute_position()
        .y
}

fn assert_still_at(app: &nova::AppWindow, y: f32) {
    assert!(
        (title_y(app) - y).abs() < 1.0,
        "Detail moved from {y} to {}",
        title_y(app)
    );
}

#[test]
fn rotation_keeps_the_visible_episode_in_view() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "rotation_keeps_the_visible_episode_in_view"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Keep my place".into());
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (0..24)
                .map(|i| nova::EpisodeRow {
                    id: format!("episode-{i}").into(),
                    text: format!("Episode {i}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_total(24);
    app.set_season_cards(
        Rc::new(VecModel::from(
            (1..=3)
                .map(|season| nova::SeasonCard {
                    name: format!("Season {season}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(24.0, 0.0, 20.0, 0.0),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    settle();
    (*app)
        .global::<nova_ui_shared::DetailContent>()
        .invoke_restore_scroll_y(-1800.0);
    settle();
    let reading_episode = || {
        let toolbar = element(&app, "DetailEpisodesModule::episode_toolbar");
        let reading_top = toolbar.absolute_position().y + toolbar.size().height;
        ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::episode_title")
            .filter(|item| item.absolute_position().y >= reading_top)
            .find_map(|item| item.accessible_label())
            .expect("an episode title below the pinned toolbar")
    };
    let reading_line = || {
        (*app)
            .global::<nova_ui_shared::DetailContent>()
            .get_episodes_viewport_top()
            + element(&app, "DetailEpisodesModule::episode_toolbar")
                .size()
                .height
            + 8.0
    };
    let reading_card = || {
        ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::ep_card")
            .find(|card| {
                let y = card.absolute_position().y;
                y <= reading_line() && y + card.size().height + 24.0 > reading_line()
            })
            .expect("an episode card crossing the reading line")
    };
    let card = reading_card();
    let fraction = (card.absolute_position().y - reading_line()) / card.size().height;
    let anchor = reading_episode();
    for (turn, (width, height)) in [
        (844, 390),
        (390, 844),
        (1200, 700),
        (390, 844),
        (844, 390),
        (390, 844),
    ]
    .into_iter()
    .enumerate()
    {
        app.global::<nova::Anim>().set_enabled(turn >= 3);
        app.global::<nova::Anim>().set_transitions(turn >= 3);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
            if width < height {
                i_slint_core::lengths::LogicalEdges::new(24.0, 0.0, 20.0, 0.0)
            } else {
                i_slint_core::lengths::LogicalEdges::new(0.0, 24.0, 0.0, 28.0)
            },
        );
        settle();
        let actual = (card.absolute_position().y - reading_line()) / card.size().height;
        assert!(
            (actual - fraction).abs() < 0.015,
            "rotation to {width}x{height} moved reading fraction {fraction} to {actual}"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, &anchor)
                .next()
                .is_some(),
            "rotation to {width}x{height} lost {anchor}; scroll {}",
            (*app)
                .global::<nova_ui_shared::DetailContent>()
                .invoke_scroll_y()
        );
    }
    // Later scrolling replaces the reading anchor, including returning to the
    // exact offset restored by the previous resize.
    let content = (*app).global::<nova_ui_shared::DetailContent>();
    let offset = content.invoke_scroll_y();
    content.invoke_restore_scroll_y(offset - 700.0);
    settle();
    content.invoke_restore_scroll_y(offset);
    settle();
    let anchor = reading_episode();
    let card = reading_card();
    let fraction = (card.absolute_position().y - reading_line()) / card.size().height;
    app.window().set_size(slint::PhysicalSize::new(844, 390));
    settle();
    let actual = (card.absolute_position().y - reading_line()) / card.size().height;
    assert!(
        (actual - fraction).abs() < 0.015,
        "scrolling must replace the saved reading fraction"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, &anchor)
            .next()
            .is_some()
    );
}

#[test]
fn opening_and_backend_focus_changes_do_not_scroll_but_keyboard_and_playback_do() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "opening_and_backend_focus_changes_do_not_scroll_but_keyboard_and_playback_do"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_selected_title("Opening stays at the top".into());
    app.window().show().unwrap();
    for (width, height) in [(390, 800), (820, 390), (1280, 800)] {
        app.set_modal_visible(false);
        settle();
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_modal_episodes(true);
        app.set_detail_tab(3);
        app.set_episode_rows(
            Rc::new(VecModel::from(
                (0..24)
                    .map(|i| nova::EpisodeRow {
                        text: format!("Episode {i}").into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_episode_total(24);
        app.set_kb_active(false);
        app.set_detail_kb_zone(3);
        app.set_detail_kb_ep(8);
        app.set_detail_episode_reveal_request(0);
        app.set_modal_visible(true);
        let top = title_y(&app);
        settle();
        assert_still_at(&app, top);
        // Data/season selection can update indices even after construction.
        for keyboard_mode in [false, true] {
            app.set_kb_active(keyboard_mode);
            for zone in [0, 3, 2] {
                app.set_detail_kb_zone(zone);
                settle();
                assert_still_at(&app, top);
            }
        }
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::DownArrow.into(),
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::DownArrow.into(),
            });
        settle();
        assert_eq!(app.get_detail_kb_zone(), 3);
        assert!(
            title_y(&app) < top - 100.0,
            "keyboard navigation must still scroll"
        );
        // Playback return explicitly reveals its episode, independently of
        // the input modality or any saved navigation indices.
        app.set_kb_active(false);
        app.set_detail_kb_ep(10);
        app.set_detail_episode_reveal_request(1);
        settle();
        let card = ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::episode_title")
            .find(|title| title.accessible_label().as_deref() == Some("Episode 10"))
            .expect("episode title after explicit reveal");
        let view = element(&app, "DetailPage::det_scroll");
        assert!(card.absolute_position().y >= view.absolute_position().y);
        assert!(card.absolute_position().y < view.absolute_position().y + view.size().height);
    }
}

#[test]
fn late_detail_logos_preserve_title_height_and_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "late_detail_logos_preserve_title_height_and_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.window().show().unwrap();
    for (width, height) in [(390, 800), (820, 390), (1280, 800)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for title in [
            "A short title",
            "A very long series title that wraps across multiple lines on a narrow phone",
        ] {
            app.set_selected_logo(Image::default());
            app.set_selected_title(title.into());
            settle();
            let title_height = element(&app, "DetailPage::detail_title").size().height;
            let action_y = element(&app, "DetailPage::watch_action_button")
                .absolute_position()
                .y;
            for (logo_w, logo_h) in [(600, 120), (120, 600), (240, 240)] {
                app.set_selected_logo(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(
                    logo_w, logo_h,
                )));
                settle();
                let title = element(&app, "DetailPage::detail_title");
                assert!((title.size().height - title_height).abs() < 1.0);
                assert!(
                    (element(&app, "DetailPage::watch_action_button")
                        .absolute_position()
                        .y
                        - action_y)
                        .abs()
                        < 1.0
                );
                let image = title
                    .query_descendants()
                    .match_id("TitleArtwork::title_logo")
                    .find_first()
                    .unwrap();
                assert!(
                    (image.size().width / image.size().height - logo_w as f32 / logo_h as f32)
                        .abs()
                        < 0.1
                );
            }
            app.set_selected_logo(Image::default());
            settle();
            assert!(
                (element(&app, "DetailPage::detail_title").size().height - title_height).abs()
                    < 1.0
            );
            assert!(
                (element(&app, "DetailPage::watch_action_button")
                    .absolute_position()
                    .y
                    - action_y)
                    .abs()
                    < 1.0
            );
        }
    }
}

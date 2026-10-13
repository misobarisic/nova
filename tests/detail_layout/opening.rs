//! Incoming Detail shells stay at the top; only user navigation reveals content.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::{rc::Rc, time::Duration};

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

//! Player header uses bounded title artwork and screenshot taps respect OSD state.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, Image, LogicalPosition, Rgba8Pixel, SharedPixelBuffer};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
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
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let s = element.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    settle(app);
}

#[test]
fn title_artwork_fits_and_screenshot_is_guarded() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_player_episode_title("S1E1: A long episode title".into());
    app.set_player_show_title("The show's title".into());
    app.set_player_title("Removed bottom title".into());
    let logo = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(600, 120));
    app.set_player_show_logo(logo.clone());
    app.window().show().unwrap();
    let captures = Rc::new(Cell::new(0));
    let recorded = captures.clone();
    app.on_take_screenshot(move || recorded.set(recorded.get() + 1));
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));

    for (width, height) in [(320, 568), (390, 844), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for android in [true, false] {
            app.set_is_android(android);
            settle(&app);
            let heading = element(&app, "PlayerOverlay::episode_heading");
            let camera = element(&app, "PlayerOverlay::screenshot_button");
            let artwork = element(&app, "TitleArtwork::title_logo");
            assert!(heading.absolute_position().x >= 68.0);
            assert!(
                heading.absolute_position().x + heading.size().width
                    <= camera.absolute_position().x - 7.5
            );
            assert!(
                artwork.absolute_position().y
                    >= heading.absolute_position().y + heading.size().height
            );
            assert!(artwork.size().height <= 64.1);
            assert!((artwork.size().width / artwork.size().height - 5.0).abs() < 0.1);
            assert!(
                artwork.absolute_position().x + artwork.size().width
                    <= camera.absolute_position().x
            );
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Removed bottom title")
                    .next()
                    .is_none()
            );
            app.set_player_show_logo(Image::default());
            settle(&app);
            assert!(
                ElementHandle::find_by_element_id(&app, "TitleArtwork::title_logo")
                    .next()
                    .is_none()
            );
            assert!(element(&app, "TitleArtwork::title_text").size().height > 0.0);
            app.set_player_show_logo(logo.clone());
        }
    }

    // Screenshot notices and hold feedback must occupy separate rows, including
    // standalone playback without a title header.
    app.set_playback_started(true);
    app.set_duration(600.0);
    let previews = Rc::new(Cell::new(0));
    let recorded = previews.clone();
    app.on_playback_speed_preview(move |speed| {
        if speed == 2.0 {
            recorded.set(recorded.get() + 1);
        }
    });
    for (width, height) in [(320, 568), (800, 360)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for title in ["The show's title", ""] {
            app.set_player_show_title(title.into());
            app.set_player_show_logo(Image::default());
            app.set_player_episode_title(
                if title.is_empty() {
                    ""
                } else {
                    "S1E1: Episode"
                }
                .into(),
            );
            app.set_screenshot_notice("Screenshot saved: Pictures/Nova/a-long-filename.png".into());
            settle(&app);
            let position = LogicalPosition::new(width as f32 / 2.0, height as f32 / 2.0);
            let before = previews.get();
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
            slint::platform::update_timers_and_animations();
            settle(&app);
            assert_eq!(previews.get(), before + 1, "hold must show live feedback");
            let notice = element(&app, "PlayerOverlay::screenshot_message");
            let feedback = element(&app, "PlayerOverlay::flash_pill");
            assert!(
                feedback.absolute_position().y
                    >= notice.absolute_position().y + notice.size().height + 8.0,
                "screenshot status must not collide with playback feedback: viewport={width}x{height}, title={title:?}, visible={}, notice={:?}/{:?}, feedback={:?}/{:?}",
                app.get_osd_visible(),
                notice.absolute_position(),
                notice.size(),
                feedback.absolute_position(),
                feedback.size()
            );
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
        }
    }
    app.set_screenshot_notice("".into());
    app.set_playback_started(false);
    app.set_is_android(false);
    settle(&app);
    click(&app, &element(&app, "PlayerOverlay::screenshot_button"));
    assert_eq!(captures.get(), 0, "loading cannot capture");
    app.set_playback_started(true);
    app.set_screenshot_busy(true);
    click(&app, &element(&app, "PlayerOverlay::screenshot_button"));
    assert_eq!(captures.get(), 0, "pending screenshot cannot duplicate");
    app.set_screenshot_busy(false);
    app.set_osd_visible(false);
    settle(&app);
    click(&app, &element(&app, "PlayerOverlay::screenshot_button"));
    assert!(app.get_osd_visible(), "first hidden tap wakes controls");
    assert_eq!(captures.get(), 0);
    click(&app, &element(&app, "PlayerOverlay::screenshot_button"));
    assert_eq!(captures.get(), 1, "visible screenshot button captures once");
}

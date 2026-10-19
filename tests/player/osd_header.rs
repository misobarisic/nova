//! Player header uses text titles and screenshot taps respect OSD state.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition};
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
fn party_badge_keeps_the_top_lane_clear_of_bottom_captions_and_follows_osd_visibility() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::party_badge_keeps_the_top_lane_clear_of_bottom_captions_and_follows_osd_visibility"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_party_busy(true);
    app.set_player_show_title("A very long show title that must elide inside its own lane".into());
    app.window().show().unwrap();
    for (width, height) in [(320, 568), (390, 844), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for android in [false, true] {
            app.set_is_android(android);
            // Factory geometry also changes for asymmetric landscape cutouts.
            let bridge = app.global::<nova::PlayerOsdBridge>();
            bridge.set_safe_top(20.0);
            bridge.set_safe_left(36.0);
            bridge.set_safe_right(4.0);
            for episode in ["", "S1 E1 · A long episode title"] {
                app.set_player_episode_title(episode.into());
                app.set_osd_visible(true);
                settle(&app);
                let header = element(&app, "PlayerOverlay::player_header");
                let badge = ElementHandle::find_by_element_type_name(&app, "PartyPlayerBadge")
                    .next()
                    .unwrap();
                assert!(
                    badge.absolute_position().y + badge.size().height
                        <= header.absolute_position().y - 8.0,
                    "party badge obscures the caption at {width}x{height}"
                );
                assert!(badge.absolute_position().y + badge.size().height < height as f32 / 2.0);
                assert_eq!(badge.computed_opacity(), 1.0);
                app.set_osd_visible(false);
                settle(&app);
                assert_eq!(badge.computed_opacity(), 0.0);
            }
        }
    }
}

#[test]
fn text_titles_fit_and_screenshot_is_guarded() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "text_titles_fit_and_screenshot_is_guarded"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_player_episode_title("S1E1: A long episode title".into());
    app.set_player_show_title("The show's title".into());
    app.set_player_title("Removed bottom title".into());
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
            let scrim = element(&app, "PlayerOverlay::osd_scrim");
            assert_eq!(scrim.absolute_position(), LogicalPosition::new(0.0, 0.0));
            assert!((scrim.size().width - width as f32).abs() < 0.5);
            assert!((scrim.size().height - height as f32).abs() < 0.5);
            app.set_playback_started(false);
            app.set_osd_visible(false);
            app.set_player_status("Connecting…".into());
            settle(&app);
            let loading_opacity = element(&app, "PlayerOverlay::osd_scrim").computed_opacity();
            assert!(
                (loading_opacity - 0.35).abs() < 0.001,
                "loading artwork uses the shared dim layer"
            );
            app.set_playback_started(true);
            app.set_is_paused(true);
            app.set_osd_visible(true);
            settle(&app);
            assert_eq!(
                element(&app, "PlayerOverlay::osd_scrim").computed_opacity(),
                0.12
            );
            app.set_is_paused(false);
            app.set_osd_visible(false);
            settle(&app);
            assert_eq!(
                element(&app, "PlayerOverlay::osd_scrim").computed_opacity(),
                0.0
            );
            app.set_playback_started(false);
            app.set_osd_visible(true);
            app.set_player_status("".into());
            settle(&app);
            let heading = element(&app, "PlayerOverlay::episode_heading");
            let camera = element(&app, "PlayerOverlay::screenshot_button");
            let show_title = element(&app, "PlayerOverlay::show_heading");
            let track = element(&app, "PlayerOverlay::unbuffered_track");
            assert!(heading.absolute_position().x >= 16.0);
            assert!(show_title.absolute_position().y < heading.absolute_position().y);
            assert!(
                heading.absolute_position().y + heading.size().height < track.absolute_position().y
            );
            assert!(show_title.size().height > 0.0 && show_title.size().height < 42.0);
            assert!(camera.absolute_position().y + camera.size().height <= height as f32);
            assert!(
                show_title.absolute_position().x + show_title.size().width <= width as f32 - 15.5
            );
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Removed bottom title")
                    .next()
                    .is_none()
            );
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "TitleArtwork").count(),
                0,
                "the player uses text regardless of title artwork availability"
            );
        }
    }

    // Feedback stays at the viewport's safe top, independent of header size
    // and screenshot notices. Exercise asymmetric landscape cutout insets.
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_is_android(true);
    let brightness_steps = Rc::new(Cell::new(0));
    let recorded = brightness_steps.clone();
    let weak = app.as_weak();
    app.on_android_brightness_step(move |_| {
        recorded.set(recorded.get() + 1);
        weak.upgrade().unwrap().set_player_brightness(0.6);
    });
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
        let bridge = app.global::<nova::PlayerOsdBridge>();
        bridge.set_safe_top(20.0);
        bridge.set_safe_left(36.0);
        bridge.set_safe_right(4.0);
        for title in ["The show's title", ""] {
            app.set_player_show_title(title.into());
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
            let position = LogicalPosition::new(width as f32 / 2.0, height as f32 / 3.0);
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
            assert!((feedback.absolute_position().y - 36.0).abs() < 0.5);
            assert!(
                (feedback.absolute_position().x + feedback.size().width / 2.0
                    - (width as f32 / 2.0 + 16.0))
                    .abs()
                    < 0.5
            );
            assert!(
                notice.absolute_position().y
                    >= feedback.absolute_position().y + feedback.size().height + 8.0,
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
            let before = brightness_steps.get();
            let start = LogicalPosition::new(width as f32 / 4.0, height as f32 / 3.0);
            let end = LogicalPosition::new(start.x, start.y + 48.0);
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                    position: start,
                    button: slint::platform::PointerEventButton::Left,
                });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerMoved { position: end });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position: end,
                    button: slint::platform::PointerEventButton::Left,
                });
            settle(&app);
            assert_eq!(brightness_steps.get(), before + 1);
            let brightness = element(&app, "PlayerOverlay::flash_pill");
            assert!((brightness.absolute_position().y - 36.0).abs() < 0.5);
            assert!(
                ElementHandle::find_by_accessible_label(&app, "60%")
                    .next()
                    .is_some()
            );
        }
    }
    app.global::<nova::PlayerOsdBridge>().set_safe_top(0.0);
    app.global::<nova::PlayerOsdBridge>().set_safe_left(0.0);
    app.global::<nova::PlayerOsdBridge>().set_safe_right(0.0);
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

#[test]
fn gesture_titles_fade_without_unmounting_or_revealing_hidden_controls() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "gesture_titles_fade_without_unmounting_or_revealing_hidden_controls"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_is_android(true);
    app.set_player_episode_title("S3E2: The Fire-Rat Cloak".into());
    app.set_player_show_title("The Apothecary Diaries".into());
    app.on_android_brightness_step(|_| {});
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || weak.upgrade().unwrap().set_osd_visible(true));
    app.show().unwrap();
    for (width, height) in [(390, 844), (800, 360)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_osd_visible(true);
        settle(&app);
        let episode = element(&app, "PlayerOverlay::episode_heading");
        let title = element(&app, "PlayerOverlay::show_heading");
        let geometry = (episode.absolute_position(), title.absolute_position());
        assert_eq!(title.computed_opacity(), 1.0);
        let start = LogicalPosition::new(width as f32 / 4.0, height as f32 / 3.0);
        let end = LogicalPosition::new(start.x, start.y + 48.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: start,
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position: end });
        // Read the new target before advancing the animation clock.
        let _ = element(&app, "PlayerOverlay::show_heading").computed_opacity();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        let fading = element(&app, "PlayerOverlay::show_heading").computed_opacity();
        assert!(
            fading > 0.0 && fading < 1.0,
            "gesture captions fade out: {fading}"
        );
        assert_eq!(episode.computed_opacity(), fading);
        assert_eq!(
            (episode.absolute_position(), title.absolute_position()),
            geometry
        );
        settle(&app);
        assert_eq!(title.computed_opacity(), 0.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: end,
                button: slint::platform::PointerEventButton::Left,
            });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(800));
        slint::platform::update_timers_and_animations();
        let _ = title.computed_opacity();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        let returning = title.computed_opacity();
        assert!(
            returning > 0.0 && returning < 1.0,
            "gesture captions fade back: {returning}"
        );
        settle(&app);
        assert_eq!(title.computed_opacity(), 1.0);
        assert_eq!(
            (episode.absolute_position(), title.absolute_position()),
            geometry
        );

        // Feedback ending while the OSD is hidden must not resurrect captions.
        app.set_osd_visible(false);
        settle(&app);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: start,
                button: slint::platform::PointerEventButton::Left,
            });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
        slint::platform::update_timers_and_animations();
        settle(&app);
        assert_eq!(title.computed_opacity(), 0.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: start,
                button: slint::platform::PointerEventButton::Left,
            });
        settle(&app);
        assert_eq!(title.computed_opacity(), 0.0);
    }
}

#[test]
#[ignore = "requires a display; SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn render_loading_dimmer_keeps_status_bright() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "render_loading_dimmer_keeps_status_bright"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(false);
    app.set_osd_visible(false);
    app.set_player_status("Loading fixture".into());
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(2, 2);
    pixels
        .make_mut_slice()
        .fill(slint::Rgba8Pixel::new(120, 180, 240, 255));
    app.set_player_poster(slint::Image::from_rgba8(pixels));
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    app.show().unwrap();
    let weak = app.as_weak();
    let phase = Rc::new(Cell::new(0));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let app = weak.upgrade().unwrap();
            let snapshot = app.window().take_snapshot().unwrap();
            let width = snapshot.width() as usize;
            let height = snapshot.height() as usize;
            let pixel = snapshot.as_slice()[height * 2 / 5 * width + width / 20];
            assert!(
                pixel.r < 100 && pixel.g < 150 && pixel.b < 200,
                "loading artwork must be dimmed: {pixel:?}"
            );
            let bright_status_pixels = (height * 2 / 5..height * 3 / 5)
                .flat_map(|y| (width / 4..width * 3 / 4).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let pixel = snapshot.as_slice()[y * width + x];
                    pixel.r > 240 && pixel.g > 240 && pixel.b > 240
                })
                .count();
            assert!(
                bright_status_pixels > 10,
                "the status must paint above the dim layer"
            );
            if let Some(dir) = std::env::var_os("NOVA_PLAYER_LOADING_PREVIEW_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    dir.join(if phase.get() == 0 {
                        "loading-standard.png"
                    } else {
                        "loading-black.png"
                    }),
                    snapshot.as_bytes(),
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            if phase.get() == 0 {
                phase.set(1);
                nova_ui::apply_theme(&app, true);
            } else {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
}

#[test]
fn responsive_footer_keeps_metadata_and_utilities_clear_of_transport_and_safe_areas() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::responsive_footer_keeps_metadata_and_utilities_clear_of_transport_and_safe_areas"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    app.set_player_show_title("The Apothecary Diaries".into());
    app.set_player_episode_title("S1E3: The Unsettling Matter of the Spirit".into());
    app.set_player_media_summary("2023 · Animation · Mystery".into());
    app.set_player_badges(
        Rc::new(slint::VecModel::from(vec![
            "4K".into(),
            "HEVC".into(),
            "HDR".into(),
            "English 5.1".into(),
        ]))
        .into(),
    );
    app.window().show().unwrap();
    for (width, height, android) in [
        (320, 568, true),
        (640, 360, true),
        (800, 360, true),
        (1280, 720, true),
        (320, 568, false),
        (1280, 720, false),
    ] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(android);
        let bridge = app.global::<nova::PlayerOsdBridge>();
        bridge.set_safe_left(30.0);
        bridge.set_safe_right(8.0);
        bridge.set_safe_bottom(12.0);
        settle(&app);
        let header = element(&app, "PlayerOverlay::player_header");
        let track = element(&app, "PlayerOverlay::unbuffered_track");
        let camera = element(&app, "PlayerOverlay::screenshot_button");
        let gear = element(&app, "PlayerOverlay::settings_gear");
        assert_eq!(
            ElementHandle::find_by_element_id(&app, "PlayerOverlay::fullscreen_button").count(),
            usize::from(!android),
            "fullscreen is a desktop control"
        );
        assert!(header.absolute_position().y > height as f32 / 3.0);
        assert!(header.absolute_position().y + header.size().height < track.absolute_position().y);
        assert!((header.absolute_position().x - track.absolute_position().x).abs() < 0.5);
        for control in [
            &camera,
            &gear,
            &element(&app, "PlayerOverlay::speed_button"),
        ] {
            let p = control.absolute_position();
            let size = control.size();
            assert!(
                p.x >= 30.0 && p.x + size.width <= width as f32 - 8.0,
                "control outside safe area at {width}x{height}: {p:?}, {size:?}"
            );
            assert!(p.y + size.height <= height as f32 - 12.0);
        }
        if android {
            let center = element(&app, "PlayerOverlay::center_transport");
            assert!(
                center.absolute_position().y + center.size().height <= header.absolute_position().y
            );
            if width >= 700 {
                assert!(
                    camera.absolute_position().y + camera.size().height
                        < track.absolute_position().y
                );
            } else {
                assert!(
                    camera.absolute_position().y > track.absolute_position().y,
                    "portrait utility placement at {width}x{height}: camera={:?}, track={:?}",
                    camera.absolute_position(),
                    track.absolute_position()
                );
            }
        } else {
            let play = element(&app, "PlayerOverlay::desktop_pause");
            assert!(play.absolute_position().x < camera.absolute_position().x);
            assert!(play.absolute_position().y > track.absolute_position().y);
        }
        click(&app, &element(&app, "PlayerOverlay::speed_button"));
        assert!(element(&app, "SettingsMenu::settings_header").computed_opacity() > 0.9);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
        settle(&app);
    }
}

#[test]
#[ignore = "requires a display; SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn render_responsive_footer_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_responsive_footer_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    app.set_player_show_title("Avatar: The Way of Water".into());
    app.set_player_media_summary("2022 · Action".into());
    app.set_player_badges(
        Rc::new(slint::VecModel::from(vec![
            "4K".into(),
            "HEVC".into(),
            "HDR".into(),
            "English 5.1".into(),
        ]))
        .into(),
    );
    app.set_duration(11520.0);
    app.set_position(5058.0);
    app.set_buffered_until(6700.0);
    app.on_format_time(|seconds| nova::player::format_time(seconds as f64).into());
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_is_android(true);
    app.show().unwrap();
    let weak = app.as_weak();
    let stage = Rc::new(Cell::new(0));
    let warmup = Rc::new(Cell::new(2));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let app = weak.upgrade().unwrap();
            let snapshot = app.window().take_snapshot().unwrap();
            // The native backend publishes its scale factor after showing the
            // window; let the embedded factory consume that geometry before capture.
            if warmup.get() > 0 {
                warmup.set(warmup.get() - 1);
                app.window().request_redraw();
                return;
            }
            if let Some(dir) = std::env::var_os("NOVA_PLAYER_PREVIEW_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    dir.join(if stage.get() == 0 {
                        "android.png"
                    } else {
                        "desktop.png"
                    }),
                    snapshot.as_bytes(),
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            if stage.get() == 0 {
                stage.set(1);
                app.set_is_android(false);
                warmup.set(2);
                app.window().request_redraw();
            } else {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
}

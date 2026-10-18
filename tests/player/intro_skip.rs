//! Timing prompts stay usable above hidden controls and clear subtitles.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..5 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

#[test]
fn intro_prompt_fits_routes_taps_and_keeps_subtitles_clear() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::intro_prompt_fits_routes_taps_and_keeps_subtitles_clear"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_position(45.0);
    app.set_duration(1400.0);
    let skipped = Rc::new(Cell::new(0));
    let recorded = skipped.clone();
    app.on_skip_segment(move || recorded.set(recorded.get() + 1));
    app.on_toggle_pause(|| panic!("a Skip tap must not toggle the video backdrop"));
    let clearance = Rc::new(Cell::new((0.0_f32, 0.0_f32)));
    let recorded = clearance.clone();
    app.on_subtitle_clearance_changed(move |fraction, opacity| recorded.set((fraction, opacity)));
    app.window().show().unwrap();
    for (width, height) in [(320, 568), (800, 360), (1280, 720)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for language in ["en", "hr"] {
            slint::select_bundled_translation(language).unwrap();
            for kind in [1, 2] {
                for controls in [false, true] {
                    app.set_osd_visible(controls);
                    app.set_skip_segment_kind(kind);
                    settle(&app);
                    let button =
                        ElementHandle::find_by_element_id(&app, "PlayerOverlay::skip_button")
                            .next()
                            .expect("active timing prompt");
                    let position = button.absolute_position();
                    let size = button.size();
                    assert!(size.width >= 44.0 && size.height >= 44.0);
                    assert!(position.x >= 0.0 && position.y >= 0.0);
                    assert!(position.x + size.width <= width as f32 + 1.0);
                    assert!(position.y + size.height <= height as f32 + 1.0);
                    assert!((width as f32 - position.x - size.width - 16.0).abs() <= 1.0);
                    let (fraction, opacity) = clearance.get();
                    assert_eq!(opacity, 1.0, "prompt survives hidden/fading OSD");
                    assert!(fraction * height as f32 >= height as f32 - position.y + 6.0);
                    let before = skipped.get();
                    button.mock_single_click(slint::platform::PointerEventButton::Left);
                    assert_eq!(skipped.get(), before + 1);
                    // Closing/hiding a prompt restores normal subtitle clearance.
                    app.set_skip_segment_kind(0);
                    app.set_osd_visible(false);
                    settle(&app);
                    assert!(
                        ElementHandle::find_by_element_id(&app, "PlayerOverlay::skip_button")
                            .next()
                            .is_none()
                    );
                    assert_eq!(clearance.get().1, 0.0);
                }
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn show_intro_modes_and_reset_are_available_in_player_settings() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::show_intro_modes_and_reset_are_available_in_player_settings"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_osd_visible(true);
    app.set_duration(1444.0);
    app.set_intro_skip_mode_index(1);
    app.window().set_size(slint::PhysicalSize::new(800, 600));
    let picked = Rc::new(Cell::new(-2));
    let recorded = picked.clone();
    let weak = app.as_weak();
    app.on_intro_skip_mode_changed(move |mode| {
        recorded.set(mode);
        weak.upgrade().unwrap().set_intro_skip_override_index(mode);
    });
    let reset = Rc::new(Cell::new(-1));
    let recorded = reset.clone();
    let weak = app.as_weak();
    app.on_restore_playback_default(move |control| {
        recorded.set(control);
        weak.upgrade().unwrap().set_intro_skip_override_index(-1);
    });
    app.window().show().unwrap();
    settle(&app);
    ElementHandle::find_by_element_id(&app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    let click = |label: &str| {
        ElementHandle::find_by_accessible_label(&app, label)
            .find(|item| {
                item.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
            })
            .unwrap_or_else(|| panic!("missing setting {label}"))
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
    };
    click("Intro skipping");
    for (mode, label) in [(0, "Off"), (1, "Show skip prompt"), (2, "Auto skip")] {
        click(label);
        assert_eq!(picked.get(), mode);
        assert_eq!(app.get_intro_skip_override_index(), mode);
        assert_eq!(
            app.get_intro_skip_mode_index(),
            1,
            "title choices leave the global mode intact"
        );
    }
    click("Use global default");
    assert_eq!(reset.get(), 4);
    assert_eq!(app.get_intro_skip_override_index(), -1);
    // The submenu stays open, and all four rows are keyboard reachable.
    for key in [
        slint::platform::Key::DownArrow,
        slint::platform::Key::Return,
    ] {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    }
    assert_eq!(picked.get(), 0);
}

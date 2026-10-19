//! Destroying a slider must release the shared lease used by Android menu scrolling.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    slint::platform::update_timers_and_animations();
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle();
}

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"))
}

fn open_settings(app: &nova::AppWindow) {
    app.set_osd_visible(true);
    settle();
    ElementHandle::find_by_element_id(app, "PlayerOverlay::settings_gear")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
}

fn advance_menu(app: &nova::AppWindow, millis: u64) {
    for _ in 0..millis.div_ceil(10) {
        // Bindings are lazy in the headless backend. Observe each frame before
        // advancing the clock so a resize and reveal start at the same time.
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (
                item.size(),
                item.absolute_position(),
                item.computed_opacity(),
            );
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
        slint::platform::update_timers_and_animations();
    }
}

fn immediate_key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

#[test]
fn settings_pages_slide_fade_and_resize_in_both_directions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::settings_pages_slide_fade_and_resize_in_both_directions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, height) in [(360, 720), (900, 360), (1280, 800)] {
        let app = nova::AppWindow::new().unwrap();
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(true);
        app.set_touch_menus(true);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_osd_visible(true);
        app.set_audio_tracks(
            Rc::new(slint::VecModel::from(
                (0..30)
                    .map(|index| nova::TrackRow {
                        index,
                        title: format!("Track {index:02}").into(),
                        lang: "en".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_sub_tracks(app.get_audio_tracks());
        let resets = Rc::new(Cell::new(0));
        let recorded = resets.clone();
        app.on_restore_playback_default(move |_| recorded.set(recorded.get() + 1));
        app.show().unwrap();
        advance_menu(&app, 300);
        // The conditional OSD host initializes its bridge from the window's
        // native safe area when mounted. Override it only after that mount.
        app.global::<nova::PlayerOsdBridge>().set_safe_top(20.0);
        open_settings(&app);
        advance_menu(&app, 300);
        let card = ElementHandle::find_by_element_id(&app, "SettingsMenu::card")
            .next()
            .expect("settings card");
        let root_height = card.size().height;
        let root_width = card.size().width;
        let mut smallest_height = root_height;
        let mut largest_height = root_height;

        // Exercise every category, including the very different natural sizes
        // of a speed slider, decoder choices, track list and subtitle controls.
        for (row, first_label) in [
            (3, "Use global default"),
            (1, "Use global default"),
            (4, "Use global defaults"),
            (2, "HW+"),
            (0, "Use global default"),
            (5, "Use global default"),
        ] {
            for _ in 0..row {
                key(&app, slint::platform::Key::DownArrow);
            }
            immediate_key(&app, slint::platform::Key::Return);
            let incoming = button(&app, first_label);
            advance_menu(&app, 50);
            let entering_x = incoming.absolute_position().x;
            let entering_height = card.size().height;
            let entering_opacity = incoming.computed_opacity();
            assert!(
                entering_opacity > 0.0 && entering_opacity < 1.0,
                "incoming page fades at {width}x{height}, row {row}: opacity={entering_opacity}, x={entering_x}, height={entering_height}, root_height={root_height}, master={}, player={}",
                app.global::<nova::Anim>().get_enabled(),
                app.global::<nova::Anim>().get_player(),
            );
            advance_menu(&app, 250);
            let page_height = card.size().height;
            smallest_height = smallest_height.min(page_height);
            largest_height = largest_height.max(page_height);
            assert!(entering_x > incoming.absolute_position().x + 1.0);
            assert_eq!(incoming.computed_opacity(), 1.0);
            assert!((card.size().width - root_width).abs() < 0.5);
            if (page_height - root_height).abs() > 4.0 {
                assert!(entering_height > root_height.min(page_height));
                assert!(entering_height < root_height.max(page_height));
            }
            let card_y = card.absolute_position().y;
            assert_eq!(app.global::<nova::PlayerOsdBridge>().get_safe_top(), 20.0);
            assert!(
                card_y >= 35.0 && card_y + page_height <= height as f32 - 15.0,
                "settings card stays inside the safe viewport at {width}x{height}, row {row}: y={card_y}, height={page_height}, safe_top={}",
                app.global::<nova::PlayerOsdBridge>().get_safe_top(),
            );

            // Back keeps a living keyboard owner, removes the old page's
            // actions immediately and reveals the root from the other side.
            immediate_key(&app, slint::platform::Key::Escape);
            assert!(
                ElementHandle::find_by_accessible_label(&app, first_label)
                    .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
                    .is_none()
            );
            let root_row = button(&app, "Subtitles");
            advance_menu(&app, 50);
            let returning_x = root_row.absolute_position().x;
            assert!(root_row.computed_opacity() > 0.0 && root_row.computed_opacity() < 1.0);
            advance_menu(&app, 250);
            assert!(returning_x < root_row.absolute_position().x - 1.0);
            assert!((card.size().height - root_height).abs() < 0.5);
            assert_eq!(
                resets.get(),
                0,
                "navigation must not edit title preferences"
            );
        }
        // A short landscape viewport clamps all long pages, but on larger
        // screens their differing natural heights must remain visible.
        if height > 400 {
            assert!(largest_height - smallest_height > 80.0);
        }

        // Reverse navigation before either reveal completes. The previous
        // page cannot retain a slider lease or eat the next key activation.
        for _ in 0..3 {
            immediate_key(&app, slint::platform::Key::DownArrow);
        }
        immediate_key(&app, slint::platform::Key::Return);
        advance_menu(&app, 30);
        immediate_key(&app, slint::platform::Key::Escape);
        immediate_key(&app, slint::platform::Key::Return);
        advance_menu(&app, 300);
        assert_eq!(button(&app, "Off").computed_opacity(), 1.0);
        assert!(!app.global::<nova::SliderGesture>().get_active());
        key(&app, slint::platform::Key::Escape);

        // Turning motion off during a resize must finish it immediately, too.
        for _ in 0..3 {
            immediate_key(&app, slint::platform::Key::DownArrow);
        }
        immediate_key(&app, slint::platform::Key::Return);
        advance_menu(&app, 40);
        app.global::<nova::Anim>().set_player(false);
        let stopped_size = card.size();
        assert_eq!(button(&app, "Use global default").computed_opacity(), 1.0);
        advance_menu(&app, 10);
        app.global::<nova::Anim>().set_player(true);
        assert_eq!(button(&app, "Use global default").computed_opacity(), 1.0);
        assert_eq!(card.size(), stopped_size);
        advance_menu(&app, 10);
        assert_eq!(button(&app, "Use global default").computed_opacity(), 1.0);
        assert_eq!(card.size(), stopped_size);
        app.global::<nova::Anim>().set_player(false);
        advance_menu(&app, 300);
        assert_eq!(card.size(), stopped_size);
        immediate_key(&app, slint::platform::Key::Escape);

        for master_disabled in [false, true] {
            app.global::<nova::Anim>().set_player(master_disabled);
            app.global::<nova::Anim>().set_enabled(!master_disabled);
            immediate_key(&app, slint::platform::Key::Return);
            let incoming = button(&app, "Use global default");
            let position = incoming.absolute_position();
            let size = card.size();
            assert_eq!(incoming.computed_opacity(), 1.0);
            advance_menu(&app, 300);
            assert_eq!(incoming.absolute_position(), position);
            assert_eq!(card.size(), size);
            immediate_key(&app, slint::platform::Key::Escape);
            assert_eq!(button(&app, "Subtitles").computed_opacity(), 1.0);
        }
    }
}

#[test]
fn android_settings_scrolls_after_adjusting_closing_and_reopening() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::android_settings_scrolls_after_adjusting_closing_and_reopening"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 720));
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_duration(600.0);
    app.set_osd_visible(true);
    app.set_audio_tracks(
        Rc::new(slint::VecModel::from(
            (0..30)
                .map(|index| nova::TrackRow {
                    index,
                    title: format!("Track {index:02}").into(),
                    lang: "en".into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_sub_tracks(app.get_audio_tracks());
    let edits = Rc::new(Cell::new(0));
    let recorded = edits.clone();
    app.on_playback_speed_changed(move |_| recorded.set(recorded.get() + 1));
    app.show().unwrap();
    settle();

    for interrupt_press in [false, true] {
        edits.set(0);
        open_settings(&app);
        button(&app, "Playback speed").mock_single_click(slint::platform::PointerEventButton::Left);
        settle();
        let slider = ElementHandle::find_by_element_id(&app, "SpeedControl::track_touch")
            .next()
            .expect("speed slider is visible");
        let p = slider.absolute_position();
        let size = slider.size();
        let start = LogicalPosition::new(p.x + size.width * 0.6, p.y + size.height / 2.0);
        let end = LogicalPosition::new(p.x + size.width * 0.8, start.y);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: start,
            button: slint::platform::PointerEventButton::Left,
        });
        settle();
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position: end });
        settle();
        assert!(
            app.global::<nova::SliderGesture>().get_active(),
            "slider owns its adjustment gesture"
        );
        if !interrupt_press {
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position: end,
                button: slint::platform::PointerEventButton::Left,
            });
            settle();
        }
        assert!(
            edits.get() > 0,
            "the slider adjustment reached the backend callback"
        );
        // System navigation can remove the slider before Android delivers its
        // final up/cancel. Exercise that interruption as well as a normal edit.
        key(&app, slint::platform::Key::Escape);
        key(&app, slint::platform::Key::Escape);
        if interrupt_press {
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position: end,
                button: slint::platform::PointerEventButton::Left,
            });
        }
        assert!(!app.global::<nova::SliderGesture>().get_active());
        assert!(!app.global::<nova::SliderGesture>().get_hovered());
        assert!(
            ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
                .next()
                .is_none()
        );

        for category in ["Audio", "Subtitles"] {
            open_settings(&app);
            button(&app, category).mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            let first = button(&app, "en — Track 00");
            let before = first.absolute_position().y;
            let drag = button(&app, "en — Track 04");
            let drag_pos = drag.absolute_position();
            let drag_size = drag.size();
            drag.mock_drag(
                LogicalPosition::new(drag_pos.x + drag_size.width / 2.0, drag_pos.y - 180.0),
                slint::platform::PointerEventButton::Left,
            );
            settle();
            assert!(
                first.absolute_position().y < before - 40.0,
                "reopened {category} list must scroll after adjustment (interrupted={interrupt_press})"
            );
            key(&app, slint::platform::Key::Escape);
            key(&app, slint::platform::Key::Escape);
        }
    }
}

#[test]
fn subtitle_style_vertical_touch_drags_scroll_without_editing_sliders() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::subtitle_style_vertical_touch_drags_scroll_without_editing_sliders"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    use i_slint_core::{input::TouchPhase, platform::InternalEvent};
    use std::cell::RefCell;

    fn touch(app: &nova::AppWindow, position: LogicalPosition, phase: TouchPhase) {
        app.window()
            .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                id: 1,
                position: i_slint_core::lengths::logical_point_from_api(position),
                phase,
            }));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        slint::platform::update_timers_and_animations();
    }

    fn open_style(app: &nova::AppWindow) {
        open_settings(app);
        // Reveal the last category even in a short landscape menu.
        for _ in 0..4 {
            key(app, slint::platform::Key::DownArrow);
        }
        key(app, slint::platform::Key::Return);
    }

    fn timing_slider(app: &nova::AppWindow) -> ElementHandle {
        ElementHandle::find_by_accessible_label(app, "Subtitle timing (seconds)")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Slider))
            .expect("subtitle timing slider")
    }

    for (width, height) in [(360, 720), (900, 360)] {
        let app = nova::AppWindow::new().unwrap();
        app.global::<nova::Anim>().set_enabled(false);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_is_android(true);
        app.set_touch_menus(true);
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_duration(600.0);
        app.set_osd_visible(true);
        let edits = Rc::new(RefCell::new(Vec::new()));
        let recorded = edits.clone();
        app.on_subtitle_style_changed(move |index, value| {
            recorded.borrow_mut().push((index, value));
        });
        app.show().unwrap();
        settle();

        for cancelled in [false, true] {
            open_style(&app);
            let slider = timing_slider(&app);
            let pos = slider.absolute_position();
            let size = slider.size();
            let start = LogicalPosition::new(pos.x + size.width * 0.2, pos.y + size.height / 2.0);
            edits.borrow_mut().clear();
            touch(&app, start, TouchPhase::Started);
            if !cancelled {
                settle();
            }
            for step in 1..=8 {
                touch(
                    &app,
                    LogicalPosition::new(
                        start.x + size.width * 0.6 * step as f32 / 8.0,
                        start.y - step as f32,
                    ),
                    TouchPhase::Moved,
                );
            }
            assert!(
                app.global::<nova::SliderGesture>().get_active(),
                "horizontal drag owns its gesture at {width}x{height}: {pos:?}, {size:?}"
            );
            assert!(
                (slider.absolute_position().y - pos.y).abs() < 1.0,
                "horizontal slider drag must not move the panel"
            );
            touch(
                &app,
                LogicalPosition::new(start.x + size.width * 0.6, start.y - 8.0),
                if cancelled {
                    TouchPhase::Cancelled
                } else {
                    TouchPhase::Ended
                },
            );
            settle();
            assert!(
                !edits.borrow().is_empty(),
                "horizontal dragging must still adjust subtitles"
            );
            assert!(!app.global::<nova::SliderGesture>().get_active());
            assert!(!app.global::<nova::SliderGesture>().get_hovered());
            key(&app, slint::platform::Key::Escape);
            key(&app, slint::platform::Key::Escape);

            // Reopening after an edit must accept a new, independent vertical
            // gesture. Native touch exercises the delayed press/cancel path.
            open_style(&app);
            let slider = timing_slider(&app);
            let pos = slider.absolute_position();
            let size = slider.size();
            let start = LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
            edits.borrow_mut().clear();
            touch(&app, start, TouchPhase::Started);
            // Exercise a delivered press as well as quick flicks: holding still
            // must not permanently choose a horizontal slider gesture.
            if !cancelled {
                settle();
            }
            for step in 1..=8 {
                touch(
                    &app,
                    LogicalPosition::new(start.x, start.y - step as f32 * 12.0),
                    TouchPhase::Moved,
                );
            }
            touch(
                &app,
                LogicalPosition::new(start.x, start.y - 96.0),
                if cancelled {
                    TouchPhase::Cancelled
                } else {
                    TouchPhase::Ended
                },
            );
            settle();
            assert!(
                slider.absolute_position().y < pos.y - 40.0,
                "vertical touch on a subtitle slider must scroll ({width}x{height}, cancelled={cancelled})"
            );
            assert!(
                edits.borrow().is_empty(),
                "vertical scrolling must not edit subtitle values"
            );
            assert!(!app.global::<nova::SliderGesture>().get_active());
            assert!(!app.global::<nova::SliderGesture>().get_hovered());
            key(&app, slint::platform::Key::Escape);
            key(&app, slint::platform::Key::Escape);
        }
    }
}

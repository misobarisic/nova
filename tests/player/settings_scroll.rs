//! Destroying a slider must release the shared lease used by Android menu scrolling.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
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
                "horizontal drag owns its gesture"
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

//! Playback preference reset rows and responsive Player settings UI (headless).

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(Duration::from_millis(ms), body);
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
}

fn has_text(app: &nova::AppWindow, expected: &str) -> bool {
    ElementHandle::find_by_element_type_name(app, "Text")
        .any(|text| text.accessible_label().as_deref() == Some(expected))
}

fn setting_scope(app: &nova::AppWindow, key: &str) -> Option<ElementHandle> {
    let id = format!("setting-scope:{key}");
    ElementQuery::from_root(app)
        .match_predicate(move |element| element.accessible_id().as_deref() == Some(id.as_str()))
        .find_all()
        .into_iter()
        .next()
}

fn title_badge_do_not_overlap(app: &nova::AppWindow, title: &str, badge: &ElementHandle) -> bool {
    let badge_position = badge.absolute_position();
    let badge_size = badge.size();
    ElementHandle::find_by_accessible_label(app, title).any(|title| {
        let position = title.absolute_position();
        let size = title.size();
        position.x + size.width <= badge_position.x - 7.5
            || position.y + size.height <= badge_position.y - 2.5
            || badge_position.y + badge_size.height <= position.y - 2.5
    })
}

fn max_visible_right_edge(app: &nova::AppWindow) -> f32 {
    let width = app.window().size().width as f32;
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|element| {
            let position = element.absolute_position();
            let size = element.size();
            (position.x >= 0.0 && position.x < width && size.width > 0.0)
                .then_some(position.x + size.width)
        })
        .fold(0.0, f32::max)
}

#[test]
fn playback_defaults_reset_and_player_settings_fit() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(800, 600));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_audio_preference_saved(true);
    app.set_subtitle_preference_saved(true);
    app.set_speed_preference_saved(true);
    app.set_audio_tracks(
        Rc::new(VecModel::from(vec![nova::TrackRow {
            index: 7,
            lang: "en".into(),
            title: "Main".into(),
            codec: "AAC".into(),
            selected: false,
        }]))
        .into(),
    );
    app.set_sub_tracks(
        Rc::new(VecModel::from(vec![nova::TrackRow {
            index: 42,
            lang: "en".into(),
            title: "English".into(),
            codec: "SRT".into(),
            selected: false,
        }]))
        .into(),
    );
    let scope_rows = Rc::new(VecModel::from(vec![
        nova::SettingSyncInfo {
            key: "default_audio_language".into(),
            status: 1,
            can_override: true,
            local_value: "English".into(),
            shared_value: "English".into(),
            has_shared: true,
            shared_supported: true,
        },
        nova::SettingSyncInfo {
            key: "default_subtitle_language".into(),
            status: 2,
            can_override: true,
            local_value: "Croatian".into(),
            shared_value: "English".into(),
            has_shared: true,
            shared_supported: true,
        },
    ]));
    app.global::<nova::SettingsSync>()
        .set_rows(scope_rows.clone().into());
    app.set_sync_enabled(true);
    let indexed_scope_rows = scope_rows.clone();
    app.global::<nova::SettingsSync>().on_lookup(move |key| {
        indexed_scope_rows
            .iter()
            .position(|row| row.key == key)
            .map(|index| index as i32)
            .unwrap_or(-1)
    });

    let restored = Rc::new(RefCell::new(Vec::<i32>::new()));
    let audio_picks = Rc::new(RefCell::new(Vec::<i32>::new()));
    let subtitle_picks = Rc::new(RefCell::new(Vec::<i32>::new()));
    let steps = Rc::new(RefCell::new(Vec::<i32>::new()));
    {
        let restored = restored.clone();
        let app = app.as_weak();
        app.upgrade()
            .unwrap()
            .on_restore_playback_default(move |control| {
                restored.borrow_mut().push(control);
                let app = app.upgrade().unwrap();
                match control {
                    0 => app.set_audio_preference_saved(false),
                    1 => app.set_subtitle_preference_saved(false),
                    2 => app.set_speed_preference_saved(false),
                    _ => {}
                }
            });
    }
    {
        let steps = steps.clone();
        app.on_playback_speed_stepped(move |direction| steps.borrow_mut().push(direction));
    }
    {
        let picks = audio_picks.clone();
        app.on_pick_audio_track(move |index| picks.borrow_mut().push(index));
    }
    {
        let picks = subtitle_picks.clone();
        app.on_pick_sub_track(move |index| picks.borrow_mut().push(index));
    }

    let app_weak = app.as_weak();
    after(350, move || {
        slint::spawn_local(async move {
            let app = app_weak.upgrade().unwrap();
            // Desktop player controls are indexed close, pause, volume, seek
            // back/forward, subtitles, audio, gear. Open the gear by keyboard.
            for _ in 0..6 {
                key(&app, slint::platform::Key::RightArrow);
            }
            key(&app, slint::platform::Key::Return);
            settings_support::settle().await;
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "the gear key opens the player settings panel"
            );

            // Subtitle reset is row 0, followed by Off and track rows.
            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring subtitles leaves the panel open"
            );
            assert!(has_text(&app, "Using global default"));
            // The Off row is now 1 and the first subtitle track is 2.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(subtitle_picks.borrow().as_slice(), [42]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                0,
                "choosing a subtitle track closes the panel"
            );

            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert!(has_text(&app, "Saved for this title"));
            // Audio reset is row 0, tracks begin at row 1.
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1, 0]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring audio leaves the panel open"
            );
            // Audio tracks begin at row 1 after the reset item.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(audio_picks.borrow().as_slice(), [7]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                0,
                "choosing an audio track closes the panel"
            );

            key(&app, slint::platform::Key::Return);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::Return);
            assert!(has_text(&app, "Saved for this title"));
            // Speed reset is row 0 and its slider is row 1. Up/Down move
            // focus; Left/Right step only while the slider has focus.
            key(&app, slint::platform::Key::DownArrow);
            key(&app, slint::platform::Key::RightArrow);
            key(&app, slint::platform::Key::LeftArrow);
            assert_eq!(steps.borrow().as_slice(), [1, -1]);
            key(&app, slint::platform::Key::UpArrow);
            key(&app, slint::platform::Key::Return);
            assert_eq!(restored.borrow().as_slice(), [1, 0, 2]);
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "SettingsMenu").count(),
                1,
                "restoring speed leaves the panel open"
            );

            // The reset entries share MenuRow styling and touch sizing with
            // the existing player menu; the 44px minimum touch target holds.
            assert!(
                ElementHandle::find_by_element_type_name(&app, "MenuRow")
                    .any(|row| row.size().height >= 44.0)
            );

            app.set_player_open(false);
            app.set_show_settings(true);
            app.set_settings_selected_id(5);
            app.set_settings_detail_open(true);
            slint::select_bundled_translation("en").unwrap();

            for (audio_status, subtitle_status) in [(1, 2), (2, 1)] {
                let mut audio_scope = scope_rows.row_data(0).unwrap();
                audio_scope.status = audio_status;
                scope_rows.set_row_data(0, audio_scope);
                let mut subtitle_scope = scope_rows.row_data(1).unwrap();
                subtitle_scope.status = subtitle_status;
                scope_rows.set_row_data(1, subtitle_scope);
                for (width, height) in [(360, 800), (800, 360)] {
                    app.window()
                        .set_size(slint::PhysicalSize::new(width, height));
                    let mut found_audio = false;
                    let mut found_subtitles = false;
                    let mut found_speed = false;
                    let mut found_audio_scope = false;
                    let mut found_subtitle_scope = false;
                    let mut checked_audio_caption = false;
                    let mut checked_subtitle_caption = false;
                    for scroll in [0.0, -120.0, -240.0, -360.0, -480.0, -600.0, -720.0] {
                        app.set_settings_scroll_y(scroll);
                        settings_support::settle().await;
                        found_audio |= has_text(&app, "Default audio language");
                        found_subtitles |= has_text(&app, "Default subtitle language");
                        found_speed |= ElementHandle::find_by_element_type_name(&app, "SpeedControl")
                            .next()
                            .is_some();
                        let expected_audio = if audio_status == 1 {
                            "Synced"
                        } else {
                            "This device (overridden)"
                        };
                        if let Some(badge) = setting_scope(&app, "default_audio_language") {
                            found_audio_scope = true;
                            assert_eq!(badge.accessible_label().as_deref(), Some(expected_audio));
                            // Element queries omit clipped titles: a stacked badge
                            // can remain visible after its title scrolls offscreen.
                            if has_text(&app, "Default audio language") {
                                checked_audio_caption = true;
                                assert!(title_badge_do_not_overlap(
                                    &app,
                                    "Default audio language",
                                    &badge
                                ), "audio title/badge overlap at {width}x{height}, status {audio_status}");
                            }
                        }
                        let expected_subtitle = if subtitle_status == 1 {
                            "Synced"
                        } else {
                            "This device (overridden)"
                        };
                        if let Some(badge) = setting_scope(&app, "default_subtitle_language") {
                            found_subtitle_scope = true;
                            assert_eq!(badge.accessible_label().as_deref(), Some(expected_subtitle));
                            if has_text(&app, "Default subtitle language") {
                                checked_subtitle_caption = true;
                                assert!(title_badge_do_not_overlap(
                                    &app,
                                    "Default subtitle language",
                                    &badge
                                ), "subtitle title/badge overlap at {width}x{height}, status {subtitle_status}");
                            }
                        }
                        let edge = max_visible_right_edge(&app);
                        assert!(
                            edge <= width as f32 + 1.0,
                            "Settings → Player overflows {width}x{height} with language scopes: right edge {edge:.1}"
                        );
                    }
                    assert!(checked_audio_caption && checked_subtitle_caption,
                        "both language captions must be checked at {width}x{height}");
                    assert!(
                        found_audio,
                        "audio language control visible at {width}x{height}"
                    );
                    assert!(
                        found_subtitles,
                        "subtitle language control visible at {width}x{height}"
                    );
                    assert!(
                        found_speed,
                        "default speed control visible at {width}x{height}"
                    );
                    assert!(
                        found_audio_scope,
                        "audio language scope visible at {width}x{height}"
                    );
                    assert!(
                        found_subtitle_scope,
                        "subtitle language scope visible at {width}x{height}"
                    );
                }
            }

            slint::quit_event_loop().unwrap();
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();
}

#[path = "support/settings.rs"]
#[allow(dead_code)]
mod settings_support;

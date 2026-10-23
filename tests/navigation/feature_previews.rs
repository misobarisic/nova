//! Opt-in native previews of populated feature flows, including prior UI fixes.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::{rc::Rc, time::Duration};

fn strings(values: &[&str]) -> slint::ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
    ))
    .into()
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
}

fn click(app: &nova::AppWindow, id: &str) {
    let item = ElementHandle::find_by_element_id(app, id).next().expect(id);
    let origin = item.absolute_position();
    let size = item.size();
    let position =
        slint::LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
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
}

#[test]
fn render_feature_design_previews() {
    if !crate::isolation::run(concat!(module_path!(), "::render_feature_design_previews")) {
        return;
    }
    let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.set_animations(false);
    app.set_anim_transitions(false);

    let backup = app.global::<nova_ui_shared::BackupBridge>();
    backup.set_profiles(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
            id: "profile-one".into(),
            label: "Evening viewing — shared family profile".into(),
            checked: true,
        }]))
        .into(),
    );
    backup.set_components(
        Rc::new(VecModel::from(
            [
                "Library and categories",
                "Playback progress and viewing history",
                "Addon configuration",
                "Profile PIN and restrictions",
            ]
            .into_iter()
            .enumerate()
            .map(|(i, label)| nova_ui_shared::BackupOption {
                id: i.to_string().into(),
                label: label.into(),
                checked: i < 2,
            })
            .collect::<Vec<_>>(),
        ))
        .into(),
    );
    backup.set_mappings(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupMapping {
            id: "profile-one".into(),
            label: "Evening viewing".into(),
            target: "Create a new profile".into(),
        }]))
        .into(),
    );
    backup.set_conflicts(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
            id: "progress".into(),
            label: "Playback progress".into(),
            checked: false,
        }]))
        .into(),
    );
    backup.set_preview_rows(strings(&[
        "Evening viewing · Library: 12 new, 4 identical, 2 conflicts, 0 deletions",
        "Playback progress: 3 new, 2 conflicts; local values preserved",
    ]));

    let queue = app.global::<nova_ui_shared::QueueBridge>();
    queue.set_rows(
        Rc::new(VecModel::from(vec![
            nova_ui_shared::WatchQueueRow {
                id: "episode-one".into(),
                title: "The Observatory — a very long series title".into(),
                context: "S2 E4 · A signal from the other side of midnight".into(),
                ..Default::default()
            },
            nova_ui_shared::WatchQueueRow {
                id: "movie-two".into(),
                title: "A Long Way Home".into(),
                context: "Movie · 2026".into(),
                watched: true,
                ..Default::default()
            },
        ]))
        .into(),
    );
    queue.set_summary("2 items · 1 watched".into());
    queue.set_notice("Continue this queue on any paired Nova device.".into());
    queue.set_current_key("episode-one".into());

    let offline = app.global::<nova_ui_shared::OfflineBridge>();
    offline.set_device_enabled(true);
    offline.set_storage_summary("2.1 GiB of 5 GiB · 18 GiB free".into());
    offline.set_rows(
        Rc::new(VecModel::from(vec![nova_ui_shared::SmartOfflineRow {
            id: "show-one".into(),
            title: "The Observatory — a very long series title".into(),
            status: "2 ready · 1 waiting for unmetered Wi-Fi".into(),
            enabled: true,
            keep_count: 3,
            source_ready: true,
            cleanup: true,
            ..Default::default()
        }]))
        .into(),
    );

    let calendar = app.global::<nova_ui_shared::CalendarBridge>();
    calendar.set_reminder_labels(strings(&[
        "The Observatory — a very long series title",
        "A signal from the other side of midnight",
    ]));
    calendar.set_reminder_keys(strings(&["show-one", "show-two"]));
    calendar.set_reminder_subscribed(Rc::new(VecModel::from(vec![true, false])).into());
    calendar.set_export_selected(Rc::new(VecModel::from(vec![true, false])).into());
    calendar.set_reminder_status("Last refreshed today · Daily digest at 09:00".into());
    calendar.set_export_start("2026-10-23".into());
    calendar.set_export_end("2027-01-21".into());
    calendar.set_export_ready(true);
    calendar.set_export_preview("2 series · 12 releases · 1 undated episode omitted".into());

    let more = app.global::<nova_ui_shared::MoreBridge>();
    more.set_policy_titles(strings(&[
        "The Observatory — a very long series title",
        "A Long Way Home",
    ]));
    more.set_policy_title_checks(Rc::new(VecModel::from(vec![true, false])).into());
    more.set_policy_addons(strings(&[
        "Community source — preferred release family",
        "Local downloaded media",
    ]));
    more.set_policy_addon_checks(Rc::new(VecModel::from(vec![true, false])).into());
    more.set_policy_pin("0012".into());
    more.set_policy_restricted(true);
    let smart = app.global::<nova_ui_shared::SmartCollectionsBridge>();
    smart.set_name("Short science-fiction discoveries".into());
    smart.set_editing_id("science-fiction".into());
    smart.set_genre("Science Fiction".into());
    smart.set_max_runtime("120".into());

    app.set_episode_rows(Rc::new(VecModel::from(vec![nova::EpisodeRow {
        text:"A Friend for This Crimson Demon Girl! With a Very Long Episode Title".into(),
        details:"The team notices that their friend has not returned, while a familiar voice brings news from the other side of the island.".into(),
        ep_no:"S2 E12".into(),date:"October 23, 2026".into(),runtime:"24m".into(),progress:0.45,..Default::default()
    }])).into());
    app.set_episode_total(1);
    app.set_season_names(strings(&["Season 2"]));
    app.set_selected_title("The Observatory".into());
    app.set_stream_addons(strings(&["Community addon"]));
    app.set_streams(
        Rc::new(VecModel::from(vec![
            nova::StreamRow {
                id: "party-local-match".into(),
                title: "The.Observatory.2026.1080p.WEB-DL-GROUP".into(),
                metadata: "Community addon · 1080p · HEVC".into(),
                text: "The Observatory · 1080p".into(),
                size: "1.2 GiB".into(),
                lines: 3,
                party_recommended: true,
                ..Default::default()
            },
            nova::StreamRow {
                id: "ordinary-local-result".into(),
                title: "The Observatory · 720p".into(),
                metadata: "Community addon · 720p".into(),
                size: "640 MiB".into(),
                lines: 2,
                ..Default::default()
            },
        ]))
        .into(),
    );
    app.show().unwrap();

    let names = [
        "backup-export",
        "backup-import",
        "watch-queue",
        "smart-offline",
        "calendar-export",
        "sleep-timer",
        "profile-policy",
        "profile-pin",
        "smart-editor",
        "episodes",
        "party-recommendation",
        "backup-import-actions",
        "smart-offline-rule",
        "calendar-export-actions",
        "profile-policy-actions",
    ];
    let timer = slint::Timer::default();
    let mut tick = 0usize;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(300),
        move || {
            let scene = tick / 3;
            let phase = tick % 3;
            let frame = scene % names.len();
            let view = match frame {
                11 => 1,
                12 => 3,
                13 => 4,
                14 => 6,
                other => other,
            };
            let device = (scene / names.len()) % 4;
            let language = if scene / (names.len() * 4) == 0 {
                "en"
            } else {
                "hr"
            };
            let (width, height, tv, android) = [
                (390, 844, false, true),
                (900, 390, false, true),
                (1280, 900, false, false),
                (960, 540, true, false),
            ][device];
            if phase == 0 {
                slint::select_bundled_translation(language).unwrap();
                app.global::<nova::InterfaceStyle>().set_tv(tv);
                app.set_is_android(android);
                app.global::<nova_ui_shared::CalendarBridge>()
                    .set_android_notifications(android);
                app.window()
                    .set_size(slint::PhysicalSize::new(width, height));
                i_slint_core::window::WindowInner::from_pub(app.window())
                    .set_window_item_safe_area(if android {
                        i_slint_core::lengths::LogicalEdges::new(28.0, 12.0, 8.0, 6.0)
                    } else {
                        i_slint_core::lengths::LogicalEdges::default()
                    });
                app.global::<nova_ui_shared::BackupBridge>().set_open(false);
                app.global::<nova_ui_shared::OfflineBridge>()
                    .set_opened(false);
                app.global::<nova_ui_shared::QueueBridge>().set_open(false);
                app.global::<nova_ui_shared::MoreBridge>()
                    .set_pin_open(false);
                app.global::<nova_ui_shared::MoreBridge>()
                    .set_policy_open(false);
                app.global::<nova_ui_shared::SmartCollectionsBridge>()
                    .set_editor_open(false);
                app.set_player_open(false);
                app.set_modal_visible(false);
                app.set_stream_selector_open(false);
                app.set_detail_is_movie(false);
                app.set_more_open(view == 2);
                app.set_show_home(false);
                app.set_show_settings(view <= 1 || view == 8);
                app.set_show_calendar(view == 4);
                app.set_show_library(false);
                app.set_show_downloads(view == 3);
                match view {
                    0 | 1 => {
                        let ui = app.global::<nova_ui_shared::BackupBridge>();
                        ui.set_importing(view == 1);
                        ui.set_preview_ready(view == 1);
                        ui.set_open(true);
                    }
                    2 => app.global::<nova_ui_shared::QueueBridge>().set_open(true),
                    3 => app
                        .global::<nova_ui_shared::OfflineBridge>()
                        .set_opened(true),
                    4 => {
                        app.set_calendar_title("October 2026".into());
                    }
                    5 => {
                        app.set_player_open(true);
                        app.set_playback_started(true);
                        app.set_osd_visible(true);
                        app.set_desktop_decoder_available(true);
                        let osd = app.global::<nova_ui_shared::PlayerOsdBridge>();
                        osd.set_sleep_timer_available(true);
                        osd.set_sleep_timer_end_available(true);
                        osd.set_sleep_timer_episodes_available(true);
                        osd.set_sleep_timer_active(true);
                        osd.set_sleep_timer_duration(true);
                        osd.set_sleep_timer_label("Sleep in 29 minutes".into());
                    }
                    6 => app
                        .global::<nova_ui_shared::MoreBridge>()
                        .set_policy_open(true),
                    7 => {
                        let ui = app.global::<nova_ui_shared::MoreBridge>();
                        ui.set_pin_length(2);
                        ui.set_pin_open(true);
                    }
                    8 => {
                        app.set_settings_selected_id(1);
                        app.set_settings_detail_open(true);
                        app.global::<nova_ui_shared::SmartCollectionsBridge>()
                            .set_editor_open(true);
                    }
                    9 => {
                        app.set_modal_episodes(true);
                        app.set_detail_tab(3);
                        app.set_modal_visible(true);
                    }
                    _ => {
                        app.set_modal_visible(true);
                        app.set_detail_is_movie(true);
                        app.set_modal_episodes(false);
                        app.set_detail_tab(0);
                        app.set_stream_selector_open(true);
                    }
                }
            } else if phase == 1 {
                if view == 4 {
                    click(&app, "CalendarPage::tools_button");
                } else if view == 5 {
                    click(&app, "PlayerOverlay::settings_gear");
                    for _ in 0..7 {
                        key(&app, slint::platform::Key::DownArrow);
                    }
                    key(&app, slint::platform::Key::Return);
                }
                if frame >= 11 {
                    for _ in 0..12 {
                        app.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(
                                    width as f32 * 0.6,
                                    height as f32 * 0.65,
                                ),
                                delta_x: 0.0,
                                delta_y: -240.0,
                            },
                        );
                    }
                }
            } else {
                let pixels = app.window().take_snapshot().unwrap();
                image::save_buffer(
                    dir.join(format!("{language}-{width}x{height}-{}.png", names[frame])),
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            tick += 1;
            if tick == names.len() * 4 * 2 * 3 {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop().unwrap();
    slint::select_bundled_translation("en").unwrap();
}

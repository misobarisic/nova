//! New destinations must remain visible in the desktop rail and phone More.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{
    ComponentHandle,
    platform::{Key, WindowEvent},
};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn visible(app: &nova::AppWindow, label: &str) -> ElementHandle {
    let element = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .expect(label);
    let pos = element.absolute_position();
    let size = element.size();
    let window = app.window().size();
    assert!(
        size.width > 0.0 && size.height > 0.0,
        "{label} must have a visible hit area"
    );
    assert!(
        pos.x >= 0.0
            && pos.y >= 0.0
            && pos.x + size.width <= window.width as f32 + 1.0
            && pos.y + size.height <= window.height as f32 + 1.0,
        "{label} is outside the viewport: {pos:?} {size:?}"
    );
    element
}

fn tap(app: &nova::AppWindow, element: ElementHandle) {
    let pos = element.absolute_position();
    let size = element.size();
    let position = slint::LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height / 2.0);
    for event in [
        WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}
fn key(app: &nova::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}
fn route(app: &nova::AppWindow, downloads: bool) {
    app.set_show_home(false);
    app.set_show_library(false);
    app.set_show_settings(false);
    app.set_show_about(false);
    app.set_show_calendar(!downloads);
    app.set_show_downloads(downloads);
}

fn agenda_navigation(app: &nova::AppWindow) {
    assert_eq!(app.get_calendar_view(), 2);
    let before = app.get_calendar_first();
    tap(app, visible(app, "Previous period"));
    assert_eq!(
        app.get_calendar_first(),
        before - 1,
        "Agenda previous must dispatch -1"
    );
    tap(app, visible(app, "Next period"));
    assert_eq!(
        app.get_calendar_first(),
        before,
        "Agenda next must dispatch +1"
    );
    let today_calls = app.get_calendar_selected();
    tap(app, visible(app, "Today"));
    assert_eq!(
        app.get_calendar_selected(),
        today_calls + 1,
        "Agenda Today must dispatch"
    );
}

fn stage(app: slint::Weak<nova::AppWindow>, step: usize, failures: Rc<RefCell<Vec<String>>>) {
    slint::Timer::single_shot(Duration::from_millis(80), move || {
        let app = app.upgrade().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if step == 0 {
                tap(&app, visible(&app, "Calendar"));
                assert!(
                    app.get_show_calendar(),
                    "desktop Calendar callback must route"
                );
                tap(&app, visible(&app, "Downloads"));
                assert!(
                    app.get_show_downloads(),
                    "desktop Downloads callback must route"
                );
            } else if step == 1 {
                tap(&app, visible(&app, "Calendar"));
                assert!(
                    app.get_show_calendar() && !app.get_more_open(),
                    "More Calendar must route"
                );
                tap(&app, visible(&app, "Month"));
                key(&app, Key::Escape.into());
                assert!(
                    app.get_more_open() && !app.get_show_calendar(),
                    "Escape after button focus must return to More"
                );
                tap(&app, visible(&app, "Downloads"));
                assert!(
                    app.get_show_downloads() && !app.get_more_open(),
                    "More Downloads must route"
                );
            } else if step == 2 {
                let input = ElementHandle::find_by_element_type_name(&app, "SearchField")
                    .next()
                    .unwrap();
                tap(&app, input);
                key(&app, "abc".into());
                key(&app, Key::End.into());
                key(&app, Key::Backspace.into());
                assert_eq!(
                    app.get_calendar_search(),
                    "ab",
                    "Backspace must edit the search"
                );
                assert!(app.get_show_calendar(), "Backspace must keep Calendar open");
                key(&app, Key::Escape.into());
                assert!(
                    app.get_more_open() && !app.get_show_calendar(),
                    "Escape after search focus must return to More"
                );
                app.set_more_open(false);
                route(&app, false);
                app.set_calendar_search("".into());
            } else {
                if step == 10 || step == 11 {
                    agenda_navigation(&app);
                }
                let width = app.window().size().width as f32;
                for element in ElementQuery::from_root(&app)
                    .match_predicate(|e: &ElementHandle| {
                        e.accessible_role() == Some(AccessibleRole::Button)
                    })
                    .find_all()
                {
                    let pos = element.absolute_position();
                    let size = element.size();
                    if pos.x >= 0.0 && pos.x < width && size.width > 0.0 {
                        assert!(
                            pos.x + size.width <= width + 1.0,
                            "page element overflows at {width}px: {pos:?} {size:?}"
                        );
                    }
                }
            }
        }));
        if result.is_err() {
            failures
                .borrow_mut()
                .push(format!("navigation/layout stage {step} failed"));
        }
        if let Some(dir) = std::env::var_os("NOVA_PAGE_PREVIEW_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let snapshot = app.window().take_snapshot().unwrap();
            let bytes = snapshot
                .as_slice()
                .iter()
                .flat_map(|p| [p.r, p.g, p.b, p.a])
                .collect::<Vec<_>>();
            image::save_buffer(
                dir.join(format!("destinations-{step}.png")),
                &bytes,
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        match step {
            0 => {
                app.window().set_size(slint::PhysicalSize::new(360, 800));
                app.set_more_open(true);
            }
            1 => {
                app.set_more_open(false);
                route(&app, false);
            }
            2 => {
                app.window().set_size(slint::PhysicalSize::new(390, 844));
                app.set_calendar_view(1);
                app.set_calendar_title("Oct 12–18, 2026".into());
            }
            3 => {
                app.set_calendar_view(0);
                app.set_calendar_title("October 2026".into());
                app.window().set_size(slint::PhysicalSize::new(1200, 800));
            }
            4 => {
                app.set_show_calendar(false);
                app.set_show_downloads(true);
                app.window().set_size(slint::PhysicalSize::new(320, 800));
            }
            5 => {
                app.window().set_size(slint::PhysicalSize::new(1200, 800));
            }
            6 => {
                route(&app, false);
                app.window().set_size(slint::PhysicalSize::new(650, 800));
            }
            7 => {
                app.window().set_size(slint::PhysicalSize::new(1000, 300));
            }
            8 => {
                app.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(30.0, 150.0),
                    delta_x: 0.0,
                    delta_y: -1000.0,
                });
            }
            9 => {
                visible(&app, "Downloads");
                app.set_calendar_view(2);
                app.set_calendar_title("Oct 11–Nov 10, 2026".into());
                app.window().set_size(slint::PhysicalSize::new(320, 800));
            }
            10 => {
                app.window().set_size(slint::PhysicalSize::new(1200, 800));
            }
            11 => {
                slint::quit_event_loop().unwrap();
                return;
            }
            _ => unreachable!(),
        }
        stage(app.as_weak(), step + 1, failures);
    });
}

#[test]
fn calendar_and_downloads_destinations_fit_desktop_and_phone() {
    if std::env::var_os("NOVA_PAGE_PREVIEW_DIR").is_none() {
        i_slint_backend_testing::init_integration_test_with_system_time();
    }
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(true);
    assert!(
        app.get_calendar_all(),
        "Calendar defaults to historical and future releases"
    );
    // These callbacks record dispatch through independent properties; backend
    // month arithmetic is covered by calendar.rs pure behavior tests.
    let weak = app.as_weak();
    app.on_calendar_step(move |delta| {
        let app = weak.upgrade().unwrap();
        app.set_calendar_first(app.get_calendar_first() + delta);
    });
    let weak = app.as_weak();
    app.on_calendar_today(move || {
        let app = weak.upgrade().unwrap();
        app.set_calendar_selected(app.get_calendar_selected() + 1);
    });
    app.set_animations(false);
    app.set_anim_transitions(false);
    // Headless fixtures do not run the backend that mirrors these settings
    // into Anim. Motion itself is covered by more_transition.
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::Anim>().set_transitions(false);
    let weak = app.as_weak();
    app.global::<nova::NavState>()
        .on_calendar_picked(move || route(&weak.upgrade().unwrap(), false));
    let weak = app.as_weak();
    app.global::<nova::NavState>()
        .on_downloads_picked(move || route(&weak.upgrade().unwrap(), true));
    let weak = app.as_weak();
    app.on_calendar_picked(move || route(&weak.upgrade().unwrap(), false));
    let weak = app.as_weak();
    app.on_downloads_picked(move || route(&weak.upgrade().unwrap(), true));
    let weak = app.as_weak();
    app.on_home_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_calendar(false);
        app.set_show_downloads(false);
        app.set_show_home(true);
    });
    app.set_downloads_page_summary("2 series · 5 episodes".into());
    app.set_downloads_page_counts(Rc::new(slint::VecModel::from(vec![5, 2, 1, 2])).into());
    app.set_downloads_page_groups(
        Rc::new(slint::VecModel::from(
            ["Orbit Nine", "Northbound"]
                .into_iter()
                .enumerate()
                .map(|(group, title)| nova::DownloadGroup {
                    id: format!("series-{group}").into(),
                    title: title.into(),
                    subtitle: "Season 1 · 3 episodes".into(),
                    episodes: Rc::new(slint::VecModel::from(
                        (0..3)
                            .map(|i| nova::DownloadEpisode {
                                id: format!("download-{group}-{i}").into(),
                                title: ["The Drift", "Afterglow", "The Relay"][i].into(),
                                badge: format!("S1 E{}", i + 1).into(),
                                details: "1.2 GiB · MP4 · 1080p".into(),
                                state: i as i32,
                                progress: if i == 1 { 0.42 } else { 0.0 },
                                action_label: if i == 1 {
                                    "Resume".into()
                                } else {
                                    "Play".into()
                                },
                                action_kind: 4,
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_calendar_title("October 2026".into());
    app.set_calendar_day_title("October 13, 2026".into());
    app.set_calendar_count(6);
    let cells = (0..42)
        .map(|i| nova::CalendarDay {
            cell: nova::CalCell {
                epoch: i,
                day: if i < 3 { 28 + i } else { (i - 3) % 31 + 1 },
                in_month: (3..34).contains(&i),
                selected: i == 15,
                today: i == 12,
                count: if i == 15 || i == 19 {
                    2
                } else if i == 17 || i == 21 {
                    1
                } else {
                    0
                },
            },
            preview: if i == 15 {
                "Orbit Nine\nPaper Lanterns".into()
            } else if i == 17 {
                "Northbound".into()
            } else {
                "".into()
            },
            weekday: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][i as usize % 7].into(),
        })
        .collect::<Vec<_>>();
    app.set_calendar_week(Rc::new(slint::VecModel::from(cells[14..21].to_vec())).into());
    app.set_calendar_cells(Rc::new(slint::VecModel::from(cells)).into());
    let rows = ["Orbit Nine", "Paper Lanterns"]
        .into_iter()
        .enumerate()
        .map(|(i, title)| nova::CalendarRelease {
            key: format!("episode-{i}").into(),
            title: title.into(),
            subtitle: "S2 E4 · The Drift".into(),
            date: "October 13, 2026".into(),
            group: if i == 0 {
                "October 13, 2026".into()
            } else {
                "".into()
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
    app.set_calendar_rows(Rc::new(slint::VecModel::from(rows.clone())).into());
    app.set_calendar_later(Rc::new(slint::VecModel::from(rows)).into());
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    let failures = Rc::new(RefCell::new(Vec::new()));
    stage(app.as_weak(), 0, failures.clone());
    slint::run_event_loop().unwrap();
    assert!(failures.borrow().is_empty(), "{:?}", failures.borrow());
}

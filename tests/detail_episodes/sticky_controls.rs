//! Section controls remain reachable after scrolling, including short safe-area windows.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn scroll(app: &nova::AppWindow, id: &str, delta: f32) {
    let view = element(app, id);
    let p = view.absolute_position();
    let s = view.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0),
            delta_x: 0.0,
            delta_y: delta,
        });
    settle();
}
fn click(app: &nova::AppWindow, item: &ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle();
}
#[test]
fn episode_and_manual_tracking_search_pin_to_their_viewport() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "episode_and_manual_tracking_search_pin_to_their_viewport"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Sticky controls".into());
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(24.0, 16.0, 0.0, 28.0),
    );
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (0..30)
                .map(|i| nova::EpisodeRow {
                    text: format!("Episode {i}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_total(30);
    app.window().set_size(slint::PhysicalSize::new(820, 390));
    app.window().show().unwrap();
    settle();
    scroll(&app, "DetailPage::det_scroll", -10000.0);
    let toolbar = element(&app, "DetailEpisodesModule::episode_toolbar");
    let view = element(&app, "DetailPage::det_scroll");
    assert!((toolbar.absolute_position().y - view.absolute_position().y).abs() < 1.0);
    assert!(toolbar.absolute_position().x >= 28.0);
    assert!(toolbar.absolute_position().x + toolbar.size().width <= 804.0);
    click(
        &app,
        &element(&app, "DetailEpisodesModule::episode_search_toggle"),
    );
    assert_eq!(
        element(&app, "DetailEpisodesModule::episode_toolbar")
            .size()
            .height,
        64.0
    );
    assert!(
        element(&app, "DetailEpisodesModule::filter_line")
            .absolute_position()
            .y
            >= view.absolute_position().y
    );
    scroll(&app, "DetailPage::det_scroll", 10000.0);
    assert!(
        element(&app, "DetailEpisodesModule::episode_toolbar")
            .absolute_position()
            .y
            > view.absolute_position().y
    );

    let searches = Rc::new(RefCell::new(Vec::new()));
    let recorded = searches.clone();
    app.on_tracking_search(move |service, query| {
        recorded.borrow_mut().push((service, query.to_string()))
    });
    app.set_tracking_open(true);
    app.set_tracking_candidates(
        Rc::new(VecModel::from(
            (0..30)
                .map(|i| nova::TrackingCandidateRow {
                    title: format!("Release {i}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    settle();
    let choose = ElementHandle::find_by_accessible_label(&app, "Choose another release")
        .next()
        .unwrap();
    click(&app, &choose);
    scroll(&app, "DetailTrackingModule::tracking_scroll", -10000.0);
    let bar = element(&app, "TrackingPanel::manual_search_bar");
    let view = element(&app, "DetailTrackingModule::tracking_scroll");
    assert!((bar.absolute_position().y - view.absolute_position().y).abs() < 1.0);
    assert!(bar.absolute_position().x >= 28.0);
    assert!(bar.absolute_position().x + bar.size().width <= 804.0);
    let search = ElementHandle::find_by_accessible_label(&app, "Search tracker")
        .next()
        .unwrap();
    assert!(search.absolute_position().y >= view.absolute_position().y);
    click(&app, &search);
    assert_eq!(
        &*searches.borrow(),
        &[(0, String::new())],
        "pinned submit remains interactive"
    );
}

#[test]
#[ignore = "requires a native display; set NOVA_DETAIL_STICKY_PREVIEW_DIR"]
fn native_sticky_previews() {
    if !crate::isolation::run(concat!(module_path!(), "::", "native_sticky_previews")) {
        return;
    }
    let dir = std::path::PathBuf::from(
        std::env::var_os("NOVA_DETAIL_STICKY_PREVIEW_DIR").expect("preview directory"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Sticky controls".into());
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (0..30)
                .map(|i| nova::EpisodeRow {
                    text: format!("Episode {i}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_total(30);
    app.set_tracking_candidates(
        Rc::new(VecModel::from(
            (0..30)
                .map(|i| nova::TrackingCandidateRow {
                    title: format!("Release {i}").into(),
                    detail: "A candidate release for the selected tracker".into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(820, 390));
    app.window().show().unwrap();
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(24.0, 16.0, 0.0, 28.0),
    );
    nova_ui::apply_theme(&app, false);
    let timer = slint::Timer::default();
    let mut stage = 0;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let capture = |name: &str| {
                let snapshot = app.window().take_snapshot().unwrap();
                image::save_buffer(
                    dir.join(format!("{name}.png")),
                    snapshot.as_bytes(),
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            };
            let wheel = |id: &str| {
                let view = element(&app, id);
                let p = view.absolute_position();
                let s = view.size();
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0),
                        delta_x: 0.0,
                        delta_y: -10000.0,
                    });
            };
            let tap = |item: ElementHandle| {
                let p = item.absolute_position();
                let s = item.size();
                let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
                for event in [
                    slint::platform::WindowEvent::PointerPressed {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    },
                    slint::platform::WindowEvent::PointerReleased {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    },
                ] {
                    app.window().dispatch_event(event);
                }
            };
            match stage {
                0 => wheel("DetailPage::det_scroll"),
                1 => {
                    capture("episodes-standard");
                    nova_ui::apply_theme(&app, true);
                }
                2 => {
                    capture("episodes-black");
                    nova_ui::apply_theme(&app, false);
                    tap(element(&app, "DetailEpisodesModule::episode_search_toggle"));
                }
                3 => {
                    capture("episode-search-standard");
                    nova_ui::apply_theme(&app, true);
                }
                4 => {
                    capture("episode-search-black");
                    nova_ui::apply_theme(&app, false);
                    app.set_tracking_open(true);
                }
                5 => tap(
                    ElementHandle::find_by_accessible_label(&app, "Choose another release")
                        .next()
                        .unwrap(),
                ),
                6 => wheel("DetailTrackingModule::tracking_scroll"),
                7 => {
                    capture("tracking-standard");
                    nova_ui::apply_theme(&app, true);
                }
                _ => {
                    capture("tracking-black");
                    slint::quit_event_loop().unwrap();
                }
            }
            stage += 1;
        },
    );
    slint::run_event_loop().unwrap();
}

//! Android short landscape scrolls Downloads context, with search and pills pinned together.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel, platform::WindowEvent};
use std::{cell::Cell, rc::Rc, time::Duration};

fn idle() {
    for _ in 0..2 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn fill(app: &nova::AppWindow) {
    app.set_downloads_page_summary("20 downloads · 20 series".into());
    app.set_downloads_page_counts(Rc::new(VecModel::from(vec![20, 20, 0, 0])).into());
    app.set_downloads_page_groups(
        Rc::new(VecModel::from(
            (0..20)
                .map(|i| nova::DownloadGroup {
                    id: format!("series-{i}").into(),
                    title: format!("Downloaded series {i}").into(),
                    subtitle: "Season 1 · 1 episode".into(),
                    episodes: Rc::new(VecModel::from(vec![nova::DownloadEpisode {
                        id: format!("episode-{i}").into(),
                        title: "Episode title".into(),
                        badge: "S1 E1".into(),
                        details: "42 MiB".into(),
                        action_label: "Play".into(),
                        action_kind: 4,
                        ..Default::default()
                    }]))
                    .into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
}
fn scroll(app: &nova::AppWindow, delta: f32) {
    let size = app.window().size();
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(size.width as f32 / 2.0, size.height as f32 - 100.0),
        delta_x: 0.0,
        delta_y: delta,
    });
    idle();
}
fn click(app: &nova::AppWindow, p: LogicalPosition) {
    for event in [
        WindowEvent::PointerPressed {
            position: p,
            button: slint::platform::PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position: p,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    idle();
}
fn drag(app: &nova::AppWindow) {
    let start = LogicalPosition::new(600.0, 400.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: start,
        button: slint::platform::PointerEventButton::Left,
    });
    for i in 1..=10 {
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(start.x, start.y - 30.0 * i as f32),
        });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(start.x, start.y - 300.0),
        button: slint::platform::PointerEventButton::Left,
    });
    idle();
}

#[test]
fn landscape_scroll_pins_search_and_filters_and_shields_safe_top() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    fill(&app);
    app.invoke_navigate_section(5);
    app.window().set_size(slint::PhysicalSize::new(1280, 576));
    app.window().show().unwrap();
    idle();
    let desktop_title = element(&app, "PageHeader::title_text");
    let title_y = desktop_title.absolute_position().y;
    scroll(&app, -200.0);
    assert_eq!(
        desktop_title.absolute_position().y,
        title_y,
        "short desktop keeps its heading pinned"
    );

    app.set_is_android(true);
    app.set_touch_menus(true);
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(16.0, 10.0, 12.0, 8.0),
    );
    idle();
    for true_black in [false, true] {
        nova_ui::apply_theme(&app, true_black);
        scroll(&app, 20000.0);
        let title = element(&app, "PageHeader::title_text");
        let summary = element(&app, "PageHeader::description");
        let card = ElementHandle::find_by_element_type_name(&app, "DownloadSeriesCard")
            .next()
            .unwrap();
        let title_y = title.absolute_position().y;
        let summary_y = summary.absolute_position().y;
        let card_y = card.absolute_position().y;
        drag(&app);
        assert!(title.absolute_position().y < title_y - 100.0);
        assert!(summary.absolute_position().y < summary_y - 100.0);
        assert!(card.absolute_position().y < card_y - 100.0);
        let panel = element(&app, "DownloadsPage::sticky_controls");
        let page = ElementHandle::find_by_element_type_name(&app, "DownloadsPage")
            .next()
            .unwrap();
        assert_eq!(panel.absolute_position().x, page.absolute_position().x);
        assert!((panel.size().width - page.size().width).abs() < 1.0);
        assert!(panel.absolute_position().y.abs() < 1.0);
        let fade = element(&app, "DownloadsPage::controls_fade");
        assert_eq!(fade.size().height, 8.0);
        assert!(
            (fade.absolute_position().y - panel.absolute_position().y - panel.size().height).abs()
                < 1.0
        );
        let search = element(&app, "DownloadsPage::download_search");
        let pills = element(&app, "DownloadsPage::filter_rail");
        assert!((search.absolute_position().y - 16.0).abs() < 1.0);
        assert!((pills.absolute_position().y - search.absolute_position().y - 58.0).abs() < 1.0);
        scroll(&app, -200.0);
        assert!((search.absolute_position().y - 16.0).abs() < 1.0);
        assert!((pills.absolute_position().y - 74.0).abs() < 1.0);
        let actions = Rc::new(Cell::new(0));
        let recorded = actions.clone();
        app.on_downloads_page_action(move |_, _| recorded.set(recorded.get() + 1));
        // Put a real card action behind the safe-top strip, then click its
        // retained coordinates. The opaque panel must absorb that activation.
        scroll(&app, 20000.0);
        let play = ElementHandle::find_by_accessible_label(&app, "Play")
            .next()
            .unwrap();
        let p = play.absolute_position();
        let size = play.size();
        scroll(&app, 8.0 - p.y - size.height / 2.0);
        click(&app, LogicalPosition::new(p.x + size.width / 2.0, 8.0));
        assert_eq!(actions.get(), 0, "safe-top blocks underlying card actions");
    }
    app.set_downloads_page_query("kept query".into());
    app.set_downloads_page_filter(2);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    idle();
    assert_eq!(
        app.get_downloads_page_query(),
        SharedString::from("kept query")
    );
    assert_eq!(app.get_downloads_page_filter(), 2);
    let title = element(&app, "PageHeader::title_text");
    let title_y = title.absolute_position().y;
    scroll(&app, -150.0);
    assert_eq!(
        title.absolute_position().y,
        title_y,
        "portrait restores the fixed heading"
    );
    app.window().set_size(slint::PhysicalSize::new(1280, 576));
    idle();
    assert!(
        element(&app, "DownloadsPage::download_search")
            .absolute_position()
            .y
            > 16.0
    );
    assert_eq!(app.get_downloads_page_filter(), 2);
    let backs = Rc::new(Cell::new(0));
    let recorded = backs.clone();
    app.on_home_picked(move || recorded.set(recorded.get() + 1));
    app.set_system_back_request(app.get_system_back_request() + 1);
    idle();
    assert_eq!(backs.get(), 1);
    app.invoke_navigate_section(5);
    idle();
    let search = element(&app, "DownloadsPage::download_search");
    let p = search.absolute_position();
    click(&app, LogicalPosition::new(p.x + 70.0, p.y + 22.0));
    let text: SharedString = slint::platform::Key::Escape.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    idle();
    assert_eq!(
        backs.get(),
        2,
        "Escape still routes Back from the independent search input"
    );
}

#[test]
#[ignore = "requires a desktop display; SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn render_downloads_landscape_previews() {
    let out = std::path::PathBuf::from(
        std::env::var_os("NOVA_DOWNLOADS_PREVIEW_DIR")
            .unwrap_or_else(|| "output/downloads-layout".into()),
    );
    std::fs::create_dir_all(&out).unwrap();
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.global::<nova::Anim>().set_enabled(false);
    fill(&app);
    app.invoke_navigate_section(5);
    let states = [
        ("desktop", 1280, 900, false, false, false),
        ("phone", 390, 844, true, false, false),
        ("landscape", 1280, 576, true, false, false),
        ("landscape-scrolled", 1280, 576, true, true, false),
        ("landscape-scrolled-black", 1280, 576, true, true, true),
    ];
    app.window()
        .set_size(slint::PhysicalSize::new(states[0].1, states[0].2));
    app.window().show().unwrap();
    let index = Rc::new(Cell::new(0));
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let app = weak.upgrade().unwrap();
            let i = index.get();
            let snapshot = app.window().take_snapshot().unwrap();
            let bytes = snapshot
                .as_slice()
                .iter()
                .flat_map(|p| [p.r, p.g, p.b, p.a])
                .collect::<Vec<_>>();
            image::save_buffer(
                out.join(format!("{}.png", states[i].0)),
                &bytes,
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
            index.set(i + 1);
            if i + 1 == states.len() {
                slint::quit_event_loop().unwrap();
            } else {
                let next = states[i + 1];
                nova_ui::apply_theme(&app, next.5);
                app.set_is_android(next.3);
                app.set_touch_menus(next.3);
                app.window()
                    .set_size(slint::PhysicalSize::new(next.1, next.2));
                i_slint_core::window::WindowInner::from_pub(app.window())
                    .set_window_item_safe_area(i_slint_core::lengths::LogicalEdges::new(
                        if next.3 { 16.0 } else { 0.0 },
                        0.0,
                        0.0,
                        0.0,
                    ));
                if next.4 {
                    app.window().dispatch_event(WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(600.0, 400.0),
                        delta_x: 0.0,
                        delta_y: -500.0,
                    });
                }
            }
        },
    );
    slint::run_event_loop().unwrap();
}

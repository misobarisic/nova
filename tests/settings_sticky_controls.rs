//! Settings keeps compact navigation and section-local addon filters available.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..20 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.absolute_position(), item.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn item(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn setup(app: &nova::AppWindow) {
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(0);
    app.set_settings_detail_open(true);
    app.set_addon_rows(
        Rc::new(VecModel::from(
            (0..20)
                .map(|i| nova::AddonRow {
                    label: format!("Example addon {i}").into(),
                    url: format!("https://example.test/{i}").into(),
                    enabled: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
}

#[test]
fn submenu_navigation_and_installed_filters_pin_without_pinning_forms() {
    if std::env::var_os("NOVA_SETTINGS_STICKY_PREVIEW_DIR").is_some() {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    setup(&app);
    let filters = Rc::new(Cell::new(0));
    let captured = filters.clone();
    app.on_addon_filter_changed(move || captured.set(captured.get() + 1));
    app.show().unwrap();
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(28.0, 16.0, 12.0, 8.0),
    );
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.set_settings_scroll_y(0.0);
        settle(&app);
        let initial_filters = item(&app, "SettingsAddonsModule::installed_controls")
            .absolute_position()
            .y;
        assert!(
            initial_filters > 200.0,
            "installation forms precede installed filters"
        );
        app.set_settings_scroll_y(-180.0);
        settle(&app);
        let header = item(&app, "SettingsPage::sticky_sub_header");
        assert!((header.absolute_position().y - 28.0).abs() < 0.5);
        assert!(
            item(&app, "SettingsAddonsModule::installed_controls")
                .absolute_position()
                .y
                > 88.0,
            "installed filters keep scrolling until their own section reaches the top"
        );
        app.set_addon_search_text("retained query".into());
        app.set_settings_scroll_y(-1000.0);
        settle(&app);
        let pinned = item(&app, "SettingsAddonsModule::installed_controls");
        let p = pinned.absolute_position();
        assert!(
            (p.y - 96.0).abs() < 0.5,
            "{width}: filters pin beneath compact header: {p:?}"
        );
        assert!(p.x >= 12.0 && p.x + pinned.size().width <= width as f32 - 8.0);
        let before_scroll = app.get_settings_scroll_y();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 - 90.0, 750.0),
                delta_x: 0.0,
                delta_y: -200.0,
            });
        settle(&app);
        assert!(
            app.get_settings_scroll_y() < before_scroll,
            "wheel scroll continues below pinned controls"
        );
        assert!(
            (item(&app, "SettingsAddonsModule::installed_controls")
                .absolute_position()
                .y
                - p.y)
                .abs()
                < 0.5
        );
        assert_eq!(app.get_addon_search_text(), "retained query");
        if width == 390 {
            // Android sends these same pointer events for a finger. Spread a
            // horizontal swipe (with slight vertical wobble) across frames so
            // both nested Flickables exercise their native gesture arbitration.
            app.set_is_android(true);
            app.set_addon_capability_filter(0);
            settle(&app);
            let rail = item(&app, "SettingsAddonsModule::capability_rail");
            let chips = item(&app, "SettingsAddonsModule::capability_chips");
            assert!(
                chips.size().width > rail.size().width,
                "phone rail must overflow"
            );
            let before_x = chips.absolute_position().x;
            let document_y = app.get_settings_scroll_y();
            let picks = filters.get();
            let start = slint::LogicalPosition::new(
                rail.absolute_position().x + 180.0,
                rail.absolute_position().y + 20.0,
            );
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                    position: start,
                    button: slint::platform::PointerEventButton::Left,
                });
            for frame in 1..=10 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(
                            start.x - 160.0 * frame as f32 / 10.0,
                            start.y + 8.0 * frame as f32 / 10.0,
                        ),
                    });
            }
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(start.x - 160.0, start.y + 8.0),
                    button: slint::platform::PointerEventButton::Left,
                });
            settle(&app);
            assert!(
                chips.absolute_position().x < before_x - 8.0,
                "finger swipe must move the pill rail"
            );
            assert_eq!(
                app.get_settings_scroll_y(),
                document_y,
                "horizontal rail swipe must not scroll the Settings document"
            );
            assert_eq!(
                filters.get(),
                picks,
                "dragging across pills must not pick a filter"
            );
            assert_eq!(app.get_addon_capability_filter(), 0);
            let streams = ElementHandle::find_by_accessible_label(&app, "Streams")
                .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
                .expect("Streams revealed by touch swipe");
            let x = streams.absolute_position().x;
            assert!(
                x >= rail.absolute_position().x
                    && x + streams.size().width
                        <= rail.absolute_position().x + rail.size().width + 0.5
            );
            streams.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            assert_eq!(app.get_addon_capability_filter(), 3);
            assert_eq!(
                filters.get(),
                picks + 1,
                "the revealed pill remains selectable"
            );
            app.set_is_android(false);
        }
        app.set_addon_capability_filter(2);
        settle(&app);
        let metadata = ElementHandle::find_by_accessible_label(&app, "Metadata")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        metadata.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(app.get_addon_capability_filter(), 2);
        assert!(filters.get() > 0);
        let stable_scroll = app.get_settings_scroll_y();
        for (key, expected) in [
            (slint::platform::Key::RightArrow, 3),
            (slint::platform::Key::LeftArrow, 2),
        ] {
            let text: slint::SharedString = key.into();
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
            settle(&app);
            assert_eq!(app.get_addon_capability_filter(), expected);
            assert_eq!(
                app.get_settings_scroll_y(),
                stable_scroll,
                "pinned chip keyboard focus must not move the document"
            );
        }
        let rail = item(&app, "SettingsAddonsModule::capability_rail");
        let streams = ElementHandle::find_by_accessible_label(&app, "Metadata")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        assert!(streams.absolute_position().x >= rail.absolute_position().x);
        assert!(
            streams.absolute_position().x + streams.size().width
                <= rail.absolute_position().x + rail.size().width + 0.5,
            "selected capability remains visible at its natural width"
        );
        // Clicking the compact Back uses the same submenu return action.
        let back = item(&app, "SettingsPage::sticky_sub_header")
            .query_descendants()
            .match_type_name("SettingsBackButton")
            .find_first()
            .unwrap();
        back.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            app.get_show_settings(),
            "{width}: submenu Back must stay inside Settings"
        );
        if width < 680 {
            assert!(!app.get_settings_detail_open());
        } else {
            assert!(
                app.get_settings_detail_open(),
                "desktop retains its selected pane"
            );
        }
        assert_eq!(
            app.global::<nova::SettingsBridge>().get_kb_zone(),
            8,
            "{width}: Back returns keyboard focus to the Settings menu"
        );
        app.set_settings_selected_id(0);
        app.set_settings_detail_open(true);
    }
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 600));
        app.set_settings_detail_open(false);
        app.set_settings_navigation_scroll(0.0);
        settle(&app);
        assert!(
            ElementHandle::find_by_element_id(&app, "SettingsPage::sticky_landing_header")
                .next()
                .is_none()
        );
        app.set_settings_navigation_scroll(-90.0);
        settle(&app);
        assert!(
            app.get_settings_navigation_scroll() < -68.0,
            "{width}: overview scroll is {}",
            app.get_settings_navigation_scroll()
        );
        let opacity =
            ElementHandle::find_by_element_id(&app, "SettingsPage::sticky_landing_header")
                .next()
                .unwrap_or_else(|| {
                    panic!(
                        "{width}: overview header missing at partial scroll {}, detail {}",
                        app.get_settings_navigation_scroll(),
                        app.get_settings_detail_open()
                    )
                })
                .computed_opacity();
        assert!(
            opacity > 0.0 && opacity < 1.0,
            "overview title fades in during collapse"
        );
        app.set_settings_navigation_scroll(-150.0);
        settle(&app);
        let header = ElementHandle::find_by_element_id(&app, "SettingsPage::sticky_landing_header")
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "{width}: overview header missing at full scroll {}, detail {}",
                    app.get_settings_navigation_scroll(),
                    app.get_settings_detail_open()
                )
            });
        assert_eq!(header.computed_opacity(), 1.0);
        assert!((header.absolute_position().y - 28.0).abs() < 0.5);
        let search = item(&app, "SettingsPage::settings_search");
        assert!(
            search.absolute_position().y
                >= header.absolute_position().y + header.size().height + 8.0
        );
        let y = search.absolute_position().y;
        app.set_settings_navigation_scroll(-300.0);
        settle(&app);
        assert!(
            (item(&app, "SettingsPage::settings_search")
                .absolute_position()
                .y
                - y)
                .abs()
                < 0.5
        );
        let back = header
            .query_descendants()
            .match_type_name("SettingsBackButton")
            .find_first()
            .unwrap();
        back.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(
            !app.get_show_settings(),
            "overview Back returns to More on either layout"
        );
        app.set_show_home(false);
        app.set_show_settings(true);
    }
}

#[test]
fn render_sticky_settings_previews() {
    let Some(dir) = std::env::var_os("NOVA_SETTINGS_STICKY_PREVIEW_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let app = nova::AppWindow::new().unwrap();
    setup(&app);
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.window().set_size(slint::PhysicalSize::new(390, 900));
    app.show().unwrap();
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(28.0, 16.0, 12.0, 8.0),
    );
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    let step = Rc::new(Cell::new(0));
    let scroll_attempts = Rc::new(Cell::new(0));
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let n = step.get();
            if n >= 5 && app.get_settings_navigation_scroll() > -108.0 {
                assert!(scroll_attempts.get() < 2, "overview wheel did not scroll at preview {n}");
                scroll_attempts.set(scroll_attempts.get() + 1);
                let landing = item(&app, "SettingsPage::landing_layer");
                app.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(landing.absolute_position().x + 30.0, landing.absolute_position().y + landing.size().height - 70.0),
                    delta_x: 0.0, delta_y: -200.0,
                });
                return;
            }
            scroll_attempts.set(0);
            if n == 0 {
                app.set_settings_scroll_y(-1000.0);
            } else {
                let snapshot = app.window().take_snapshot().unwrap();
                let pixels = snapshot
                    .as_slice()
                    .iter()
                    .flat_map(|p| [p.r, p.g, p.b, p.a])
                    .collect::<Vec<_>>();
                if (1..=8).contains(&n) {
                    // The header and installed filters are one opaque stack.
                    // A fade here exposed avatar/title fragments above the
                    // filters; only the bottom of the combined stack fades.
                    let id = if n <= 4 { "SettingsPage::sticky_sub_header" } else { "SettingsPage::sticky_landing_header" };
                    let header = ElementHandle::find_by_element_id(&app, id).next().unwrap_or_else(|| panic!("preview {n}: {id} missing; navigation scroll {}, show settings {}, home {}, two pane {}, detail {}", app.get_settings_navigation_scroll(), app.get_show_settings(), app.get_show_home(), app.get_settings_two_pane(), app.get_settings_detail_open()));
                    let p = header.absolute_position();
                    let s = header.size();
                    let scale = app.window().scale_factor();
                    let canvas = app.global::<nova::Theme>().get_current().artwork_canvas;
                    for y in ((p.y + s.height + 1.0) * scale) as usize
                        ..((p.y + s.height + 7.0) * scale) as usize
                    {
                        for x in ((p.x + 1.0) * scale).max(0.0) as usize
                            ..((p.x + s.width - 1.0) * scale)
                                .min(snapshot.width() as f32) as usize
                        {
                            let pixel = snapshot.as_slice()[y * snapshot.width() as usize + x];
                            assert!(pixel.r.abs_diff(canvas.red()) <= 1
                                && pixel.g.abs_diff(canvas.green()) <= 1
                                && pixel.b.abs_diff(canvas.blue()) <= 1,
                                "scrolled cards leak through the stacked-header seam at ({x}, {y}): {pixel:?}");
                        }
                    }
                }
                image::save_buffer(
                    dir.join(format!(
                        "sticky-settings-{}-{}-{}.png",
                        app.window().size().width,
                        if n == 3 || n == 4 || n == 7 || n == 8 {
                            "black"
                        } else {
                            "standard"
                        },
                        if n >= 5 { "overview" } else { "submenu" }
                    )),
                    &pixels,
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
                match n {
                    1 => app.window().set_size(slint::PhysicalSize::new(1280, 900)),
                    2 => {
                        nova_ui::apply_theme(&app, true);
                        app.window().set_size(slint::PhysicalSize::new(390, 900));
                    }
                    3 => app.window().set_size(slint::PhysicalSize::new(1280, 900)),
                    4 => {
                        nova_ui::apply_theme(&app, false);
                        app.window().set_size(slint::PhysicalSize::new(390, 600));
                        app.set_settings_detail_open(false);
                    }
                    5 => app.window().set_size(slint::PhysicalSize::new(1280, 600)),
                    6 => {
                        nova_ui::apply_theme(&app, true);
                        app.window().set_size(slint::PhysicalSize::new(390, 600));
                        app.set_settings_detail_open(false);
                    }
                    7 => app.window().set_size(slint::PhysicalSize::new(1280, 600)),
                    _ => slint::quit_event_loop().unwrap(),
                }
            }
            step.set(n + 1);
        },
    );
    slint::run_event_loop().unwrap();
}

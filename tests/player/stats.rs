//! Read-only diagnostics uses the real compiled player and permanent bridge.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, platform::WindowEvent};
use std::{cell::Cell, cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn button(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
}

fn key(app: &nova::AppWindow, text: impl Into<slint::SharedString>) {
    let text = text.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}

fn fixture(app: &nova::AppWindow, croatian: bool) {
    let rows = nova_ui::backend_text::with_language(
        if croatian {
            nova_config::Language::Croatian
        } else {
            nova_config::Language::English
        },
        || {
            [
                ("Source", "Torrent (RAM-only)"),
                ("Video", "1920 × 1080 @ 23.976 fps"),
                ("Viewport", "1280 × 720"),
                ("Codec", "HEVC Main 10"),
                ("Decoder", "HW+"),
                ("Video bitrate", "—"),
                ("Output drops", "0"),
                ("Decoder drops", "0"),
                ("Buffer", "42.6 s"),
                ("Download", "3.2 MiB/s"),
                ("RAM cache", "148 / 256 MiB"),
                ("Audio", "AAC · 48 kHz · stereo"),
                ("A/V sync", "—"),
                ("Playback speed", "1.545×"),
                ("Renderer", "OpenGL"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (label, value))| nova::PlayerStatsRow {
                label: nova_ui::backend_text::stats_label(label).into(),
                value: value.into(),
                section_start: index == 8 || index == 11,
            })
            .collect::<Vec<_>>()
        },
    );
    app.set_stats_rows(Rc::new(slint::VecModel::from(rows)).into());
}

fn open_from_menu(app: &nova::AppWindow) {
    app.set_osd_visible(true);
    settle(app);
    element(app, "PlayerOverlay::settings_gear")
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
    for _ in 0..12 {
        if let Some(stats) = button(app, "Stats for nerds") {
            assert!(
                stats
                    .query_descendants()
                    .match_inherits("Image")
                    .find_all()
                    .iter()
                    .any(|icon| icon.size().width >= 20.0),
                "diagnostics menu entry has an icon"
            );
            stats.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(app);
            return;
        }
        let scroll = element(app, "SettingsMenu::settings_scroll");
        let p = scroll.absolute_position();
        let size = scroll.size();
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0),
            delta_x: 0.0,
            delta_y: -160.0,
        });
        settle(app);
    }
    panic!("Stats for nerds must be available from Playback settings");
}

#[test]
fn stats_is_available_to_party_guests_and_preserves_playback_shortcuts() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::stats_is_available_to_party_guests_and_preserves_playback_shortcuts"
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
    app.set_duration(600.0);
    app.set_party_active(true);
    app.set_party_can_control(false);
    app.set_party_can_choose_media(false);
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    fixture(&app, false);
    let visibility = Rc::new(RefCell::new(Vec::new()));
    let recorded = visibility.clone();
    app.on_stats_visibility_changed(move |visible| recorded.borrow_mut().push(visible));
    let copies = Rc::new(Cell::new(0));
    let recorded = copies.clone();
    let weak = app.as_weak();
    app.on_stats_copy(move || {
        recorded.set(recorded.get() + 1);
        weak.upgrade()
            .unwrap()
            .set_stats_notice("Diagnostics copied.".into());
    });
    let pauses = Rc::new(Cell::new(0));
    let recorded = pauses.clone();
    app.on_toggle_pause(move || recorded.set(recorded.get() + 1));
    app.on_close_player(|| panic!("closing diagnostics must not close playback"));
    app.on_seek(|_| panic!("diagnostics input sought playback"));
    app.on_seek_relative(|_| panic!("diagnostics input sought playback"));
    app.on_playback_speed_changed(|_| panic!("diagnostics changed playback speed"));
    app.on_party_set_target_speed(|_| panic!("diagnostics changed shared speed"));
    app.window().show().unwrap();
    open_from_menu(&app);
    assert!(
        app.get_stats_visible(),
        "read-only diagnostics is available under HostOnly"
    );
    assert_eq!(&*visibility.borrow(), &[true]);
    assert_eq!(pauses.get(), 0);
    assert!(
        !app.global::<nova::PlayerOsdBridge>()
            .get_osd_auto_hide_blocked()
    );
    app.set_osd_visible(false);
    settle(&app);
    assert!(!app.get_osd_visible());
    assert!(element(&app, "PlayerOverlay::stats_overlay").computed_opacity() > 0.99);
    let card = element(&app, "PlayerOverlay::stats_overlay");
    let p = card.absolute_position();
    let position = slint::LogicalPosition::new(p.x + 22.0, p.y + 20.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position,
        delta_x: 0.0,
        delta_y: -400.0,
    });
    settle(&app);
    assert_eq!(pauses.get(), 0, "card input must not pass through to video");
    key(&app, "k");
    assert_eq!(
        pauses.get(),
        0,
        "ordinary shared-control permission still applies"
    );
    app.set_party_active(false);
    key(&app, "k");
    assert_eq!(
        pauses.get(),
        1,
        "non-modal diagnostics preserves the pause shortcut"
    );
    key(&app, " ");
    assert_eq!(
        pauses.get(),
        2,
        "Space still pauses playback while diagnostics has focus"
    );
    assert!(app.get_stats_visible());
    button(&app, "Copy diagnostics")
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(copies.get(), 1);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Diagnostics copied.")
            .next()
            .is_some()
    );
    let close = button(&app, "Close stats").unwrap();
    let body = element(&app, "PlayerStats::stats_body");
    assert!(
        close.absolute_position().y + close.size().height <= body.absolute_position().y,
        "Close must occupy the header above the scroll body: close={:?}/{:?}, body={:?}/{:?}",
        close.absolute_position(),
        close.size(),
        body.absolute_position(),
        body.size()
    );
    close.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(
        !app.get_stats_visible(),
        "the header Close pointer action must close diagnostics"
    );
    assert!(app.get_player_open());
    assert_eq!(pauses.get(), 2);
    assert_eq!(&*visibility.borrow(), &[true, false]);
}

#[test]
fn stats_keeps_header_and_copy_reachable_with_safe_areas_translation_and_tv_back() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::stats_keeps_header_and_copy_reachable_with_safe_areas_translation_and_tv_back"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_duration(600.0);
    let copies = Rc::new(Cell::new(0));
    let recorded = copies.clone();
    app.on_stats_copy(move || recorded.set(recorded.get() + 1));
    let closes = Rc::new(Cell::new(0));
    let recorded = closes.clone();
    app.on_close_player(move || recorded.set(recorded.get() + 1));
    app.window().show().unwrap();
    for (language, title, close, copy) in [
        ("en", "Stats for nerds", "Close stats", "Copy diagnostics"),
        (
            "hr",
            "Statistika za znatiželjne",
            "Zatvorite statistiku",
            "Kopirajte dijagnostiku",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        fixture(&app, language == "hr");
        for (width, height, tv) in [
            (320, 568, false),
            (800, 260, false),
            (1280, 720, false),
            (960, 540, true),
        ] {
            app.global::<nova::InterfaceStyle>().set_tv(tv);
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            let bridge = app.global::<nova::PlayerOsdBridge>();
            bridge.set_safe_top(20.0);
            bridge.set_safe_left(28.0);
            bridge.set_safe_right(8.0);
            bridge.set_safe_bottom(12.0);
            app.set_stats_visible(true);
            app.set_osd_visible(false);
            app.set_stats_notice("".into());
            settle(&app);
            let card = element(&app, "PlayerOverlay::stats_overlay");
            let p = card.absolute_position();
            let size = card.size();
            assert!(p.x >= 28.0 && p.y >= 20.0);
            assert!(p.x + size.width <= width as f32 - 8.0);
            assert!(p.y + size.height <= height as f32 - 12.0);
            assert!(
                ElementHandle::find_by_accessible_label(&app, title)
                    .next()
                    .is_some()
            );
            for label in [close, copy] {
                let action = button(&app, label)
                    .unwrap_or_else(|| panic!("{language} {width}×{height}: {label} is clipped"));
                let a = action.absolute_position();
                let a_size = action.size();
                assert!(a.y >= p.y && a.y + a_size.height <= p.y + size.height + 0.5);
                assert!(a.x >= p.x && a.x + a_size.width <= p.x + size.width + 0.5);
            }
            let before = copies.get();
            key(&app, slint::platform::Key::DownArrow); // Enter scroll body.
            key(&app, slint::platform::Key::PageDown);
            key(&app, slint::platform::Key::RightArrow); // Copy stays fixed.
            key(&app, slint::platform::Key::Return);
            assert_eq!(
                copies.get(),
                before + 1,
                "TV/keyboard activation copies without playback effects"
            );
            key(&app, slint::platform::Key::Back);
            assert!(
                !app.get_stats_visible(),
                "Back closes diagnostics before player"
            );
            assert_eq!(closes.get(), 0);
            assert!(app.get_player_open());
        }
    }
    slint::select_bundled_translation("en").unwrap();
    app.set_stats_visible(true);
    settle(&app);
    app.set_player_open(false);
    settle(&app);
    assert!(!app.get_stats_visible());
    assert_eq!(app.get_stats_rows().row_count(), 0);
    assert!(app.get_stats_notice().is_empty());
}

#[test]
fn stats_header_drags_with_mouse_and_touch_without_playback_changes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::stats_header_drags_with_mouse_and_touch_without_playback_changes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    use i_slint_core::{input::TouchPhase, platform::InternalEvent};
    use slint::LogicalPosition;

    fn pointer(app: &nova::AppWindow, position: LogicalPosition, phase: TouchPhase, touch: bool) {
        if touch {
            app.window()
                .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                    id: 7,
                    position: i_slint_core::lengths::logical_point_from_api(position),
                    phase,
                }));
        } else {
            app.window().dispatch_event(match phase {
                TouchPhase::Started => WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
                TouchPhase::Moved => WindowEvent::PointerMoved { position },
                TouchPhase::Ended => WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
                TouchPhase::Cancelled => WindowEvent::PointerExited,
            });
        }
        settle(app);
    }
    fn drag(app: &nova::AppWindow, dx: f32, dy: f32, touch: bool) {
        let header = element(app, "PlayerStats::header_drag");
        let p = header.absolute_position();
        let start = LogicalPosition::new(p.x + 32.0, p.y + 20.0);
        pointer(app, start, TouchPhase::Started, touch);
        for fraction in [0.25, 0.5, 0.75, 1.0] {
            pointer(
                app,
                LogicalPosition::new(start.x + dx * fraction, start.y + dy * fraction),
                TouchPhase::Moved,
                touch,
            );
        }
        pointer(
            app,
            LogicalPosition::new(start.x + dx, start.y + dy),
            TouchPhase::Ended,
            touch,
        );
    }
    fn card(app: &nova::AppWindow) -> (LogicalPosition, slint::LogicalSize) {
        let panel = element(app, "PlayerOverlay::stats_overlay");
        (panel.absolute_position(), panel.size())
    }
    fn near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0,
            "actual {actual}, expected {expected}"
        );
    }

    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_duration(600.0);
    app.set_stats_visible(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    fixture(&app, false);
    app.on_toggle_pause(|| panic!("dragging diagnostics paused playback"));
    app.on_seek(|_| panic!("dragging diagnostics sought playback"));
    app.on_seek_relative(|_| panic!("dragging diagnostics sought playback"));
    app.on_volume_step(|_| panic!("dragging diagnostics changed volume"));
    app.on_android_volume_step(|_| panic!("dragging diagnostics changed Android volume"));
    app.on_android_brightness_step(|_| panic!("dragging diagnostics changed brightness"));
    app.on_playback_speed_preview(|_| panic!("dragging diagnostics started a speed hold"));
    app.on_playback_speed_changed(|_| panic!("dragging diagnostics changed speed"));
    app.on_party_set_target_speed(|_| panic!("dragging diagnostics changed the party"));
    let copies = Rc::new(Cell::new(0));
    let recorded = copies.clone();
    app.on_stats_copy(move || recorded.set(recorded.get() + 1));
    app.window().show().unwrap();
    settle(&app);
    let initial = card(&app);
    let dot = element(&app, "PlayerStats::live_dot");
    let live = element(&app, "PlayerStats::live_label");
    assert!(
        dot.absolute_position().x + dot.size().width < live.absolute_position().x,
        "Live dot must precede the label"
    );

    drag(&app, 0.0, 0.0, false);
    assert_eq!(card(&app), initial, "a header click must not move the card");
    drag(&app, 200.0, 90.0, false);
    near(card(&app).0.x, initial.0.x + 200.0);
    near(card(&app).0.y, initial.0.y + 90.0);
    assert_eq!(
        card(&app).1,
        initial.1,
        "moving downward must not shrink the body"
    );
    app.set_is_android(true);
    drag(&app, -60.0, 35.0, true);
    near(card(&app).0.x, initial.0.x + 140.0);
    near(card(&app).0.y, initial.0.y + 125.0);
    assert_eq!(
        card(&app).1,
        initial.1,
        "repeated drags must keep a stable size"
    );

    let before_body = card(&app).0;
    let body = element(&app, "PlayerStats::stats_body");
    let p = body.absolute_position();
    let start = LogicalPosition::new(p.x + 40.0, p.y + 30.0);
    pointer(&app, start, TouchPhase::Started, true);
    pointer(
        &app,
        LogicalPosition::new(start.x + 20.0, start.y - 20.0),
        TouchPhase::Moved,
        true,
    );
    pointer(
        &app,
        LogicalPosition::new(start.x + 20.0, start.y - 20.0),
        TouchPhase::Ended,
        true,
    );
    assert_eq!(
        card(&app).0,
        before_body,
        "body scrolling must not drag the card"
    );
    button(&app, "Copy diagnostics")
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(copies.get(), 1);
    let saved = card(&app).0;
    button(&app, "Close stats")
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(!app.get_stats_visible());
    app.set_stats_visible(true);
    settle(&app);
    assert_eq!(
        card(&app).0,
        saved,
        "reopening retains this player instance's position"
    );

    drag(&app, -2000.0, -2000.0, false);
    near(card(&app).0.x, 8.0);
    near(card(&app).0.y, 8.0);
    drag(&app, 4000.0, 4000.0, true);
    let (p, size) = card(&app);
    near(p.x + size.width, 1272.0);
    near(p.y + size.height, 892.0);
    for (width, height) in [(800, 260), (360, 800), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        let bridge = app.global::<nova::PlayerOsdBridge>();
        bridge.set_safe_top(24.0);
        bridge.set_safe_left(40.0);
        bridge.set_safe_right(12.0);
        bridge.set_safe_bottom(20.0);
        settle(&app);
        let (p, size) = card(&app);
        assert!(
            p.x >= 47.5 && p.y >= 31.5,
            "safe-area top/left clamp: {p:?}"
        );
        assert!(
            p.x + size.width <= width as f32 - 19.5 && p.y + size.height <= height as f32 - 27.5,
            "safe-area right/bottom clamp: {p:?} {size:?}"
        );
        assert!(
            button(&app, "Close stats").is_some() && button(&app, "Copy diagnostics").is_some()
        );
    }
    app.set_player_open(false);
    settle(&app);
    app.set_player_open(true);
    app.set_playback_started(true);
    fixture(&app, false);
    app.set_stats_visible(true);
    settle(&app);
    let bridge = app.global::<nova::PlayerOsdBridge>();
    near(card(&app).0.x, bridge.get_safe_left() + 18.0);
    near(card(&app).0.y, bridge.get_safe_top() + 60.0);
}

#[test]
fn pointer_motion_within_stats_reveals_and_rearms_the_normal_osd_timer() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::pointer_motion_within_stats_reveals_and_rearms_the_normal_osd_timer"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_duration(600.0);
    app.set_stats_visible(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    fixture(&app, false);
    app.on_toggle_pause(|| panic!("hovering stats paused playback"));
    app.on_seek(|_| panic!("hovering stats sought playback"));
    app.on_seek_relative(|_| panic!("hovering stats sought playback"));
    app.on_volume_step(|_| panic!("hovering stats changed volume"));
    app.on_stats_copy(|| panic!("hovering stats copied diagnostics"));
    let calls = Rc::new(Cell::new(0));
    let recorded = calls.clone();
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_osd_visible(true);
        app.invoke_restart_osd_hide();
    });
    app.window().show().unwrap();
    settle(&app);
    for (id, left_margin) in [
        ("PlayerStats::header_drag", false),
        ("PlayerStats::close_touch", false),
        ("PlayerStats::stats_scroll", false),
        ("PlayerStats::row_label", false),
        ("PlayerStats::copy_touch", false),
        ("PlayerOverlay::stats_overlay", true),
    ] {
        let region = element(&app, id);
        let p = region.absolute_position();
        let size = region.size();
        let x = p.x + if left_margin { 2.0 } else { size.width / 2.0 };
        let y = p.y + size.height / 2.0;
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        });
        settle(&app);
        app.set_osd_visible(false);
        let before = calls.get();
        // Stay within the same target while the controls are hidden.
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x + 1.0, y),
        });
        settle(&app);
        assert!(calls.get() > before, "{id} must report pointer movement");
        assert!(app.get_osd_visible(), "{id} must wake the normal OSD");
        assert!(
            !app.global::<nova::PlayerOsdBridge>()
                .get_osd_auto_hide_blocked()
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(3100));
        settle(&app);
        assert!(!app.get_osd_visible(), "{id} must not pin the OSD visible");
        let before = calls.get();
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x + 2.0, y),
        });
        settle(&app);
        assert!(
            calls.get() > before && app.get_osd_visible(),
            "{id} must wake again without leaving the panel"
        );
    }
    let card = element(&app, "PlayerOverlay::stats_overlay");
    let p = card.absolute_position();
    let y = p.y + card.size().height / 2.0;
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2000));
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(p.x + 5.0, y),
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(2000));
    settle(&app);
    assert!(
        app.get_osd_visible(),
        "movement restarts the existing hide deadline"
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    settle(&app);
    assert!(!app.get_osd_visible());
    let before = calls.get();
    fixture(&app, false);
    settle(&app);
    key(&app, "x");
    assert_eq!(
        calls.get(),
        before,
        "layout and keyboard input are not pointer activity"
    );
}

#[test]
fn overflowing_stats_body_still_pans_and_wakes_on_hover() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::overflowing_stats_body_still_pans_and_wakes_on_hover"
    )) {
        return;
    }
    use i_slint_core::{input::TouchPhase, platform::InternalEvent};
    use slint::LogicalPosition;

    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(false);
    app.set_duration(600.0);
    app.set_stats_visible(true);
    app.window().set_size(slint::PhysicalSize::new(800, 260));
    fixture(&app, false);
    app.on_toggle_pause(|| panic!("scrolling stats paused playback"));
    app.on_seek(|_| panic!("scrolling stats sought playback"));
    app.on_seek_relative(|_| panic!("scrolling stats sought playback"));
    app.on_volume_step(|_| panic!("scrolling stats changed volume"));
    app.on_playback_speed_preview(|_| panic!("scrolling stats started a speed hold"));
    let calls = Rc::new(Cell::new(0));
    let recorded = calls.clone();
    let weak = app.as_weak();
    app.on_osd_mouse_moved(move || {
        recorded.set(recorded.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_osd_visible(true);
        app.invoke_restart_osd_hide();
    });
    app.window().show().unwrap();
    settle(&app);
    let card = element(&app, "PlayerOverlay::stats_overlay");
    let original_card = card.absolute_position();
    let row = element(&app, "PlayerStats::row_label");
    let original_row_y = row.absolute_position().y;
    let scroll = element(&app, "PlayerStats::stats_scroll");
    let p = scroll.absolute_position();
    let size = scroll.size();
    let start = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height * 0.75);
    let touch = |position: LogicalPosition, phase: TouchPhase| {
        app.window()
            .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                id: 7,
                position: i_slint_core::lengths::logical_point_from_api(position),
                phase,
            }));
        settle(&app);
    };
    touch(start, TouchPhase::Started);
    touch(
        LogicalPosition::new(start.x, start.y - 12.0),
        TouchPhase::Moved,
    );
    touch(
        LogicalPosition::new(start.x, start.y - 40.0),
        TouchPhase::Moved,
    );
    touch(
        LogicalPosition::new(start.x, start.y - 40.0),
        TouchPhase::Ended,
    );
    assert!(
        row.absolute_position().y < original_row_y - 5.0,
        "touch must pan the overflowing content"
    );
    assert_eq!(
        card.absolute_position(),
        original_card,
        "body panning must not drag the card"
    );
    let hover = LogicalPosition::new(start.x, start.y - 30.0);
    app.window()
        .dispatch_event(WindowEvent::PointerMoved { position: hover });
    settle(&app);
    app.set_osd_visible(false);
    let before = calls.get();
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(hover.x + 1.0, hover.y),
    });
    settle(&app);
    assert!(
        calls.get() > before && app.get_osd_visible(),
        "hover still wakes the OSD after touch panning"
    );
    assert!(
        !app.global::<nova::PlayerOsdBridge>()
            .get_osd_auto_hide_blocked()
    );
    let after_touch = row.absolute_position().y;
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: hover,
        delta_x: 0.0,
        delta_y: -80.0,
    });
    settle(&app);
    assert!(
        row.absolute_position().y < after_touch,
        "wheel scrolling remains available"
    );
    assert_eq!(card.absolute_position(), original_card);
}

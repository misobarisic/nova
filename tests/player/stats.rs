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

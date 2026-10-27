//! Real player overlay: layout, hidden-OSD taps and nearest-layer Back.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let s = element.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    click_at(app, position);
}

fn click_at(app: &nova::AppWindow, position: LogicalPosition) {
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
    settle(app);
}

fn banner(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "NextEpisodeBanner").next()
}

#[test]
fn banner_fits_and_routes_actions_without_interrupting_playback() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "banner_fits_and_routes_actions_without_interrupting_playback"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_next_episode_token("session:episode".into());
    app.set_next_episode_context("S12 E100".into());
    app.set_next_episode_title(
        "A very long episode title that must elide without covering the buttons".into(),
    );
    app.set_next_episode_visible(true);
    app.window().show().unwrap();
    let choices = Rc::new(RefCell::new(Vec::new()));
    let recorded = choices.clone();
    app.on_next_episode_streams(move |token| recorded.borrow_mut().push(token.to_string()));
    let dismissals = Rc::new(RefCell::new(Vec::new()));
    let recorded = dismissals.clone();
    let weak = app.as_weak();
    app.on_next_episode_dismiss(move |token| {
        recorded.borrow_mut().push(token.to_string());
        weak.upgrade().unwrap().set_next_episode_visible(false);
    });
    let pauses = Rc::new(RefCell::new(0));
    let recorded = pauses.clone();
    app.on_toggle_pause(move || *recorded.borrow_mut() += 1);
    let closes = Rc::new(RefCell::new(0));
    let recorded = closes.clone();
    app.on_close_player(move || *recorded.borrow_mut() += 1);

    for (width, height) in [
        (360, 800),
        (800, 360),
        (1280, 720),
        (320, 568),
        (360, 640),
        (640, 360),
    ] {
        for android in [false, true] {
            for osd_visible in [false, true] {
                for language in ["en", "hr"] {
                    slint::select_bundled_translation(language).unwrap();
                    app.set_is_android(android);
                    app.set_osd_visible(osd_visible);
                    app.window()
                        .set_size(slint::PhysicalSize::new(width, height));
                    settle(&app);
                    let banner = banner(&app).expect("banner mounted");
                    let p = banner.absolute_position();
                    let s = banner.size();
                    assert!(
                        s.width <= 361.0 && s.height <= 65.0,
                        "offer must stay a slim strip at {width}x{height}: {s:?}"
                    );
                    assert!(
                        p.x >= 0.0
                            && p.y >= 0.0
                            && p.x + s.width <= width as f32 + 1.0
                            && p.y + s.height <= height as f32 + 1.0,
                        "banner overflow at {width}x{height}: {p:?} {s:?}"
                    );
                    assert!((width as f32 - p.x - s.width - 16.0).abs() <= 1.0);
                    let actual_gap = height as f32 - p.y - s.height;
                    let state = format!(
                        "{width}x{height}, android={android}, osd={osd_visible}, language={language}, banner={p:?} {s:?}, bottom_gap={actual_gap}"
                    );
                    if osd_visible {
                        let footer =
                            ElementHandle::find_by_element_id(&app, "PlayerOverlay::player_header")
                                .next()
                                .expect("measured player footer caption");
                        let footer_top = footer.absolute_position().y;
                        assert!(
                            footer.computed_opacity() > 0.99,
                            "fixture must reveal controls: {state}"
                        );
                        assert!(
                            p.y + s.height <= footer_top - 11.0,
                            "banner must clear the actual caption/footer with 12px spacing: {state}, footer_top={footer_top}"
                        );
                        let preferred_bottom = footer_top - 12.0;
                        let center = ElementHandle::find_by_element_id(
                            &app,
                            "PlayerOverlay::center_transport",
                        )
                        .next()
                        .filter(|control| control.computed_opacity() > 0.99);
                        let avoids_center = center.as_ref().is_some_and(|control| {
                            let c = control.absolute_position();
                            let size = control.size();
                            p.x < c.x + size.width
                                && p.x + s.width > c.x
                                && preferred_bottom - s.height < c.y + size.height
                                && preferred_bottom > c.y
                        });
                        if !avoids_center {
                            assert!(
                                (p.y + s.height - preferred_bottom).abs() <= 1.0,
                                "offer retains bottom-right anchoring when the footer lane is clear: {state}, footer_top={footer_top}"
                            );
                        }
                    } else {
                        assert!(
                            (actual_gap - 16.0).abs() <= 1.0,
                            "hidden controls keep the offer 16px from the bottom: {state}"
                        );
                    }
                    assert!(
                        banner
                            .query_descendants()
                            .match_inherits("Image")
                            .find_all()
                            .is_empty(),
                        "text-only strip must not mount artwork"
                    );
                    let action = ElementHandle::find_by_accessible_label(
                        &app,
                        if language == "hr" {
                            "Odaberite streamove"
                        } else {
                            "Choose streams"
                        },
                    )
                    .next()
                    .expect("choose action");
                    let a = action.absolute_position();
                    let size = action.size();
                    assert!(size.height >= 43.0 && size.width >= 43.0);
                    let arrow = action
                        .query_descendants()
                        .match_inherits("IcChevronRight")
                        .find_first()
                        .expect("action arrow");
                    let arrow_pos = arrow.absolute_position();
                    assert!(
                        (arrow_pos.y + arrow.size().height / 2.0 - a.y - size.height / 2.0).abs()
                            <= 1.0,
                        "action arrow is not vertically centered at {width}x{height}"
                    );
                    assert!(
                        a.x >= p.x
                            && a.y >= p.y
                            && a.x + size.width <= p.x + s.width + 1.0
                            && a.y + size.height <= p.y + s.height + 1.0,
                        "action clipped at {width}x{height}"
                    );
                    let dismiss = ElementHandle::find_by_accessible_label(
                        &app,
                        if language == "hr" {
                            "Sakrijte sljedeću epizodu"
                        } else {
                            "Dismiss next episode"
                        },
                    )
                    .next()
                    .expect("dismiss action");
                    let d = dismiss.absolute_position();
                    let ds = dismiss.size();
                    assert!(ds.width >= 43.0 && ds.height >= 43.0);
                    assert!(d.x >= a.x + size.width && d.x + ds.width <= p.x + s.width + 1.0);
                    assert!((d.y - a.y).abs() <= 1.0, "actions must stay on one row");
                    if android && osd_visible {
                        let transport = ElementHandle::find_by_element_id(
                            &app,
                            "PlayerOverlay::center_transport",
                        )
                        .next()
                        .expect("visible Android center transport");
                        let c = transport.absolute_position();
                        let size = transport.size();
                        assert!(
                            p.x + s.width <= c.x
                                || p.x >= c.x + size.width
                                || p.y + s.height <= c.y
                                || p.y >= c.y + size.height,
                            "banner overlaps actual center transport: {state}, transport={c:?} {size:?}"
                        );
                    }
                }
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    app.set_is_android(true);
    app.set_osd_visible(true);
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    settle(&app);
    // Mounting the banner must not steal focus from Pause.
    key(&app, slint::platform::Key::Return);
    assert_eq!(*pauses.borrow(), 1);
    app.set_osd_visible(false);
    settle(&app);
    let action = ElementHandle::find_by_accessible_label(&app, "Choose streams")
        .next()
        .unwrap();
    click(&app, &action);
    assert_eq!(&*choices.borrow(), &["session:episode"]);
    assert_eq!(*pauses.borrow(), 1, "banner tap leaked to video");

    // The OSD's transparent hover zone extends above its controls. It must
    // not intercept the bottom of the strip's touch targets when visible.
    app.set_osd_visible(true);
    settle(&app);
    let action = ElementHandle::find_by_accessible_label(&app, "Choose streams")
        .next()
        .unwrap();
    let p = action.absolute_position();
    let s = action.size();
    click_at(
        &app,
        LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height - 2.0),
    );
    assert_eq!(choices.borrow().len(), 2);
    assert!(app.get_osd_visible(), "offer tap must not hide controls");
    assert_eq!(*pauses.borrow(), 1);
    app.set_osd_visible(false);
    settle(&app);

    let dismiss = ElementHandle::find_by_accessible_label(&app, "Dismiss next episode")
        .next()
        .unwrap();
    click(&app, &dismiss);
    assert!(banner(&app).is_none());
    assert_eq!(*closes.borrow(), 0);
    app.set_next_episode_visible(true);
    settle(&app);
    // Tab navigation reaches the camera and banner, skipping Android's absent fullscreen.
    for _ in 0..10 {
        key(&app, slint::platform::Key::Tab);
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Shift.into(),
        });
    key(&app, slint::platform::Key::Tab);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Shift.into(),
        });
    key(&app, slint::platform::Key::Return);
    assert_eq!(choices.borrow().len(), 3);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Shift.into(),
        });
    // Reverse traversal includes the existing speed shortcut before Camera;
    // Android skips its absent fullscreen control between Camera and Settings.
    key(&app, slint::platform::Key::Tab); // Playback speed.
    key(&app, slint::platform::Key::Tab); // Camera.
    key(&app, slint::platform::Key::Tab); // Settings gear.
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Shift.into(),
        });
    key(&app, slint::platform::Key::Return); // Settings gear.
    let menu = ElementHandle::find_by_element_type_name(&app, "SettingsMenu")
        .next()
        .expect("keyboard traversal must open the player settings menu");
    assert!(menu.computed_opacity() > 0.99);
    assert!(banner(&app).is_none(), "banner must yield to player menus");
    key(&app, slint::platform::Key::Escape);
    assert!(banner(&app).is_some());
    key(&app, slint::platform::Key::Back);
    assert_eq!(dismissals.borrow().len(), 2);
    assert_eq!(*closes.borrow(), 0, "first Back dismisses the offer only");
    key(&app, slint::platform::Key::Back);
    assert_eq!(*closes.borrow(), 1);

    slint::select_bundled_translation("hr").unwrap();
    app.set_next_episode_visible(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Odaberite streamove")
            .next()
            .is_some()
    );
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn party_next_button_and_media_key_follow_live_control_permissions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::party_next_button_and_media_key_follow_live_control_permissions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_next_episode_visible(true);
    app.set_next_episode_token("current-generation:next".into());
    app.set_party_active(true);
    let choices = Rc::new(RefCell::new(Vec::new()));
    let recorded = choices.clone();
    app.on_next_episode_streams(move |token| recorded.borrow_mut().push(token.to_string()));
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.window().show().unwrap();
    for tv in [false, true] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        for (host, everyone, busy) in [
            (true, false, false),
            (false, false, false),
            (false, true, false),
            (false, true, true),
            (false, true, false),
            (false, false, false),
        ] {
            app.set_party_is_host(host);
            app.set_party_allow_everyone(everyone);
            app.set_party_can_control(host || everyone);
            app.set_party_can_choose_media(host || everyone);
            app.set_party_busy(busy);
            app.set_osd_visible(true);
            settle(&app);
            let allowed = (host || everyone) && !busy;
            let choose = ElementHandle::find_by_accessible_label(&app, "Choose streams")
                .next()
                .expect("next episode action");
            assert_eq!(choose.accessible_enabled(), Some(allowed));
            let count = choices.borrow().len();
            click(&app, &choose);
            assert_eq!(
                choices.borrow().len(),
                count + usize::from(allowed),
                "pointer next: TV={tv}, host={host}, everyone={everyone}, busy={busy}"
            );
            let media_next: slint::SharedString = "\u{e104}".into();
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                    text: media_next.clone(),
                });
            app.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: media_next });
            settle(&app);
            assert_eq!(
                choices.borrow().len(),
                count + 2 * usize::from(allowed),
                "native media next uses the same accepted permissions"
            );
            let dismiss = ElementHandle::find_by_accessible_label(&app, "Dismiss next episode")
                .next()
                .expect("dismiss remains accessible");
            assert_ne!(dismiss.accessible_enabled(), Some(false));
        }
    }
}

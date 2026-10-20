//! Settings → Sync subpage horizontal-overflow regression test (headless).
//!
//! The Sync subpage renders long unbreakable tokens: the endpoint id, the
//! invite ticket (`NV1` + base32, ~82 chars) and status/error lines that embed
//! endpoint ids. Wrapped text reports its longest token as its minimum width,
//! so without `min-width: 0` these would widen the whole settings content
//! layer and let the page pan sideways. Asserts nothing extends past the
//! window edge at phone widths.

use crate::destinations;
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

#[test]
fn peer_removal_confirmation_stays_in_view_after_scrolling() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::peer_removal_confirmation_stays_in_view_after_scrolling"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_sync_enabled(true);
    app.invoke_more_bridge_sync_picked();
    app.set_settings_detail_open(true);
    let peers = (0..20)
        .map(|i| nova::SyncPeer {
            id: format!("peer-{i}").into(),
            name: ("Long living room device name ".repeat(4) + &i.to_string()).into(),
            last_seen: "Last connected 2m ago".into(),
        })
        .collect::<Vec<_>>();
    app.set_sync_peers(Rc::new(VecModel::from(peers)).into());
    let removed = Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = removed.clone();
    app.on_sync_remove_peer(move |id| recorded.borrow_mut().push(id.to_string()));
    app.show().unwrap();
    let settle = || {
        for _ in 0..12 {
            let _ = max_right_edge(&app);
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(25));
        }
    };
    let element = |id: &str| {
        ElementHandle::find_by_element_id(&app, id)
            .next()
            .expect(id)
    };
    let remove_button = || {
        ElementQuery::from_root(&app)
            .match_predicate(|e| e.accessible_id().as_deref() == Some("sync-remove:peer-19"))
            .find_all()
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "last peer removal button; visible IDs: {:?}",
                    ElementQuery::from_root(&app)
                        .match_predicate(|e| e.accessible_id().is_some())
                        .find_all()
                        .into_iter()
                        .map(|e| e.accessible_id().unwrap().to_string())
                        .collect::<Vec<_>>()
                )
            })
    };
    let key = |key: slint::platform::Key| {
        let text: SharedString = key.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        settle();
    };
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for (width, height) in [(320, 800), (720, 320), (1280, 800)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle();
            assert!(app.get_sync_enabled(), "sync fixture must be enabled");
            assert_eq!(
                app.get_settings_selected_id(),
                7,
                "Sync page must be mounted"
            );
            app.set_settings_scroll_y(0.0);
            for attempt in 0..100 {
                settle();
                let visible = ElementQuery::from_root(&app)
                    .match_predicate(|e| {
                        e.accessible_id().as_deref() == Some("sync-remove:peer-19")
                    })
                    .find_all()
                    .into_iter()
                    .any(|e| {
                        let y = e.absolute_position().y;
                        y >= 24.0 && y + e.size().height <= height as f32 - 24.0
                    });
                if visible {
                    break;
                }
                app.set_settings_scroll_y(-120.0 * (attempt + 1) as f32);
            }
            let _ = remove_button();
            let offset = app.get_settings_scroll_y();
            assert!(
                offset < -100.0,
                "fixture must exercise a scrolled peer list"
            );
            let before = removed.borrow().len();
            remove_button().mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            let panel = element("SettingsPage::sync_remove_panel");
            let p = panel.absolute_position();
            let size = panel.size();
            assert!(p.x >= 0.0 && p.y >= 0.0);
            assert!(p.x + size.width <= width as f32 + 1.0);
            assert!(p.y + size.height <= height as f32 + 1.0);
            for id in [
                "SettingsPage::sync_remove_cancel",
                "SettingsPage::sync_remove_confirm",
            ] {
                let action = element(id);
                let p = action.absolute_position();
                let size = action.size();
                assert!(
                    size.height >= 44.0 && p.y >= 0.0 && p.y + size.height <= height as f32 + 1.0
                );
            }
            assert!((app.get_settings_scroll_y() - offset).abs() < 1.0);
            element("SettingsPage::sync_remove_cancel")
                .mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            assert_eq!(removed.borrow().len(), before);
            assert!(
                ElementHandle::find_by_element_id(&app, "SettingsPage::sync_remove_panel")
                    .next()
                    .is_none()
            );
            remove_button().mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            key(slint::platform::Key::Back);
            assert!(
                ElementHandle::find_by_element_id(&app, "SettingsPage::sync_remove_panel")
                    .next()
                    .is_none()
            );
            assert!(app.get_show_settings());
            assert!((app.get_settings_scroll_y() - offset).abs() < 1.0);
            remove_button().mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            key(slint::platform::Key::Return); // Cancel is the safe initial action.
            assert_eq!(removed.borrow().len(), before);
            remove_button().mock_single_click(slint::platform::PointerEventButton::Left);
            settle();
            if locale == "en" {
                element("SettingsPage::sync_remove_confirm")
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                settle();
            } else {
                key(slint::platform::Key::RightArrow);
                key(slint::platform::Key::Return);
            }
            assert_eq!(removed.borrow().len(), before + 1);
            assert_eq!(removed.borrow().last().unwrap(), "peer-19");
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Rightmost edge over elements whose left edge is on screen. The inactive
/// landing layer (slid left) is ignored by the `x` bound; a real overflow
/// keeps its left edge visible while its right edge runs past the window.
fn max_right_edge(app: &nova::AppWindow) -> f32 {
    let width = app.window().size().width as f32;
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|e| {
            // More stays mounted behind its animated child. Its fully faded
            // descendants do not contribute visible overflow on Sync.
            if e.computed_opacity() < 0.01 {
                return None;
            }
            let sz = e.size();
            let p = e.absolute_position();
            (p.x >= 0.0 && p.x < width && sz.width > 0.0).then_some(p.x + sz.width)
        })
        .fold(0.0f32, f32::max)
}

#[test]
fn settings_sync_subpage_has_no_horizontal_overflow() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "settings_sync_subpage_has_no_horizontal_overflow"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    app.set_sync_enabled(true);
    // Exercise the Android-only scanner button layout too.
    app.set_sync_qr_scan_available(true);
    let long_id = "a".repeat(64);
    app.set_sync_identity(s(long_id.as_str()));
    app.set_sync_invite_ticket(s(
        "NV1MFRGGZDFMZTWQ2LKNNWG23TPOBYXE43UOWLZVCVNBQWY5DFMFRGGZDFMZTWQ2LKNNWG23T",
    ));
    app.set_sync_status(s(&format!(
        "Last sync failed: connect to {long_id} timed out"
    )));
    app.set_sync_link_notice(s(
        "Paired with a-device-with-an-extremely-long-name-for-testing",
    ));
    app.set_sync_peers(
        Rc::new(VecModel::from(vec![nova::SyncPeer {
            id: s(long_id.as_str()),
            name: s("A device with an extremely long name for testing the layout"),
            last_seen: s("Last connected 2m ago"),
        }]))
        .into(),
    );

    let failures: Rc<std::cell::RefCell<Vec<String>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Sync is an account-wide destination under More.
        let sync_link = ({
            app.set_more_open(true);
            destinations::find(&app, "more:sync").next()
        })
        .expect("Sync landing entry");
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            sync_link
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let edge = max_right_edge(&app);
                if edge > 361.0 {
                    failures2.borrow_mut().push(format!(
                        "360px sync: content reaches {edge:.1}, want <= 360"
                    ));
                }
                app.window().set_size(slint::PhysicalSize::new(320, 800));
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    let edge = max_right_edge(&app);
                    if edge > 321.0 {
                        failures3.borrow_mut().push(format!(
                            "320px sync: content reaches {edge:.1}, want <= 320"
                        ));
                    }
                    // Translated labels run longer than their English sources
                    // ("Join" → "Pridružite se", the invite ticket heading),
                    // and a card heading that cannot wrap reports its full
                    // single-line width as the page minimum: re-check the
                    // narrowest width in Croatian. Skipped in the interpreter
                    // build, which bundles no catalogs.
                    if slint::select_bundled_translation("hr").is_err() {
                        slint::quit_event_loop().unwrap();
                        return;
                    }
                    app.set_sync_status(s(&format!(
                        "Posljednja sinkronizacija nije uspjela: veza s {long_id} je istekla"
                    )));
                    app.set_sync_link_notice(s("Upareno s uređajem s-izuzetno-dugackim-nazivom"));
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        let edge = max_right_edge(&app);
                        if edge > 321.0 {
                            failures4.borrow_mut().push(format!(
                                "hr 320px sync: content reaches {edge:.1}, want <= 320"
                            ));
                        }
                        slint::quit_event_loop().unwrap();
                    });
                });
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "overflow failures:\n  {}",
        failures.join("\n  ")
    );
}

/// Incoming pairing is a separate conditional card: its spaced match code
/// must not impose a desktop-sized minimum width on the phone document.
#[test]
fn incoming_pairing_code_fits_phone_widths_in_both_languages() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::incoming_pairing_code_fits_phone_widths_in_both_languages"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.set_sync_enabled(true);
    app.invoke_more_bridge_sync_picked();
    app.set_settings_detail_open(true);
    app.set_sync_pair_prompt_visible(true);
    app.set_sync_pair_prompt_name("Living room television with a long device name".into());
    app.set_sync_pair_prompt_code("123456".into());
    app.global::<nova::Anim>().set_enabled(false);
    app.show().unwrap();
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for (width, height) in [(320, 800), (360, 800), (390, 844), (720, 360), (1280, 900)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for _ in 0..20 {
                let _ = max_right_edge(&app);
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
            }
            let edge = max_right_edge(&app);
            assert!(
                edge <= width as f32 + 1.0,
                "{locale} incoming pairing at {width}x{height}: content reaches {edge}"
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

#[test]
fn sync_inspector_explanations_wrap_across_phone_and_landscape_sizes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::sync_inspector_explanations_wrap_across_phone_and_landscape_sizes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.invoke_more_bridge_sync_picked();
    app.set_settings_detail_open(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 1000));
    app.show().unwrap();
    let settle = || {
        for _ in 0..12 {
            let _ = max_right_edge(&app);
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(25));
        }
    };
    let click = |id: &str| {
        let query_id = id.to_owned();
        ElementQuery::from_root(&app)
            .match_predicate(move |element| {
                element.accessible_id().as_deref() == Some(query_id.as_str())
            })
            .find_first()
            .unwrap_or_else(|| panic!("missing inspector action {id}"))
            .mock_single_click(slint::platform::PointerEventButton::Left);
    };
    settle();
    click("sync-inspector-toggle");
    settle();
    assert!(
        app.global::<nova::SettingsBridge>()
            .get_sync_inspector_active()
    );
    click("sync-inspector-episodes");
    let long = "VeryLongUnbrokenEpisodeTitle".repeat(12);
    app.global::<nova::SettingsBridge>()
        .set_sync_inspector_rows(
            Rc::new(VecModel::from(vec![nova::SyncInspectorRow {
                title: long.clone().into(),
                detail: "120 current · 3 deletions retained · waiting to apply on this device"
                    .into(),
                status: "Stored on this device".into(),
            }]))
            .into(),
        );
    app.global::<nova::SettingsBridge>().set_sync_inspector_episodes(
        Rc::new(VecModel::from(vec![nova::SyncInspectorRow {
            title: long.into(),
            detail: "The unwatch action wins over the watch action.\nWatch action: 1760000000000:123 · ffffffffffffffff\nUnwatch action: 1760000000000:124 · aaaaaaaaaaaaaaaa".into(),
            status: "Unwatched".into(),
        }]))
        .into(),
    );
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for (width, height) in [(320, 800), (360, 800), (720, 320), (1280, 900)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle();
            let edge = max_right_edge(&app);
            assert!(
                edge <= width as f32 + 1.0,
                "{locale} inspector at {width}x{height}: content reaches {edge}"
            );
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

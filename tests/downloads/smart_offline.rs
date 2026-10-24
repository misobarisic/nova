//! Covered download controls stay inert while the offline management page owns input.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, Model, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

#[test]
fn smart_downloads_animates_both_directions_with_header_back() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::smart_downloads_animates_both_directions_with_header_back"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (width, tv) in [(390., false), (1280., false), (1920., true)] {
        for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
            let app = nova::AppWindow::new().unwrap();
            app.global::<nova::InterfaceStyle>().set_tv(tv);
            let anim = app.global::<nova::Anim>();
            anim.set_enabled(enabled);
            anim.set_transitions(transitions);
            app.set_show_home(false);
            app.set_show_downloads(true);
            app.window().set_size(slint::LogicalSize::new(width, 1080.));
            app.show().unwrap();
            settle(&app);
            let offline = app.global::<nova::OfflineBridge>();
            let weak = app.as_weak();
            offline.on_closed(move || {
                weak.upgrade()
                    .unwrap()
                    .global::<nova::OfflineBridge>()
                    .set_opened(false);
            });
            let underlay =
                ElementHandle::find_by_element_id(&app, "DownloadsPage::downloads_layer")
                    .next()
                    .expect("retained Downloads list");
            let underlay_origin = underlay.absolute_position().x;
            offline.set_opened(true);
            let panel = ElementHandle::find_by_element_id(&app, "SmartDownloads::panel")
                .next()
                .expect("retained Smart downloads content");
            // Keep the handles so the final hidden frame remains measurable.
            let sample = || {
                (
                    panel.absolute_position().x,
                    panel.computed_opacity(),
                    underlay.absolute_position().x,
                    underlay.computed_opacity(),
                )
            };
            let advance = |millis| {
                for _ in 0..millis {
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
                    let _ = sample();
                }
            };
            let _ = sample();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
            let start = sample();
            advance(90);
            let middle = sample();
            advance(220);
            let end = sample();
            if enabled && transitions {
                assert!(start.0 > middle.0 && middle.0 > end.0, "entry slide");
                assert!(start.1 < middle.1 && middle.1 < end.1, "entry fade");
                assert!(
                    start.0 - end.0 > panel.size().width * 0.8,
                    "the submenu enters from a full screen width"
                );
                assert!(
                    start.2 > middle.2 && middle.2 > end.2,
                    "Downloads pushes left"
                );
                assert!(
                    start.3 > middle.3 && middle.3 > end.3,
                    "Downloads fades out"
                );
                assert!(
                    (end.2 - underlay_origin + panel.size().width * 0.28).abs() < 0.5,
                    "Downloads uses the standard 28% push distance"
                );
            } else {
                assert!(
                    (start.0 - end.0).abs() < 0.1 && start.1 > 0.99,
                    "disabled entry: width={width}, enabled={enabled}, transitions={transitions}: {start:?} -> {end:?}"
                );
            }
            let header = ElementHandle::find_by_element_id(&app, "SmartDownloads::header")
                .next()
                .unwrap();
            let back = header
                .query_descendants()
                .match_type_name("SettingsBackButton")
                .find_all()
                .pop()
                .expect("shared chevron button inside header");
            let p = back.absolute_position();
            let size = back.size();
            assert_eq!(size.width, if tv { 56. } else { 44. });
            assert!(p.y < header.absolute_position().y + header.size().height);
            assert!(
                header
                    .query_descendants()
                    .match_type_name("PillButton")
                    .find_all()
                    .is_empty(),
                "Back is a header chevron, not a separate text pill"
            );
            let position = LogicalPosition::new(p.x + size.width / 2., p.y + size.height / 2.);
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
            assert!(!offline.get_opened(), "header Back closes the submenu");
            let _ = sample();
            if enabled && transitions {
                assert!(offline.get_visual_retained(), "retain the return animation");
                app.set_system_back_request(app.get_system_back_request() + 1);
                assert!(app.get_show_downloads(), "repeated Back stays on Downloads");
                advance(90);
                let returning = sample();
                assert!(returning.0 > end.0 && returning.1 < end.1);
                assert!(
                    returning.2 > end.2 && returning.3 > end.3,
                    "Downloads returns with the submenu"
                );
                // Reopening reverses the retained panel rather than resetting it.
                offline.set_opened(true);
                let reopened = sample();
                assert!((reopened.0 - returning.0).abs() < 0.1);
                advance(300);
                app.set_system_back_request(app.get_system_back_request() + 1);
                let _ = sample();
                advance(300);
                assert!(!offline.get_visual_retained(), "animated return completes");
                offline.set_opened(true);
                let _ = sample();
                advance(100);
                offline.set_opened(false);
                let _ = sample();
                advance(40);
                anim.set_enabled(false);
                let _ = sample();
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
                assert!(
                    !offline.get_visual_retained(),
                    "master-off ends the return immediately"
                );
            }
            advance(300);
            assert!(
                !offline.get_visual_retained(),
                "return releases the overlay"
            );
            assert!(app.get_show_downloads());
            offline.set_opened(true);
            let _ = sample();
            app.set_show_downloads(false);
            settle(&app);
            assert!(
                !offline.get_visual_retained(),
                "navigation clears the Back guard"
            );
        }
    }
}

#[test]
fn smart_downloads_blocks_covered_actions_and_back_closes_only_one_layer() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "smart_downloads_blocks_covered_actions_and_back_closes_only_one_layer"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_downloads(true);
    app.set_downloads_page_counts(Rc::new(VecModel::from(vec![0, 0, 0, 0])).into());
    app.window().set_size(slint::LogicalSize::new(420.0, 840.0));
    let sorted = ElementHandle::find_by_accessible_label(&app, "Newest first")
        .next()
        .expect("underlying Downloads sort control");
    let position = sorted.absolute_position();
    let size = sorted.size();
    let sorts = Rc::new(Cell::new(0));
    let seen = sorts.clone();
    app.on_downloads_page_sort_changed(move |_| seen.set(seen.get() + 1));
    let offline = app.global::<nova::OfflineBridge>();
    offline.set_opened(true);
    settle(&app);
    let _ = ElementHandle::find_by_element_type_name(&app, "SmartDownloads")
        .next()
        .expect("offline page mounted")
        .size();
    let point = LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle(&app);
    assert_eq!(
        sorts.get(),
        0,
        "covered download sort must not receive taps"
    );
    let weak = app.as_weak();
    offline.on_closed(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<nova::OfflineBridge>().set_opened(false);
        }
    });
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!app.global::<nova::OfflineBridge>().get_opened());
    assert!(app.get_show_downloads());
    assert_eq!(
        app.global::<nova::OfflineBridge>().get_rows().row_count(),
        0
    );

    offline.set_rows(
        Rc::new(VecModel::from(vec![nova_ui_shared::SmartOfflineRow {
            id: "observatory".into(),
            title: "The Observatory — a longer series title".into(),
            status: "2 ready · 1 waiting for unmetered Wi-Fi".into(),
            enabled: true,
            source_ready: true,
            keep_count: 3,
            cleanup: true,
            ..Default::default()
        }]))
        .into(),
    );
    offline.set_opened(true);
    for (width, tv) in [(360., false), (1280., false), (1920., true)] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window().set_size(slint::LogicalSize::new(width, 1080.));
        settle(&app);
        let card = ElementHandle::find_by_element_id(&app, "SmartDownloads::rule_card")
            .next()
            .expect("populated offline rule card");
        let origin = card.absolute_position();
        let card_size = card.size();
        let labels = card.query_descendants().match_type_name("Text").find_all();
        assert!(
            labels.len() >= 10,
            "measure all rule actions and supporting text"
        );
        for text in labels {
            let position = text.absolute_position();
            let size = text.size();
            assert!(
                position.x >= origin.x + 11.5
                    && position.x + size.width <= origin.x + card_size.width - 11.5,
                "offline rule text stays within horizontal card padding at {width}px"
            );
            assert!(
                position.y >= origin.y + 11.5
                    && position.y + size.height <= origin.y + card_size.height - 11.5,
                "offline rule action stays above card bottom padding at {width}px: {position:?} {size:?}, card {origin:?} {card_size:?}"
            );
        }
        let status = ElementHandle::find_by_element_id(&app, "SmartDownloads::rule_status")
            .next()
            .expect("offline rule status");
        assert!(
            status.size().height >= if tv { 20. } else { 14. },
            "supporting status uses readable phone/TV typography"
        );
    }
}

#[test]
fn smart_downloads_keeps_short_content_compact_at_phone_and_tv_sizes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::smart_downloads_keeps_short_content_compact_at_phone_and_tv_sizes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_downloads(true);
    let offline = app.global::<nova::OfflineBridge>();
    offline.set_storage_summary(
        "Automatic storage: 0.00 / 5 GiB · Reserved: 0.00 GiB · Manually kept: 0.00 GiB".into(),
    );
    offline.set_profiles(
        Rc::new(VecModel::from(vec![nova_ui_shared::OfflineDeviceProfile {
            id: "default".into(),
            name: "Default".into(),
            enabled: false,
        }]))
        .into(),
    );
    offline.set_opened(true);
    app.window().set_size(slint::LogicalSize::new(390., 1080.));
    app.show().unwrap();
    settle(&app);
    let item = |id: &str| {
        ElementHandle::find_by_element_id(&app, &format!("SmartDownloads::{id}"))
            .next()
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    // Keep the handles across resizes so short landscape viewports can still
    // measure rows below the clip without requiring them to be visible.
    let intro = item("introduction");
    let storage = item("storage");
    let device =
        ElementHandle::find_by_accessible_label(&app, "Enable this profile on this device")
            .next()
            .unwrap();
    let profile_heading = item("profile_heading");
    let profile =
        ElementHandle::find_by_accessible_label(&app, "Default · Disabled on this device")
            .next()
            .unwrap();
    let empty = item("empty_notice");
    let mut phone_content_bottom: Option<f32> = None;
    for (width, height, tv) in [
        (360., 800., false),
        (390., 800., false),
        (390., 1080., false),
        (800., 390., false),
        (1280., 1080., false),
        (1920., 1080., true),
    ] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window()
            .set_size(slint::LogicalSize::new(width, height));
        settle(&app);
        for (above, below) in [
            (&intro, &storage),
            (&storage, &device),
            (&profile_heading, &profile),
            (&profile, &empty),
        ] {
            let gap =
                below.absolute_position().y - above.absolute_position().y - above.size().height;
            assert!(
                (gap - 12.).abs() < 0.5,
                "rows retain their 12px gap at {width}x{height}, tv={tv}: {gap}"
            );
        }
        assert!(
            intro.size().height < if tv { 90. } else { 80. },
            "wrapped supporting text keeps its natural height at {width}x{height}"
        );
        if width == 390. {
            let bottom = empty.absolute_position().y + empty.size().height;
            if let Some(previous) = phone_content_bottom {
                assert!(
                    (bottom - previous).abs() < 0.5,
                    "a taller phone must not stretch the short content: {previous} -> {bottom}"
                );
            }
            phone_content_bottom = Some(bottom);
        }
    }
}

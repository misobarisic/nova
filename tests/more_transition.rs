//! More destinations animate into and back out of the page on narrow layouts.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, platform::WindowEvent};
use std::time::Duration;

fn page(app: &nova::AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_element_type_name(app, name)
        .next()
        .unwrap_or_else(|| panic!("missing page {name}"))
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    let mut matches = ElementHandle::find_by_accessible_label(app, label)
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Button));
    // During return the inactive More header is below the outgoing header.
    let found = if label == "Back" {
        matches.last()
    } else {
        matches.next()
    };
    found.unwrap_or_else(|| panic!("missing button {label}"))
}

fn assert_shared_back(app: &nova::AppWindow) {
    let back = button(app, "Back");
    let size = back.size();
    assert!(
        (size.width - 44.0).abs() < 0.1,
        "Back target width: {}",
        size.width
    );
    assert!(
        (size.height - 44.0).abs() < 0.1,
        "Back target height: {}",
        size.height
    );
}
fn assert_settings_header_spacing(
    app: &nova::AppWindow,
    title_label: &str,
    subtitle_label: &str,
    language: &str,
) {
    let in_header = |element: &ElementHandle| {
        let position = element.absolute_position();
        let size = element.size();
        position.y >= 12.0 && position.y < 110.0 && size.width > 100.0 && size.height > 0.0
    };
    let title = ElementHandle::find_by_accessible_label(app, title_label)
        .find(&in_header)
        .unwrap_or_else(|| panic!("{language}: missing Settings overview title"));
    let subtitle = ElementHandle::find_by_accessible_label(app, subtitle_label)
        .find(&in_header)
        .unwrap_or_else(|| panic!("{language}: missing Settings overview subtitle"));
    let title_bottom = title.absolute_position().y + title.size().height;
    let subtitle_position = subtitle.absolute_position();
    let subtitle_size = subtitle.size();
    assert!(
        subtitle_position.y >= title_bottom + 7.0,
        "{language}: Settings subtitle must clear title by 7px (title bottom {title_bottom:.1}, subtitle y {:.1})",
        subtitle_position.y
    );
    assert!(
        subtitle_position.x >= -0.5 && subtitle_position.x + subtitle_size.width <= 320.5,
        "{language}: Settings subtitle must fit the 320px viewport ({:.1}..{:.1})",
        subtitle_position.x,
        subtitle_position.x + subtitle_size.width
    );
    let search = ElementHandle::find_by_element_id(app, "SettingsPage::settings_search")
        .next()
        .expect("Settings landing search field");
    assert!(
        subtitle_position.y + subtitle_size.height + 7.0 <= search.absolute_position().y,
        "{language}: wrapped Settings subtitle must clear search by 7px"
    );
}
fn tap(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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

#[test]
fn more_subpages_slide_fade_and_accept_back_during_entry() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        for (label, component) in [
            ("Settings", "SettingsPage"),
            ("About", "AboutPage"),
            ("Calendar", "CalendarPage"),
            ("Downloads", "DownloadsPage"),
        ] {
            let app = nova::AppWindow::new().unwrap();
            app.set_is_android(true);
            app.set_touch_menus(true);
            app.window().set_size(slint::PhysicalSize::new(320, 800));
            app.global::<nova::Anim>().set_enabled(enabled);
            app.global::<nova::Anim>().set_transitions(transitions);
            app.on_settings_search_matches(|_, _| true);
            let weak = app.as_weak();
            app.on_settings_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_about(false);
                a.set_show_settings(true);
            });
            let weak = app.as_weak();
            app.on_about_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_settings(false);
                a.set_show_about(true);
            });
            let weak = app.as_weak();
            app.on_calendar_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_calendar(true);
            });
            let weak = app.as_weak();
            app.on_downloads_picked(move || {
                let a = weak.upgrade().unwrap();
                a.set_show_home(false);
                a.set_show_downloads(true);
            });
            app.show().unwrap();
            app.set_more_open(true);
            let _ = page(&app, "MorePage").size();
            assert_shared_back(&app);
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            tap(&app, button(&app, label));
            let start = page(&app, component);
            let x = start.absolute_position().x;
            let opacity = start.computed_opacity();
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
                usize::from(label == "Calendar" || label == "Downloads")
            );
            if enabled && transitions {
                assert!(x > 0.0 && opacity < 0.01);
            } else {
                assert!(x.abs() < 0.1 && opacity > 0.99);
            }
            let mut slide = false;
            let mut fade = false;
            for _ in 0..35 {
                let p = page(&app, component);
                let current = p.absolute_position().x;
                let alpha = p.computed_opacity();
                slide |= current > 0.1 && current < x - 0.1;
                fade |= alpha > 0.01 && alpha < 0.99;
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            }
            let p = page(&app, component);
            assert!(p.absolute_position().x.abs() < 0.1 && p.computed_opacity() > 0.99);
            assert_shared_back(&app);
            if label == "Settings" {
                let catalogs = slint::select_bundled_translation("en").is_ok();
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
                assert_settings_header_spacing(&app, "Settings", "Make Nova yours", "English");
                if catalogs {
                    let _ = slint::select_bundled_translation("hr");
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
                    assert_settings_header_spacing(
                        &app,
                        "Postavke",
                        "Prilagodi Novu sebi",
                        "Croatian",
                    );
                    let _ = slint::select_bundled_translation("en");
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
                }
            }
            if enabled && transitions {
                assert!(slide && fade);
            }
            // Return to More, then press system Back during the next entry.
            tap(&app, button(&app, "Back"));
            if enabled && transitions {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(280));
            }
            let _ = page(&app, "MorePage").size();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            tap(&app, button(&app, label));
            let _ = page(&app, component).size();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            let entry_x = page(&app, component).absolute_position().x;
            let entry_alpha = page(&app, component).computed_opacity();
            for event in [
                WindowEvent::KeyPressed {
                    text: slint::platform::Key::Back.into(),
                },
                WindowEvent::KeyPressRepeated {
                    text: slint::platform::Key::Back.into(),
                },
                WindowEvent::KeyReleased {
                    text: slint::platform::Key::Back.into(),
                },
            ] {
                assert!(matches!(
                    app.window().dispatch_event_with_result(event).unwrap(),
                    slint::platform::WindowEventDispatchResult::Accepted
                ));
            }
            if enabled && transitions {
                assert!(!app.get_more_open());
                let reversing = page(&app, component);
                assert!(reversing.size().width > 0.0);
                assert!((reversing.absolute_position().x - entry_x).abs() < 0.5);
                assert!(
                    (reversing.computed_opacity() - entry_alpha).abs() < 0.01,
                    "Back during entry must not flash a fully opaque subpage"
                );
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(110));
                let outgoing = page(&app, component);
                assert!(outgoing.absolute_position().x > 0.1);
                assert!(outgoing.computed_opacity() < 0.99);
                let incoming = page(&app, "MorePage");
                assert!(incoming.computed_opacity() > 0.01 && incoming.computed_opacity() < 0.99);
                assert!(incoming.absolute_position().x < -0.1);
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(180));
            } else {
                assert!(app.get_more_open());
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, component).count(),
                    0
                );
            }
            assert!(app.get_more_open());
            assert!(
                !app.get_show_settings()
                    && !app.get_show_about()
                    && !app.get_show_calendar()
                    && !app.get_show_downloads()
            );

            // A settled page also slides/fades out on the ordinary header or
            // navbar Back path; disabled motion returns synchronously.
            tap(&app, button(&app, label));
            for _ in 0..35 {
                let _ = page(&app, component).absolute_position();
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            }
            let settled = page(&app, component);
            assert!(settled.absolute_position().x.abs() < 0.1);
            assert!(settled.computed_opacity() > 0.99);
            tap(&app, button(&app, "Back"));
            if enabled && transitions {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(110));
                let outgoing = page(&app, component);
                assert!(outgoing.absolute_position().x > 0.1);
                assert!(outgoing.computed_opacity() < 0.99);
                let incoming = page(&app, "MorePage");
                assert!(incoming.computed_opacity() > 0.01 && incoming.computed_opacity() < 0.99);
                assert!(incoming.absolute_position().x < -0.1);
                let mut previous_x = outgoing.absolute_position().x;
                let mut previous_alpha = incoming.computed_opacity();
                assert!((outgoing.computed_opacity() + previous_alpha - 1.0).abs() < 0.01);
                // A second Back must not restart the return timer.
                tap(&app, button(&app, "Back"));
                for _ in 0..8 {
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
                    let outgoing = page(&app, component);
                    let incoming = page(&app, "MorePage");
                    let x = outgoing.absolute_position().x;
                    let alpha = incoming.computed_opacity();
                    assert!(x >= previous_x && alpha >= previous_alpha);
                    assert!(
                        (outgoing.computed_opacity() + alpha - 1.0).abs() < 0.01,
                        "return must not have a blank frame before More appears"
                    );
                    previous_x = x;
                    previous_alpha = alpha;
                }
                assert!(
                    previous_alpha > 0.98,
                    "More should be revealed before teardown"
                );
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(42));
            }
            assert!(app.get_more_open());
            assert_eq!(
                ElementHandle::find_by_element_type_name(&app, component).count(),
                0
            );
        }
    }
    more_return_cancels_for_destination_navigation();
    more_return_animates_wide_and_motion_toggle_finishes_pending_return();
}

fn more_return_cancels_for_destination_navigation() {
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(true);
    app.window().set_size(slint::PhysicalSize::new(320, 800));
    let weak = app.as_weak();
    app.on_calendar_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_home(false);
        app.set_show_calendar(true);
    });
    let weak = app.as_weak();
    app.on_library_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_calendar(false);
        app.set_show_downloads(false);
        app.set_show_library(true);
    });
    app.show().unwrap();
    app.set_more_open(true);
    let _ = page(&app, "MorePage").size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    tap(&app, button(&app, "Calendar"));
    for _ in 0..35 {
        let _ = page(&app, "CalendarPage").absolute_position();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
    tap(&app, button(&app, "Back"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    assert!(!app.get_more_open());
    let library = ElementHandle::find_by_element_id(&app, "BottomNav::tab_library")
        .last()
        .expect("the outgoing page's Library tab");
    tap(&app, library);
    assert!(app.get_show_library());
    assert!(!app.get_show_calendar() && !app.get_more_open());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "MorePage").count(),
        0
    );
    assert!(app.get_show_library());
}

fn more_return_animates_wide_and_motion_toggle_finishes_pending_return() {
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1200, 800));
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    let weak = app.as_weak();
    app.on_calendar_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_home(false);
        app.set_show_calendar(true);
    });
    app.show().unwrap();
    app.set_more_open(true);
    let _ = page(&app, "MorePage").size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    tap(&app, button(&app, "Calendar"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(page(&app, "CalendarPage").absolute_position().x.abs() < 0.1);
    tap(&app, button(&app, "Back"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    let outgoing = page(&app, "CalendarPage");
    assert!(outgoing.absolute_position().x > 0.1);
    assert!(outgoing.computed_opacity() < 0.99);
    assert!(!app.get_more_open());
    app.global::<nova::Anim>().set_transitions(false);
    // Slint delivers changed bindings on the next event-loop turn, without
    // waiting out the original 260ms return timer.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    assert!(app.get_more_open());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "CalendarPage").count(),
        0
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(app.get_more_open());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "CalendarPage").count(),
        0
    );
}

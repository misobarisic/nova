//! More destinations share content push transitions and stationary navigation chrome.
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

// Calendar is a More link on phones; wide layouts expose it in the rail.
// Enter through the reachable narrow link before checking retained transitions
// and stationary chrome at the caller's original desktop size.
fn calendar_from_more(app: &nova::AppWindow) {
    let size = app.window().size();
    if size.width >= 700 {
        app.window()
            .set_size(slint::PhysicalSize::new(699, size.height));
        let _ = page(app, "MorePage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
    tap(app, button(app, "Calendar"));
    if size.width >= 700 {
        app.window().set_size(size);
        let _ = page(app, "CalendarPage").size();
    }
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
                0
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
            let incoming = page(&app, component);
            let _ = incoming.size();
            // The entry timer starts motion on the next UI tick. Render that
            // first animated frame before advancing time within the animation;
            // a single mock-time jump otherwise starts it at the jump's end.
            let _ = incoming.computed_opacity();
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
            let _ = (incoming.absolute_position(), incoming.computed_opacity());
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            let entry_x = page(&app, component).absolute_position().x;
            let entry_alpha = page(&app, component).computed_opacity();
            if enabled && transitions {
                assert!(
                    entry_alpha > 0.01 && entry_alpha < 0.99,
                    "{component}: Back must interrupt a visible entry frame, alpha={entry_alpha}, x={entry_x}"
                );
            }
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
                    "{component}: Back during entry must preserve opacity ({entry_alpha} before, {} after)",
                    reversing.computed_opacity()
                );
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(110));
                let outgoing = page(&app, component);
                assert!(outgoing.absolute_position().x > 0.1);
                assert!(outgoing.computed_opacity() < 0.99);
                let incoming = page(&app, "MorePage");
                let incoming_alpha = incoming.computed_opacity();
                assert!(
                    incoming_alpha > 0.01 && incoming_alpha < 0.99,
                    "{component}: reverse More alpha={incoming_alpha}, entry alpha={entry_alpha}, outgoing alpha={}, entry x={entry_x}, outgoing x={}, More open={}",
                    outgoing.computed_opacity(),
                    outgoing.absolute_position().x,
                    app.get_more_open()
                );
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
                let incoming_alpha = incoming.computed_opacity();
                assert!(
                    incoming_alpha > 0.01 && incoming_alpha < 0.99,
                    "{component}: reverse More alpha={incoming_alpha}, entry alpha={entry_alpha}, outgoing alpha={}, entry x={entry_x}, outgoing x={}, More open={}",
                    outgoing.computed_opacity(),
                    outgoing.absolute_position().x,
                    app.get_more_open()
                );
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
    canonical_direct_routes_and_modal_interruptions();
    more_state_survives_children_and_breakpoints();
    settings_navigation_has_six_desktop_targets();
    pending_return_blocks_scroll_and_editor_input();
}

fn more_return_cancels_for_destination_navigation() {
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(false);
    app.window().set_size(slint::PhysicalSize::new(900, 800));
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
    calendar_from_more(&app);
    for _ in 0..35 {
        let _ = page(&app, "CalendarPage").absolute_position();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
    tap(&app, button(&app, "Back"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    assert!(!app.get_more_open());
    let library = ElementHandle::find_by_element_type_name(&app, "SideItem")
        .nth(2)
        .expect("the stationary Library tab");
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
    calendar_from_more(&app);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(page(&app, "CalendarPage").absolute_position().x > 64.0);
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SideNav").count(),
        1
    );
    app.set_system_back_request(app.get_system_back_request() + 1);
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

fn canonical_direct_routes_and_modal_interruptions() {
    for width in [699, 700, 1100] {
        let app = nova::AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(width, 800));
        let weak = app.as_weak();
        app.global::<nova::NavState>()
            .on_calendar_picked(move || weak.upgrade().unwrap().invoke_navigate_section(4));
        let weak = app.as_weak();
        app.global::<nova::NavState>()
            .on_downloads_picked(move || weak.upgrade().unwrap().invoke_navigate_section(5));
        app.show().unwrap();
        app.set_more_open(true);
        let _ = page(&app, "MorePage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        calendar_from_more(&app);
        let initial = page(&app, "CalendarPage");
        let initial_size = initial.size();
        let rail_position = if width >= 700 {
            Some(page(&app, "SideNav").absolute_position())
        } else {
            None
        };
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
            0
        );
        for _ in 0..20 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            assert_eq!(page(&app, "CalendarPage").size(), initial_size);
            if let Some(position) = rail_position {
                assert_eq!(page(&app, "SideNav").absolute_position(), position);
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, "SideNav").count(),
                    1
                );
            }
        }
        tap(&app, button(&app, "Back"));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(64));
        // Exercise exactly the global callback production mouse and keyboard use.
        app.global::<nova::NavState>().invoke_downloads_picked();
        assert!(app.get_show_downloads());
        assert!(!app.get_more_open() && !app.get_show_calendar());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        assert!(
            !app.get_more_open(),
            "stale return must not replace Downloads"
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "MorePage").count(),
            0
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
            0
        );
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        assert!(
            app.get_show_home(),
            "direct Calendar/Downloads Back returns Home"
        );
        app.invoke_navigate_section(3);
        let _ = page(&app, "MorePage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        calendar_from_more(&app);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
        app.set_modal_visible(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        assert!(
            !app.get_more_open(),
            "modal invalidates pending More return"
        );
        app.set_modal_visible(false);
        assert!(app.get_show_calendar());
        // Dismissing Detail recreates Calendar's factory product. Publish its
        // tree before sending a new Back; entry init seeds the request counter
        // to ignore requests from the page that was just dismissed.
        let _ = page(&app, "CalendarPage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        app.set_system_back_request(app.get_system_back_request() + 1);
        // Deliver the changed request on a UI tick before timing the return.
        // A single jump would begin the animation at the end of that jump.
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let returning = page(&app, "CalendarPage");
        let _ = (returning.absolute_position(), returning.computed_opacity());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        assert!(
            app.get_more_open(),
            "Detail close retains the More Back origin"
        );
        app.invoke_navigate_section(3);
        let _ = page(&app, "MorePage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        calendar_from_more(&app);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
        app.set_player_open(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        assert!(
            !app.get_more_open(),
            "player invalidates pending More return"
        );
        app.set_player_open(false);
        // Publish the restored destination, then deliver and render the Back
        // request before measuring its full return duration, as for Detail.
        let _ = page(&app, "CalendarPage").size();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        app.set_system_back_request(app.get_system_back_request() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let returning = page(&app, "CalendarPage");
        let _ = (returning.absolute_position(), returning.computed_opacity());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        assert!(
            app.get_more_open(),
            "player close retains the More Back origin"
        );
    }
}

fn more_state_survives_children_and_breakpoints() {
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(360, 360));
    app.set_more_choice(3);
    app.set_more_scroll_y(-120.0);
    app.set_more_open(true);
    app.show().unwrap();
    let _ = page(&app, "MorePage").size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    let scroll = app.get_more_scroll_y();
    assert!(scroll < 0.0, "short More viewport must restore scroll");
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Return.into(),
    });
    assert!(app.get_show_about());
    assert!(app.get_kb_active());
    app.set_system_back_request(app.get_system_back_request() + 1);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(app.get_more_open());
    assert_eq!(app.get_more_choice(), 3);
    assert!((app.get_more_scroll_y() - scroll).abs() < 0.5);
    assert!(
        app.get_kb_active(),
        "keyboard return retains its choice highlight"
    );
    app.set_kb_active(false);
    for width in [700, 699, 700] {
        app.window().set_size(slint::PhysicalSize::new(width, 360));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "SideNav").count(),
            usize::from(width >= 700)
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
            usize::from(width < 700)
        );
    }
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    tap(&app, button(&app, "About"));
    app.set_system_back_request(app.get_system_back_request() + 1);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    assert!(app.get_more_open());
    assert!(
        !app.get_kb_active(),
        "Android Back must not invent a keyboard highlight"
    );
}

fn settings_navigation_has_six_desktop_targets() {
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(900, 800));
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|_, _| true);
    app.show().unwrap();
    app.invoke_navigate_section(6);
    let _ = page(&app, "SettingsPage").size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    // Factory publication can schedule the guarded focus handoff on another
    // UI tick. Let that frame finish before delivering native navigation keys.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    let native_focus = i_slint_core::window::WindowInner::from_pub(app.window())
        .focus_item
        .borrow()
        .upgrade()
        .map(|item| {
            let mut name = slint::SharedString::default();
            slint::private_unstable_api::re_exports::VRc::borrow_pin(item.item_tree())
                .as_ref()
                .item_element_infos(item.index(), &mut name);
            name
        });
    assert!(
        native_focus
            .as_ref()
            .is_some_and(|name| name.contains("SettingsPage::nav-scope")),
        "Settings must own native keyboard focus after publication: {native_focus:?}"
    );
    let navigation = app.global::<nova::SettingsBridge>();
    assert_eq!(navigation.get_kb_zone(), 0);
    assert_eq!(navigation.get_kb_nav(), 3);
    for expected in [0, 1, 2, 4, 5, 3] {
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::RightArrow.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: slint::platform::Key::RightArrow.into(),
        });
        assert_eq!(
            navigation.get_kb_nav(),
            expected,
            "desktop keyboard cursor follows the sidebar order"
        );
    }
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Return.into(),
    });
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Return.into(),
    });
    assert!(
        app.get_more_open(),
        "six arrow presses return the desktop cursor to More"
    );
}

// Use real native input and Flickable dispatch: disabled page shortcut scopes
// alone do not disable sibling editors, and an empty click shield lets wheel
// events fall through to the outgoing page.
fn pending_return_blocks_scroll_and_editor_input() {
    let app = nova::AppWindow::new().unwrap();
    app.set_is_android(true);
    app.window().set_size(slint::PhysicalSize::new(360, 520));
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    app.on_settings_search_matches(|_, _| true);
    app.set_more_open(true);
    app.show().unwrap();
    let _ = page(&app, "MorePage").size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    tap(&app, button(&app, "Settings"));
    for _ in 0..35 {
        let _ = page(&app, "SettingsPage").absolute_position();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }

    let scroll_position = slint::LogicalPosition::new(200.0, 300.0);
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: scroll_position,
        delta_x: 0.0,
        delta_y: -200.0,
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let _ = page(&app, "SettingsPage").absolute_position();
    assert!(
        app.get_settings_navigation_scroll() < 0.0,
        "the active Settings landing must be scrollable for this regression"
    );
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: scroll_position,
        delta_x: 0.0,
        delta_y: 10_000.0,
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let _ = page(&app, "SettingsPage").absolute_position();
    let search = ElementHandle::find_by_element_id(&app, "SettingsPage::settings_search")
        .next()
        .unwrap();
    tap(&app, search);
    for event in [
        WindowEvent::KeyPressed { text: "q".into() },
        WindowEvent::KeyReleased { text: "q".into() },
    ] {
        app.window().dispatch_event(event);
    }
    assert_eq!(app.get_settings_search_query().as_str(), "q");
    app.set_settings_search_query("".into());

    // The canonical More route begins return even while the search owns focus.
    app.invoke_navigate_section(3);
    assert!(!app.get_more_open() && app.get_show_settings());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
        1,
        "stationary chrome must be available throughout return"
    );
    let settings_scroll = app.get_settings_navigation_scroll();
    let more_scroll = app.get_more_scroll_y();
    let selected = app.get_settings_selected_id();
    let choice = app.get_more_choice();
    for (delta_x, delta_y) in [(0.0, -200.0), (70.0, -40.0), (0.0, 200.0)] {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position: scroll_position,
            delta_x,
            delta_y,
        });
    }
    // Wheel dispatch uses smooth scrolling, so advance a frame before checking.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    let _ = page(&app, "SettingsPage").absolute_position();
    assert_eq!(app.get_settings_navigation_scroll(), settings_scroll);
    assert_eq!(app.get_more_scroll_y(), more_scroll);
    // Tab can reach a native editor despite the outgoing page's disabled
    // shortcut scope. The ancestor capture must consume subsequent edits and
    // activation before either the editor or an inactive More link handles them.
    for _ in 0..20 {
        for text in [
            slint::SharedString::from(slint::platform::Key::Tab),
            "x".into(),
            slint::platform::Key::Return.into(),
        ] {
            app.window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        }
    }
    assert_eq!(app.get_settings_search_query().as_str(), "");
    assert_eq!(app.get_settings_selected_id(), selected);
    assert!(!app.get_settings_detail_open());
    assert_eq!(app.get_more_choice(), choice);
    assert!(app.get_show_settings() && !app.get_more_open());

    let library = ElementHandle::find_by_element_id(&app, "BottomNav::tab_library")
        .next()
        .expect("stationary Library tab");
    tap(&app, library);
    assert!(app.get_show_library() && !app.get_show_settings());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(app.get_show_library() && !app.get_more_open());
}

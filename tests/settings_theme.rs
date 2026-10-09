//! Theme is searchable and its toggle applies without remounting.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..20 {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.size(), e.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn key(app: &nova::AppWindow, text: slint::SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}
#[test]
fn theme_search_toggle_resize_and_android_back() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_touch_menus(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.on_settings_edited(move |field| {
        recorded.borrow_mut().push(field.to_string());
        let app = weak.upgrade().unwrap();
        nova_ui::apply_theme(&app, app.get_true_black());
    });
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    app.set_settings_search_query("AMOLED".into());
    settle(&app);
    let link = ElementQuery::from_root(&app)
        .match_predicate(|e| e.accessible_id().as_deref() == Some("settings:theme"))
        .find_all()
        .into_iter()
        .next()
        .expect("search finds Theme");
    link.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(app.get_settings_selected_id(), 14);
    assert!(app.get_settings_detail_open());
    let toggle = ElementHandle::find_by_accessible_label(&app, "True black")
        .last()
        .unwrap();
    let p = toggle.absolute_position();
    assert!(p.x >= 0.0 && p.x + toggle.size().width <= 390.0);
    toggle.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert!(app.get_true_black());
    assert!(app.global::<nova::Theme>().get_true_black());
    assert_eq!(edits.borrow().as_slice(), ["true_black"]);
    let glow = ElementHandle::find_by_element_id(&app, "SettingsPage::settings_backdrop").next();
    assert!(glow.is_none(), "opaque blue header artwork must be hidden");
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    settle(&app);
    assert_eq!(app.get_settings_selected_id(), 14);
    let toggle = ElementHandle::find_by_accessible_label(&app, "True black")
        .last()
        .unwrap();
    let p = toggle.absolute_position();
    assert!(p.x >= 0.0 && p.x + toggle.size().width <= 1280.0);
    // The same mounted row remains reachable by the Settings keyboard scope.
    key(&app, slint::platform::Key::Return.into());
    assert!(!app.get_true_black());
    assert!(!app.global::<nova::Theme>().get_true_black());
    // Wide Settings uses the extra width for its detail pane, rather than
    // leaving a capped column beside unused space. Sticky controls still mask
    // rows at a fixed size when the independent navigation pane scrolls.
    let host = ElementHandle::find_by_element_id(&app, "SettingsPage::content_host")
        .next()
        .unwrap();
    let initial_width = host.size().width;
    app.window().set_size(slint::PhysicalSize::new(2048, 900));
    settle(&app);
    let expanded_width = host.size().width;
    assert!((expanded_width - initial_width - 768.0).abs() < 1.0);
    let mask = ElementHandle::find_by_element_id(&app, "SettingsPage::landing_controls_canvas")
        .next()
        .unwrap();
    let initial_mask_y = mask.absolute_position().y;
    app.set_settings_navigation_scroll(-108.0);
    settle(&app);
    assert!((initial_mask_y - mask.absolute_position().y - 108.0).abs() < 1.0);
    assert!((mask.size().height - 124.0).abs() < 1.0);
    // Both preset controls are keyboard-reachable and feed the mounted
    // theme immediately. Zero must produce genuinely square cards/no gaps.
    key(&app, slint::platform::Key::DownArrow.into());
    for _ in 0..10 {
        key(&app, slint::platform::Key::LeftArrow.into());
    }
    assert_eq!(app.get_card_corner_radius(), 0.0);
    assert_eq!(app.global::<nova::Theme>().get_card_corner_radius(), 0.0);
    key(&app, slint::platform::Key::DownArrow.into());
    for _ in 0..8 {
        key(&app, slint::platform::Key::LeftArrow.into());
    }
    assert_eq!(app.get_card_spacing(), 0.0);
    assert_eq!(app.global::<nova::Theme>().get_card_spacing(), 0.0);
    assert!(edits.borrow().iter().any(|f| f == "card_corner_radius"));
    assert!(edits.borrow().iter().any(|f| f == "card_spacing"));

    // Reset covers the full Theme section and remains contained at phone
    // widths, even after a live resize. Keep an unrelated preference intact.
    app.set_animations(false);
    app.set_true_black(true);
    app.set_hero_title_alignment(2);
    nova_ui::apply_theme(&app, true);
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        settle(&app);
        // Theme has more rows than a phone viewport. Queries omit clipped
        // descendants, so reveal the reset before checking its bounds.
        let reset = (0..20)
            .find_map(|_| {
                let reset =
                    ElementHandle::find_by_accessible_label(&app, "Restore theme defaults").last();
                if reset.is_none() {
                    app.set_settings_scroll_y(app.get_settings_scroll_y() - 100.0);
                    settle(&app);
                }
                reset
            })
            .expect("theme reset must be reachable by scrolling");
        let p = reset.absolute_position();
        assert!(p.x >= 0.0 && p.x + reset.size().width <= width as f32);
    }
    // Hero title alignment follows the two backdrop rows before Reset.
    for _ in 0..5 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    key(&app, slint::platform::Key::Return.into());
    assert!(!app.get_true_black());
    assert!(!app.global::<nova::Theme>().get_true_black());
    assert_eq!(app.get_card_corner_radius(), 10.0);
    assert_eq!(app.get_card_spacing(), 8.0);
    assert_eq!(app.get_hero_title_alignment(), 1);
    assert_eq!(app.global::<nova::Theme>().get_card_corner_radius(), 10.0);
    assert_eq!(app.global::<nova::Theme>().get_card_spacing(), 8.0);
    assert_eq!(app.global::<nova::Theme>().get_hero_title_alignment(), 1);
    assert!(!app.get_animations());
    assert_eq!(
        &edits.borrow()[edits.borrow().len() - 7..],
        [
            "true_black",
            "card_corner_radius",
            "card_spacing",
            "status_bar_gradient",
            "home_backdrop_size",
            "detail_backdrop_size",
            "hero_title_alignment"
        ]
    );

    app.window().set_size(slint::PhysicalSize::new(390, 844));
    settle(&app);
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!app.get_settings_detail_open());
    assert_eq!(app.get_settings_search_query(), "AMOLED");
}

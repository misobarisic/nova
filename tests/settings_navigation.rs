//! Real Settings search, shortcuts, responsive navigation and disabled controls.
#![cfg(not(feature = "live-preview"))]
#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, Model};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let timer_ready = ready.clone();
    let timer_waker = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(350), move || {
        timer_ready.set(true);
        if let Some(waker) = timer_waker.borrow_mut().take() {
            waker.wake();
        }
    });
    std::future::poll_fn(|cx| {
        if ready.get() {
            std::task::Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
    .await;
}
fn back(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
}
fn links(app: &nova::AppWindow) -> Vec<String> {
    ElementHandle::find_by_element_type_name(app, "SettingsLink")
        .filter_map(|e| e.accessible_id().map(|s| s.to_string()))
        .collect()
}
async fn all_links(app: &nova::AppWindow) -> Vec<String> {
    let mut found = Vec::new();
    for _ in 0..14 {
        for id in links(app) {
            if !found.contains(&id) {
                found.push(id);
            }
        }
        key(app, slint::platform::Key::DownArrow);
        settle().await;
    }
    for _ in 0..14 {
        key(app, slint::platform::Key::UpArrow);
    }
    settle().await;
    found
}

async fn click(app: &nova::AppWindow, label: &str) {
    let candidates: Vec<_> = if label.starts_with("settings:") {
        destinations::find(app, label).collect()
    } else {
        ElementHandle::find_by_accessible_label(app, label).collect()
    };
    let element = candidates
        .into_iter()
        .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button {label}"));
    element
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
}

#[test]
fn settings_navigation_survives_search_back_breakpoints_and_recreation() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    let writes = Rc::new(Cell::new(0));
    let count = writes.clone();
    app.on_save_settings(move || count.set(count.get() + 1));
    let count = writes.clone();
    app.on_settings_edited(move |_| count.set(count.get() + 1));
    let weak = app.as_weak();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        settle().await;
        assert!(app.get_settings_two_pane());
        assert_eq!(
            app.get_settings_selected_id(),
            13,
            "first desktop entry selects Home"
        );
        assert!(app.get_settings_detail_open());
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        settle().await;
        assert!(!app.get_settings_two_pane());
        assert!(
            app.get_settings_detail_open(),
            "shrinking preserves detail selection"
        );
        back(&app);
        settle().await;
        assert!(!app.get_settings_detail_open());
        let destinations = vec![
            "home",
            "display",
            "theme",
            "look-and-feel",
            "player",
            "addons",
            "categories",
            "sync",
            "tracking",
            "image-cache",
            "downloads",
            "p2p",
            "about",
        ]
        .into_iter()
        .map(|id| format!("settings:{id}"))
        .collect::<Vec<_>>();
        assert_eq!(all_links(&app).await, destinations);
        click(&app, "Services").await;
        assert_eq!(
            app.get_settings_search_query(),
            "",
            "shortcuts jump without filtering"
        );
        assert!(app.get_settings_navigation_scroll() < 0.0);
        let link = destinations::find(&app, "settings:addons").next().unwrap();
        assert!(
            link.absolute_position().y >= 0.0
                && link.absolute_position().y + link.size().height <= 800.0
        );
        click(&app, "settings:addons").await;
        assert_eq!(app.get_settings_selected_id(), 0);
        app.set_addon_url("https://example.org/a-very-long-draft/manifest.json".into());
        app.window().set_size(slint::PhysicalSize::new(1100, 900));
        settle().await;
        assert!(app.get_settings_two_pane());
        assert_eq!(app.get_settings_selected_id(), 0);
        assert!(app.get_addon_url().contains("draft"));
        app.set_cache_busy(true);
        app.set_cache_rewriting(true);
        app.set_cache_processed(3);
        app.set_cache_total(10);
        app.set_show_settings(false);
        settle().await;
        app.set_show_settings(true);
        settle().await;
        assert_eq!(app.get_settings_selected_id(), 0);
        assert_eq!(
            app.get_cache_processed(),
            3,
            "job state survives page teardown"
        );
        assert_eq!(app.get_cache_total(), 10);
        assert!(app.get_cache_rewriting());
        app.set_cache_busy(false);
        app.set_cache_rewriting(false);
        app.set_settings_search_query("  jPeG   QUALITY ".into());
        settle().await;
        assert_eq!(links(&app), ["settings:image-cache"]);
        for query in ["Resume", "VLC", "1.25×"] {
            app.set_settings_search_query(query.into());
            settle().await;
            assert_eq!(links(&app), ["settings:player"], "option label {query}");
        }
        app.set_settings_search_query("licenses".into());
        settle().await;
        assert_eq!(links(&app), ["settings:about", "settings:licenses"]);
        click(&app, "settings:licenses").await;
        assert_eq!(app.get_settings_selected_id(), 11);
        app.window().set_size(slint::PhysicalSize::new(430, 800));
        settle().await;
        back(&app);
        settle().await;
        assert_eq!(
            app.get_settings_selected_id(),
            10,
            "nested Back returns to About"
        );
        back(&app);
        settle().await;
        assert!(!app.get_settings_detail_open());
        assert_eq!(
            app.get_settings_focused_id(),
            11,
            "navigation origin survives nested Back"
        );
        assert_eq!(app.get_settings_search_query(), "licenses");
        assert_eq!(links(&app), ["settings:about", "settings:licenses"]);
        app.set_settings_search_query("Downloaded episodes".into());
        settle().await;
        assert_eq!(
            links(&app),
            ["settings:downloads", "settings:downloaded-episodes"]
        );
        click(&app, "settings:downloaded-episodes").await;
        assert_eq!(app.get_settings_selected_id(), 9);
        back(&app);
        settle().await;
        assert_eq!(app.get_settings_selected_id(), 8);
        back(&app);
        settle().await;
        assert!(!app.get_settings_detail_open());
        assert_eq!(app.get_settings_focused_id(), 9);
        app.set_settings_search_query("no-such-setting".into());
        settle().await;
        assert!(links(&app).is_empty());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "No settings found.")
                .next()
                .is_some()
        );
        let services = ElementHandle::find_by_accessible_label(&app, "Services")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        assert_eq!(services.accessible_enabled(), Some(false));
        click(&app, "Clear search").await;
        assert!(links(&app).contains(&"settings:home".to_string()));
        slint::select_bundled_translation("hr").unwrap();
        app.set_settings_search_query("kvaliteta".into());
        settle().await;
        assert_eq!(links(&app), ["settings:image-cache"]);
        app.set_settings_search_query("quality".into());
        settle().await;
        assert_eq!(
            links(&app),
            ["settings:image-cache"],
            "English aliases also match Croatian UI"
        );
        slint::select_bundled_translation("en").unwrap();
        assert_eq!(
            writes.get(),
            0,
            "search and navigation must not edit settings"
        );
        app.set_settings_search_query("Image cache".into());
        settle().await;
        click(&app, "settings:image-cache").await;
        app.set_cache_images(false);
        app.set_cache_enabled(true);
        let compression = ElementHandle::find_by_accessible_label(&app, "Compress images")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Checkbox))
            .unwrap();
        assert_eq!(compression.accessible_enabled(), Some(false));
        compression
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        assert!(
            app.get_cache_enabled(),
            "disabled compression retains its saved value"
        );
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(
            app.get_cache_lru_mb(),
            144.0,
            "keyboard skips disabled compression and reaches memory"
        );
        assert_eq!(app.get_cache_quality(), 85.0);
        assert!(app.get_settings_page_scrolls().row_count() >= 14);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}

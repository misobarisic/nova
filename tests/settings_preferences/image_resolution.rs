//! Image-resolution settings remain reachable by touch and keyboard in both layouts.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..16 {
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

fn dropdown(app: &nova::AppWindow, title: &str) -> ElementHandle {
    let title = title.to_owned();
    let row = ElementHandle::find_by_element_type_name(app, "SettingsRow")
        .find(|row| {
            row.query_descendants()
                .match_predicate({
                    let title = title.clone();
                    move |element| element.accessible_label().as_deref() == Some(title.as_str())
                })
                .find_first()
                .is_some()
        })
        .unwrap_or_else(|| panic!("missing image resolution row {title}"));
    row.query_descendants()
        .match_inherits("Dropdown")
        .find_first()
        .unwrap_or_else(|| panic!("missing dropdown for {title}"))
}

#[test]
fn image_resolution_dropdowns_fit_and_capture_touch_keyboard_changes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "image_resolution_dropdowns_fit_and_capture_touch_keyboard_changes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(2);
    app.set_settings_detail_open(true);
    app.set_touch_menus(true);
    app.set_cache_images(false);
    app.set_cache_enabled(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edited = Rc::new(RefCell::new(Vec::new()));
    let edited_copy = edited.clone();
    app.on_settings_edited(move |field| edited_copy.borrow_mut().push(field.to_string()));
    app.window().show().unwrap();

    let rows = [
        ("Detail posters", "detail_poster_max_side", 0),
        ("Detail backdrops", "detail_backdrop_max_side", 1),
        ("Episode thumbnails", "episode_thumbnail_max_side", 2),
        (
            "Player episode backdrops",
            "player_episode_backdrop_max_side",
            3,
        ),
    ];
    for width in [360, 1280] {
        // Keep all resolution rows mounted while checking width; the Storage
        // section now has a disk-limit row above them.
        app.window().set_size(slint::PhysicalSize::new(width, 1800));
        app.set_settings_scroll_y(0.0);
        settle(&app);
        for (title, _, _) in rows {
            let control = dropdown(&app, title);
            let position = control.absolute_position();
            let size = control.size();
            assert!(position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5);
            assert!(
                size.height >= 44.0,
                "touch dropdown should keep a 44px target"
            );
            // The pointer selection below proves these controls stay enabled
            // even while both disk caching and compression are off.
        }

        // Open each actual dropdown and choose a preset by touch. Its selected
        // callback also establishes the corresponding keyboard row; a Right
        // press then advances one preset through the page's keyboard handler.
        for (title, field, row_index) in rows {
            let y = dropdown(&app, title).absolute_position().y;
            app.set_settings_scroll_y((app.get_settings_scroll_y() - (y - 250.0)).min(0.0));
            settle(&app);
            dropdown(&app, title).mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            let option = ElementHandle::find_by_accessible_label(&app, "720 px")
                .last()
                .expect("720 px popup option");
            option.mock_single_click(slint::platform::PointerEventButton::Left);
            settle(&app);
            let selected = [
                app.get_detail_poster_resolution_index(),
                app.get_detail_backdrop_resolution_index(),
                app.get_episode_thumbnail_resolution_index(),
                app.get_player_episode_backdrop_resolution_index(),
            ][row_index];
            assert_eq!(selected, 4);
            key(&app, slint::platform::Key::RightArrow);
            let advanced = [
                app.get_detail_poster_resolution_index(),
                app.get_detail_backdrop_resolution_index(),
                app.get_episode_thumbnail_resolution_index(),
                app.get_player_episode_backdrop_resolution_index(),
            ][row_index];
            assert_eq!(advanced, 5, "keyboard changes {title}");
            assert!(edited.borrow().iter().any(|seen| seen == field));
        }
    }
}

#[test]
fn disk_limit_dropdown_supports_touch_keyboard_and_narrow_landscape() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "disk_limit_dropdown_supports_touch_keyboard_and_narrow_landscape"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(2);
    app.set_settings_detail_open(true);
    app.set_touch_menus(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    let edited = Rc::new(RefCell::new(Vec::new()));
    let capture = edited.clone();
    app.on_settings_edited(move |field| capture.borrow_mut().push(field.to_string()));
    app.window().show().unwrap();
    for (width, height) in [(390, 900), (820, 400), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_settings_scroll_y(0.0);
        app.set_cache_images(true);
        settle(&app);
        let y = dropdown(&app, "Image disk-cache limit")
            .absolute_position()
            .y;
        app.set_settings_scroll_y((app.get_settings_scroll_y() - (y - 150.0)).min(0.0));
        settle(&app);
        let control = dropdown(&app, "Image disk-cache limit");
        let position = control.absolute_position();
        let size = control.size();
        assert!(position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5);
        assert!(position.y >= 0.0 && position.y + size.height <= height as f32);
        control.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        ElementHandle::find_by_accessible_label(&app, "2 GiB")
            .last()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(app.get_cache_disk_limit_index(), 4);
        key(&app, slint::platform::Key::RightArrow);
        assert_eq!(app.get_cache_disk_limit_index(), 5);
        assert!(
            edited
                .borrow()
                .iter()
                .any(|field| field == "image_disk_cache_limit_mb")
        );
        key(&app, slint::platform::Key::UpArrow);
        key(&app, slint::platform::Key::RightArrow);
        assert!(
            !app.get_cache_images(),
            "Up from the limit reaches disk caching"
        );
        if width == 820 {
            key(&app, slint::platform::Key::DownArrow);
            // Disk-off traversal skips Compression and Re-encode, ending on
            // Clear. Its Down key must not scroll past the final control.
            for _ in 0..5 {
                key(&app, slint::platform::Key::DownArrow);
            }
            app.set_settings_scroll_y(-100.0);
            settle(&app);
            let scroll = app.get_settings_scroll_y();
            key(&app, slint::platform::Key::DownArrow);
            assert_eq!(app.get_settings_scroll_y(), scroll);
        }
    }
}

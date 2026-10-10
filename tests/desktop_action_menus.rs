//! Desktop action menus stay native at both responsive layout widths.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn click(app: &nova::AppWindow, item: ElementHandle) {
    let p = item.absolute_position();
    let s = item.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
    settle();
}

fn s(value: &str) -> SharedString {
    SharedString::from(value)
}

fn stream_row(id: &str, text: &str, details: &str, is_download: bool) -> nova::StreamRow {
    nova::StreamRow {
        id: s(id),
        text: s(text),
        details: s(details),
        lines: 1,
        is_download,
        ..Default::default()
    }
}

#[test]
fn desktop_action_menus_use_popups_at_phone_and_desktop_widths() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    desktop_library_options_never_open_a_bottom_sheet();
    desktop_stream_right_click_prepares_actions_without_opening_a_sheet_or_playing();
}

fn desktop_library_options_never_open_a_bottom_sheet() {
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_in_library(true);
    app.set_selected_title("Saved movie".into());
    app.window().show().unwrap();
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded_actions = actions.clone();
    app.on_detail_library_action(move |action| recorded_actions.borrow_mut().push(action));

    // Platform, rather than viewport width, determines the menu presentation.
    // Native context menus aren't rendered by the headless backend, but these
    // activations must not mount a sheet or dispatch an action by themselves.
    for width in [390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        settle();
        click(&app, element(&app, "DetailPage::library_options_button"));
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "MenuSheet").count(),
            0,
            "desktop pointer activation at {width}px must use a popup"
        );
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
        app.set_detail_kb_zone(0);
        app.set_detail_kb_top(2);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Return.into(),
            });
        settle();
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "MenuSheet").count(),
            0,
            "desktop keyboard activation at {width}px must use a popup"
        );
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
    }
    assert!(actions.borrow().is_empty());
}

fn desktop_stream_right_click_prepares_actions_without_opening_a_sheet_or_playing() {
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(false);
    app.set_stream_selector_open(true);
    app.set_selected_title(s("Movie"));
    app.set_streams(
        Rc::new(VecModel::from(vec![
            stream_row("download:job-1", "Downloaded stream", "", true),
            stream_row("stream-1", "Remote stream", "", false),
        ]))
        .into(),
    );
    let requested = Rc::new(RefCell::new(Vec::new()));
    let recorded_requested = requested.clone();
    let weak = app.as_weak();
    app.on_stream_action_requested(move |id| {
        recorded_requested.borrow_mut().push(id.to_string());
        let app = weak.upgrade().unwrap();
        app.set_stream_action_id(id);
        app.set_stream_action_items(
            Rc::new(VecModel::from(vec![nova::SheetItem {
                label: s("Play stream"),
                enabled: true,
            }]))
            .into(),
        );
        app.set_stream_action_open(true);
    });
    let played = Rc::new(RefCell::new(Vec::new()));
    let recorded_played = played.clone();
    app.on_stream_picked(move |index| recorded_played.borrow_mut().push(index));
    app.window().show().unwrap();

    for series in [false, true] {
        app.set_detail_tab(if series { 3 } else { 0 });
        app.set_season_names(if series {
            Rc::new(VecModel::from(vec![s("Season 1")])).into()
        } else {
            Default::default()
        });
        for width in [390, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 1600));
            for label in ["Downloaded stream", "Remote stream"] {
                for _ in 0..10 {
                    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(
                        50,
                    ));
                }
                let row = ElementHandle::find_by_accessible_label(&app, label)
                    .next()
                    .expect("stream row");
                let p = row.absolute_position();
                let size = row.size();
                let position =
                    LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
                for event in [
                    slint::platform::WindowEvent::PointerPressed {
                        position,
                        button: slint::platform::PointerEventButton::Right,
                    },
                    slint::platform::WindowEvent::PointerReleased {
                        position,
                        button: slint::platform::PointerEventButton::Right,
                    },
                ] {
                    app.window().dispatch_event(event);
                }
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
                assert_eq!(
                    requested.borrow().last().map(String::as_str),
                    Some(if label == "Downloaded stream" {
                        "download:job-1"
                    } else {
                        "stream-1"
                    })
                );
                assert!(!app.get_stream_action_open());
                assert_eq!(
                    ElementHandle::find_by_element_type_name(&app, "MenuSheet").count(),
                    0,
                    "desktop stream menu at {width}px must use a popup"
                );
                assert!(played.borrow().is_empty(), "right-click must not play");
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: slint::platform::Key::Escape.into(),
                    });
            }
        }
    }
    assert_eq!(requested.borrow().len(), 8);
}

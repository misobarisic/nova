//! Selective backup stays usable on narrow screens and under TV navigation.
use crate::isolation;
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova_ui::AppWindow) {
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
fn key(app: &nova_ui::AppWindow, key: SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
    settle(app);
}
fn button(app: &nova_ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {label}"))
}

#[test]
fn backup_phone_preview_wraps_long_names_and_only_exposes_current_stage_actions() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_phone_preview_wraps_long_names_and_only_exposes_current_stage_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(360., 780.));
    app.set_is_android(true);
    app.set_animations(false);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_importing(true);
    ui.set_preview_ready(true);
    ui.set_mappings(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupMapping {
            id: "p1".into(),
            label: "A long household profile name which wraps on phones".into(),
            target: "Create new profile".into(),
        }]))
        .into(),
    );
    ui.set_components(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
            id: "p1\0progress".into(),
            label: "A long household profile / Playback progress and unknown provider IDs (1200)"
                .into(),
            checked: true,
        }]))
        .into(),
    );
    ui.set_preview_rows(Rc::new(VecModel::from(vec![SharedString::from("Household / Playback progress\nArchive: 1200 · New: 100 · Conflicts: 3 · Preserved local: 1097\nConflict identities: native-provider:episode-with-a-very-long-identifier")])).into());
    app.window().show().unwrap();
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Export selected data")
            .next()
            .is_none()
    );
    // The preview is taller than a phone. Reach its primary action through
    // the real focus route, which must also reveal it inside the viewport.
    for _ in 0..7 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let apply = button(&app, "Import reviewed changes");
    let bounds = apply.size();
    assert!(bounds.width <= 324., "button overflows phone: {bounds:?}");
    assert!(bounds.height >= 44.);
    key(&app, slint::platform::Key::DownArrow.into());
    assert!(button(&app, "Rebuild import preview").size().height >= 44.);
    let closed = Rc::new(RefCell::new(0));
    let calls = closed.clone();
    ui.on_closed(move || *calls.borrow_mut() += 1);
    ui.set_busy(true);
    key(&app, slint::platform::Key::Escape.into());
    assert_eq!(
        *closed.borrow(),
        0,
        "Back must not imply cancelling a commit"
    );
    ui.set_busy(false);
    key(&app, slint::platform::Key::Escape.into());
    assert_eq!(*closed.borrow(), 1);
}

#[test]
fn backup_tv_stage_navigation_skips_absent_actions_and_activates_current_primary() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_tv_stage_navigation_skips_absent_actions_and_activates_current_primary"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1920., 1080.));
    app.global::<nova_ui::InterfaceStyle>().set_tv(true);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_importing(true);
    ui.set_preview_ready(false);
    let built = Rc::new(RefCell::new(0));
    let calls = built.clone();
    ui.on_build_preview(move || *calls.borrow_mut() += 1);
    app.window().show().unwrap();
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Import reviewed changes")
            .next()
            .is_none()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Rebuild import preview")
            .next()
            .is_none()
    );
    // Back, three presets, Replace mode, then the current primary action.
    for _ in 0..2 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    // Publishing an underlying page must retain this modal's focus and
    // current selection while asynchronous profile/settings models refresh.
    app.invoke_preserve_back_focus();
    app.invoke_focus_mounted_page();
    settle(&app);
    for _ in 0..3 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(*built.borrow(), 1);
}

#[test]
fn backup_blank_margins_block_underlying_settings_clicks_and_wheel() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_blank_margins_block_underlying_settings_clicks_and_wheel"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1440., 960.));
    app.set_animations(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(1);
    app.set_settings_detail_open(true);
    let settings = app.global::<nova_ui_shared::SettingsBridge>();
    settings.set_category_rows(
        Rc::new(VecModel::from(vec![nova_ui_shared::CategoryRow {
            name: "Retain this category".into(),
        }]))
        .into(),
    );
    let removals = Rc::new(RefCell::new(0));
    let calls = removals.clone();
    settings.on_category_removed(move |_| *calls.borrow_mut() += 1);
    app.window().show().unwrap();
    settle(&app);
    for attempt in 0..14 {
        let row = button(&app, "Remove");
        let position = row.absolute_position();
        if position.y >= 120. && position.y + row.size().height < 900. {
            break;
        }
        app.set_settings_scroll_y(if attempt == 0 {
            -400.
        } else {
            app.get_settings_scroll_y() - 120.
        });
        settle(&app);
    }
    let row = button(&app, "Remove");
    app.global::<nova_ui::BackupBridge>().set_open(true);
    settle(&app);
    let position = row.absolute_position();
    let size = row.size();
    let underlay_scroll = app.get_settings_scroll_y();
    let scroll = ElementHandle::find_by_element_id(&app, "BackupPanel::scroll")
        .next()
        .unwrap();
    assert!(
        position.x > scroll.absolute_position().x + scroll.size().width,
        "underlay target must be in a blank overlay margin"
    );
    let position =
        slint::LogicalPosition::new(position.x + size.width / 2., position.y + size.height / 2.);
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerScrolled {
            position,
            delta_x: 0.,
            delta_y: -300.,
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle(&app);
    assert_eq!(*removals.borrow(), 0);
    assert_eq!(app.get_settings_scroll_y(), underlay_scroll);
}

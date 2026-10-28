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
fn data_submenu_pushes_and_pops_with_retained_inactive_exit() {
    if !isolation::run(concat!(
        module_path!(),
        "::data_submenu_pushes_and_pops_with_retained_inactive_exit"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(430., 800.));
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_detail_open(false);
    app.set_settings_search_query("Data".into());
    app.on_settings_search_matches(|q, h| nova_ui::settings_search_matches(&q, &h));
    let ui = app.global::<nova_ui::BackupBridge>();
    let weak = app.as_weak();
    ui.on_opened(move || {
        weak.upgrade()
            .unwrap()
            .global::<nova_ui::BackupBridge>()
            .set_open(true);
    });
    let closed = Rc::new(std::cell::Cell::new(0));
    let calls = closed.clone();
    let weak = app.as_weak();
    ui.on_closed(move || {
        calls.set(calls.get() + 1);
        weak.upgrade()
            .unwrap()
            .global::<nova_ui::BackupBridge>()
            .set_open(false);
    });
    let presets = Rc::new(std::cell::Cell::new(0));
    let calls = presets.clone();
    ui.on_preset(move |_| calls.set(calls.get() + 1));
    app.show().unwrap();
    for _ in 0..4 {
        settle(&app);
    }

    fn frames(app: &nova_ui::AppWindow, count: usize) {
        for _ in 0..count {
            for item in ElementQuery::from_root(app)
                .match_predicate(|_| true)
                .find_all()
            {
                let _ = (item.absolute_position(), item.size());
            }
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
    }
    fn page(app: &nova_ui::AppWindow) -> Option<ElementHandle> {
        ElementHandle::find_by_element_id(app, "AppWindow::backup_page").next()
    }
    fn back_without_wait(app: &nova_ui::AppWindow) {
        let text: SharedString = slint::platform::Key::Back.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    }

    button(&app, "Data").mock_single_click(slint::platform::PointerEventButton::Left);
    frames(&app, 4);
    let entering = page(&app)
        .expect("Data mounts before its entrance completes")
        .absolute_position()
        .x;
    assert!(
        entering > 0.5 && entering < 429.5,
        "Data must sample an intermediate push, got {entering}"
    );
    frames(&app, 24);
    assert!(page(&app).unwrap().absolute_position().x.abs() < 0.5);
    let select = button(&app, "Select");
    assert_eq!(select.accessible_enabled(), Some(true));

    back_without_wait(&app);
    frames(&app, 4);
    assert_eq!(closed.get(), 1);
    let exiting = page(&app)
        .expect("Back retains Data for its exit")
        .absolute_position()
        .x;
    assert!(
        exiting > 0.5 && exiting < 429.5,
        "Data must sample an intermediate pop, got {exiting}"
    );
    assert_eq!(
        select.accessible_enabled(),
        Some(false),
        "outgoing actions have no authority"
    );
    select.mock_single_click(slint::platform::PointerEventButton::Left);
    back_without_wait(&app);
    app.set_system_back_request(app.get_system_back_request() + 1);
    frames(&app, 1);
    assert_eq!(presets.get(), 0);
    assert_eq!(closed.get(), 1, "repeat Back cannot close another layer");
    assert!(app.get_show_settings());

    // Interrupting exit must cancel its cleanup; the replacement open stays
    // mounted and reacquires focus rather than disappearing on an old timer.
    ui.set_open(true);
    frames(&app, 28);
    assert!(page(&app).unwrap().absolute_position().x.abs() < 0.5);
    assert_eq!(button(&app, "Select").accessible_enabled(), Some(true));
    back_without_wait(&app);
    frames(&app, 28);
    assert!(page(&app).is_none());
    assert!(app.get_show_settings());
    assert!(!app.get_settings_detail_open());

    for (master, transitions) in [(false, true), (true, false)] {
        app.set_animations(master);
        app.set_anim_transitions(transitions);
        app.global::<nova_ui::Anim>().set_enabled(master);
        app.global::<nova_ui::Anim>().set_transitions(transitions);
        ui.set_open(true);
        frames(&app, 2);
        assert!(
            page(&app).unwrap().absolute_position().x.abs() < 0.5,
            "disabled motion opens immediately"
        );
        back_without_wait(&app);
        frames(&app, 2);
        assert!(page(&app).is_none(), "disabled motion closes immediately");
    }
    app.set_animations(true);
    app.set_anim_transitions(true);
    app.global::<nova_ui::Anim>().set_enabled(true);
    app.global::<nova_ui::Anim>().set_transitions(true);
    ui.set_open(true);
    frames(&app, 28);
    back_without_wait(&app);
    frames(&app, 2);
    app.set_anim_transitions(false);
    app.global::<nova_ui::Anim>().set_transitions(false);
    frames(&app, 2);
    assert!(
        page(&app).is_none(),
        "disabling transitions finishes a pending exit"
    );
    app.set_anim_transitions(true);
    app.global::<nova_ui::Anim>().set_transitions(true);
    frames(&app, 28);
    assert!(
        page(&app).is_none(),
        "enabling motion cannot replay a closed Data page"
    );
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
    app.global::<nova_ui::Anim>().set_enabled(false);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_importing(true);
    ui.set_preview_ready(true);
    ui.set_profiles(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
            id: "p1".into(),
            label: "A long household profile name which wraps on phones".into(),
            checked: true,
        }]))
        .into(),
    );
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
    for _ in 0..9 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let apply = button(&app, "Import reviewed changes");
    let bounds = apply.size();
    assert!(bounds.width <= 324., "button overflows phone: {bounds:?}");
    assert!(bounds.height >= 44.);
    assert!(apply.absolute_position().y >= 18.);
    assert!(apply.absolute_position().y + bounds.height <= 762.);
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
fn backup_full_viewport_blocks_underlying_settings_clicks_and_wheel() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_full_viewport_blocks_underlying_settings_clicks_and_wheel"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1440., 960.));
    app.set_animations(false);
    app.global::<nova_ui::Anim>().set_enabled(false);
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
        position.x >= scroll.absolute_position().x
            && position.x + size.width <= scroll.absolute_position().x + scroll.size().width + 0.5,
        "The full-width destination covers the underlying control"
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

#[test]
fn backup_tv_reviews_every_row_and_pages_long_details_before_import() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_tv_reviews_every_row_and_pages_long_details_before_import"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(960., 600.));
    app.global::<nova_ui::InterfaceStyle>().set_tv(true);
    app.set_animations(false);
    app.global::<nova_ui::Anim>().set_enabled(false);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_importing(true);
    ui.set_preview_ready(true);
    let long = (0..50)
        .map(|index| format!("Conflict identity {index}: provider/title/episode"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut rows = vec![SharedString::from(long)];
    rows.extend((1..12).map(|index| {
        SharedString::from(format!(
            "Profile / Component {index}\nArchive: 15 · Deletions: {index}"
        ))
    }));
    ui.set_preview_rows(Rc::new(VecModel::from(rows.clone())).into());
    let imported = Rc::new(RefCell::new(0));
    let calls = imported.clone();
    ui.on_apply_import(move || *calls.borrow_mut() += 1);
    app.window().show().unwrap();
    settle(&app);
    // Back, presets and mode precede the first preview row.
    for _ in 0..5 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let long_row = ElementHandle::find_by_accessible_label(&app, &rows[0])
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Text))
        .max_by(|a, b| a.size().height.total_cmp(&b.size().height))
        .unwrap();
    assert!(long_row.size().height > 564.);
    assert!(
        (long_row.absolute_position().y - 18.).abs() < 2.,
        "long reviews begin at their first line"
    );
    let pages = ((long_row.size().height - 564.) / (564. * 0.75)).ceil() as usize;
    for _ in 0..pages {
        let before = long_row.absolute_position().y;
        key(&app, slint::platform::Key::DownArrow.into());
        assert!(
            long_row.absolute_position().y < before,
            "Down reads the rest of a long row before leaving it"
        );
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(*imported.borrow(), 0);
    }
    for row in rows.iter().skip(1) {
        key(&app, slint::platform::Key::DownArrow.into());
        let row = ElementHandle::find_by_accessible_label(&app, row)
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Text))
            .max_by(|a, b| a.size().height.total_cmp(&b.size().height))
            .unwrap();
        assert!(row.absolute_position().y >= 17.);
        assert!(row.absolute_position().y + row.size().height <= 583.);
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(*imported.borrow(), 0, "review rows never activate Import");
    }
    key(&app, slint::platform::Key::DownArrow.into());
    let apply = button(&app, "Import reviewed changes");
    assert!(apply.absolute_position().y >= 18.);
    assert!(apply.absolute_position().y + apply.size().height <= 582.);
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(*imported.borrow(), 1);
    // Reverse traversal starts at the end of the oversized row and lets Up
    // reveal earlier details, rather than cutting off either reading edge.
    for _ in 0..12 {
        key(&app, slint::platform::Key::UpArrow.into());
    }
    let before = long_row.absolute_position().y;
    key(&app, slint::platform::Key::UpArrow.into());
    assert!(long_row.absolute_position().y > before);
}

#[test]
fn backup_recovery_delete_requires_confirmation_and_back_cancels() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_recovery_delete_requires_confirmation_and_back_cancels"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(360., 780.));
    app.set_animations(false);
    app.global::<nova_ui::Anim>().set_enabled(false);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_recovery_rows(
        Rc::new(VecModel::from(vec![
            SharedString::from("2026-10-20 · Household · Components: 2"),
            SharedString::from("2026-10-21 · Guest · Components: 1"),
        ]))
        .into(),
    );
    let deleted = Rc::new(RefCell::new(0));
    let calls = deleted.clone();
    ui.on_confirm_delete_recovery(move || *calls.borrow_mut() += 1);
    let cancelled = Rc::new(RefCell::new(0));
    let calls = cancelled.clone();
    let weak = app.as_weak();
    ui.on_cancel_delete_recovery(move || {
        *calls.borrow_mut() += 1;
        weak.upgrade()
            .unwrap()
            .global::<nova_ui::BackupBridge>()
            .set_recovery_confirmation(false);
    });
    let weak = app.as_weak();
    ui.on_delete_recovery(move || {
        weak.upgrade()
            .unwrap()
            .global::<nova_ui::BackupBridge>()
            .set_recovery_confirmation(true)
    });
    let closed = Rc::new(RefCell::new(0));
    let calls = closed.clone();
    ui.on_closed(move || *calls.borrow_mut() += 1);
    app.window().show().unwrap();
    settle(&app);
    for _ in 0..7 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    key(&app, slint::platform::Key::Return.into());
    assert!(ui.get_recovery_confirmation());
    assert_eq!(
        *deleted.borrow(),
        0,
        "first activation only opens the inventory"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Delete 2 recovery copies permanently?")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Export selected data")
            .next()
            .is_none()
    );
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(*cancelled.borrow(), 1, "confirmation defaults to Cancel");
    assert_eq!(*deleted.borrow(), 0);
    ui.set_recovery_confirmation(true);
    settle(&app);
    key(&app, slint::platform::Key::Back.into());
    assert_eq!(*cancelled.borrow(), 2);
    assert_eq!(
        *closed.borrow(),
        0,
        "Back cancels only the destructive prompt"
    );
    assert_eq!(*deleted.borrow(), 0);
    ui.set_recovery_confirmation(true);
    settle(&app);
    for _ in 0..3 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let delete = button(&app, "Permanently delete recovery copies");
    assert!(delete.size().width <= 324.);
    assert!(delete.absolute_position().y + delete.size().height <= 762.);
    key(&app, slint::platform::Key::Return.into());
    assert_eq!(*deleted.borrow(), 1);
}

#[test]
fn backup_import_exposes_one_include_switch_per_source_profile() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_import_exposes_one_include_switch_per_source_profile"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.global::<nova_ui::Anim>().set_enabled(false);
    app.window().set_size(slint::LogicalSize::new(960., 600.));
    app.global::<nova_ui::InterfaceStyle>().set_tv(true);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    ui.set_importing(true);
    ui.set_profiles(
        Rc::new(VecModel::from(vec![
            nova_ui_shared::BackupOption {
                id: "p1".into(),
                label: "Household".into(),
                checked: true,
            },
            nova_ui_shared::BackupOption {
                id: "p2".into(),
                label: "Guest".into(),
                checked: false,
            },
        ]))
        .into(),
    );
    ui.set_components(
        Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
            id: "p1\0progress".into(),
            label: "Household / Progress".into(),
            checked: true,
        }]))
        .into(),
    );
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let calls = toggled.clone();
    ui.on_toggle_profile(move |source| calls.borrow_mut().push(source.to_string()));
    let component_changes = Rc::new(RefCell::new(0));
    let calls = component_changes.clone();
    ui.on_toggle_component(move |_| *calls.borrow_mut() += 1);
    app.window().show().unwrap();
    settle(&app);
    // Reach the first profile through the actual remote route. TV presets
    // put these controls below the initial viewport, so inspect each only
    // after focus has revealed its row.
    for _ in 0..4 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    for (index, (label, checked)) in [
        ("Include profile: Household", true),
        ("Include profile: Guest", false),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            key(&app, slint::platform::Key::DownArrow.into());
        }
        let toggle = ElementHandle::find_by_accessible_label(&app, label)
            .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
            .unwrap();
        assert_eq!(toggle.accessible_checked(), Some(checked));
        assert!(toggle.absolute_position().y >= 18.);
        assert!(toggle.absolute_position().y + toggle.size().height <= 582.);
        key(&app, slint::platform::Key::Return.into());
    }
    assert_eq!(&*toggled.borrow(), &["p1", "p2"]);
    assert_eq!(
        *component_changes.borrow(),
        0,
        "source inclusion has its own action"
    );
}

#[test]
fn backup_retained_toggle_tracks_presets_and_external_checked_updates_after_clicks() {
    use slint::Model;
    use std::cell::Cell;
    if !isolation::run(concat!(
        module_path!(),
        "::backup_retained_toggle_tracks_presets_and_external_checked_updates_after_clicks"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(960., 700.));
    app.set_animations(false);
    app.global::<nova_ui::Anim>().set_enabled(false);
    let ui = app.global::<nova_ui::BackupBridge>();
    ui.set_open(true);
    let rows = Rc::new(VecModel::from(vec![nova_ui_shared::BackupOption {
        id: "library".into(),
        label: "Library data".into(),
        checked: false,
    }]));
    ui.set_components(rows.clone().into());
    let original = ui.get_components();
    let denied = Rc::new(Cell::new(true));
    let denied_calls = Rc::new(Cell::new(0));
    let gate = denied.clone();
    let calls = denied_calls.clone();
    let backend_rows = rows.clone();
    ui.on_toggle_component(move |_| {
        if gate.get() {
            calls.set(calls.get() + 1);
            return;
        }
        let mut row = backend_rows.row_data(0).unwrap();
        row.checked = !row.checked;
        backend_rows.set_row_data(0, row);
    });
    let backend_rows = rows.clone();
    ui.on_preset(move |preset| {
        let mut row = backend_rows.row_data(0).unwrap();
        row.checked = preset == 0;
        backend_rows.set_row_data(0, row);
    });
    app.window().show().unwrap();
    settle(&app);
    for _ in 0..4 {
        key(&app, slint::platform::Key::DownArrow.into());
    }
    let toggle = ElementHandle::find_by_accessible_label(&app, "Library data")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
        .unwrap();
    let click = || {
        let position = toggle.absolute_position();
        let size = toggle.size();
        let position = slint::LogicalPosition::new(
            position.x + size.width / 2.,
            position.y + size.height / 2.,
        );
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
        settle(&app);
    };
    click();
    assert_eq!(
        denied_calls.get(),
        1,
        "the pointer reached the denied backend request"
    );
    assert_eq!(
        toggle.accessible_checked(),
        Some(false),
        "denial must restore an unchecked owner even without a checked change notification"
    );
    assert!(!rows.row_data(0).unwrap().checked);
    // Same-ID publishers skip unchanged rows, so publication alone cannot
    // repair a switch left in its speculative local state by a denied click.
    ui.set_components(original.clone());
    settle(&app);
    assert_eq!(toggle.accessible_checked(), Some(false));
    denied.set(false);
    click();
    assert_eq!(toggle.accessible_checked(), Some(true));
    assert!(rows.row_data(0).unwrap().checked);
    ui.invoke_preset(1);
    settle(&app);
    assert_eq!(
        toggle.accessible_checked(),
        Some(false),
        "a preset must override the preceding local click"
    );
    ui.invoke_preset(0);
    settle(&app);
    assert_eq!(toggle.accessible_checked(), Some(true));
    click();
    assert_eq!(toggle.accessible_checked(), Some(false));
    assert!(!rows.row_data(0).unwrap().checked);
    let mut external = rows.row_data(0).unwrap();
    external.checked = true;
    rows.set_row_data(0, external);
    settle(&app);
    assert_eq!(
        toggle.accessible_checked(),
        Some(true),
        "external checked changes still reach the retained switch"
    );
    denied.set(true);
    click();
    assert_eq!(denied_calls.get(), 2);
    assert_eq!(
        toggle.accessible_checked(),
        Some(true),
        "denial also restores a checked owner after a speculative off click"
    );
    ui.set_components(original.clone());
    settle(&app);
    assert_eq!(toggle.accessible_checked(), Some(true));
    assert!(
        std::ptr::eq(original.as_any(), ui.get_components().as_any()),
        "state updates keep the model and toggle delegate mounted"
    );
}

#[test]
fn backup_canvas_and_content_follow_the_full_viewport_through_resize_and_motion() {
    if !isolation::run(concat!(
        module_path!(),
        "::backup_canvas_and_content_follow_the_full_viewport_through_resize_and_motion"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova_ui::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_animations(false);
    app.global::<nova_ui::Anim>().set_enabled(false);
    app.window().show().unwrap();
    let ui = app.global::<nova_ui::BackupBridge>();
    let safe = i_slint_core::lengths::LogicalEdges {
        top: 28.0,
        bottom: 12.0,
        left: 8.0,
        right: 16.0,
    };
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(safe);

    fn element(app: &nova_ui::AppWindow, id: &str) -> ElementHandle {
        ElementHandle::find_by_element_id(app, id)
            .next()
            .unwrap_or_else(|| panic!("missing {id}"))
    }
    fn full_viewport(app: &nova_ui::AppWindow, safe: i_slint_core::lengths::LogicalEdges) {
        let viewport = app.window().size().to_logical(app.window().scale_factor());
        let host = element(app, "AppWindow::backup_host");
        let page = element(app, "AppWindow::backup_page");
        let scroll = element(app, "BackupPanel::scroll");
        assert!(host.absolute_position().x.abs() < 0.5 && host.absolute_position().y.abs() < 0.5);
        assert!((host.size().width - viewport.width).abs() < 0.5);
        assert!((host.size().height - viewport.height).abs() < 0.5);
        assert_eq!(
            host.computed_opacity(),
            1.0,
            "Canvas must remain opaque while its content slides/fades"
        );
        assert!((page.size().width - viewport.width).abs() < 0.5);
        assert!((page.size().height - viewport.height).abs() < 0.5);
        let expected_width = (viewport.width - safe.left - safe.right - 36.0).max(0.0);
        assert!(
            (scroll.size().width - expected_width).abs() < 1.0,
            "Data must use all safe width, got {} of {expected_width}",
            scroll.size().width
        );
        assert!(
            (scroll.absolute_position().x - page.absolute_position().x - safe.left - 18.0).abs()
                < 1.0
        );
        assert!(scroll.size().height >= 0.0);
        assert!(scroll.absolute_position().y >= safe.top);
        assert!(
            scroll.absolute_position().y + scroll.size().height
                <= viewport.height - safe.bottom + 0.5
        );
    }
    fn frames(app: &nova_ui::AppWindow, count: usize) {
        for _ in 0..count {
            for item in ElementQuery::from_root(app)
                .match_predicate(|_| true)
                .find_all()
            {
                let _ = (item.absolute_position(), item.size());
            }
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
    }
    for (width, height, android, tv) in [
        (360, 780, true, false),
        (1440, 900, false, false),
        (1920, 1080, false, true),
        (800, 360, true, false),
    ] {
        app.set_is_android(android);
        app.global::<nova_ui::InterfaceStyle>().set_tv(tv);
        app.window()
            .set_size(slint::LogicalSize::new(width as f32, height as f32));
        ui.set_open(true);
        settle(&app);
        full_viewport(&app, safe);
        let scroll = element(&app, "BackupPanel::scroll");
        let select = button(&app, "Select");
        assert!(select.absolute_position().x >= scroll.absolute_position().x);
        assert!(
            select.absolute_position().x + select.size().width
                <= scroll.absolute_position().x + scroll.size().width + 0.5,
            "Controls cannot introduce horizontal overflow"
        );
        app.window().set_size(slint::LogicalSize::new(
            width as f32 + 112.0,
            height as f32 + 32.0,
        ));
        settle(&app);
        full_viewport(&app, safe);
        ui.set_open(false);
        settle(&app);
    }
    app.set_is_android(false);
    app.global::<nova_ui::InterfaceStyle>().set_tv(false);
    app.set_animations(true);
    app.set_anim_transitions(true);
    app.global::<nova_ui::Anim>().set_enabled(true);
    app.global::<nova_ui::Anim>().set_transitions(true);
    app.window()
        .set_size(slint::LogicalSize::new(1440.0, 900.0));
    ui.set_open(true);
    frames(&app, 4);
    full_viewport(&app, safe);
    let entering = element(&app, "AppWindow::backup_page")
        .absolute_position()
        .x;
    assert!(
        entering > 0.5 && entering < 1439.5,
        "Content retains the existing push transition"
    );
    app.window()
        .set_size(slint::LogicalSize::new(1920.0, 1080.0));
    frames(&app, 1);
    full_viewport(&app, safe);
    frames(&app, 28);
    // Full-width rows put Select near the right edge. Retain its steady-state
    // handle: accessibility lookup culls it once the exit slides it offscreen.
    let retained_select = button(&app, "Select");
    ui.set_open(false);
    frames(&app, 4);
    full_viewport(&app, safe);
    assert!(
        element(&app, "AppWindow::backup_page")
            .absolute_position()
            .x
            > 0.5
    );
    assert_eq!(
        retained_select.accessible_enabled(),
        Some(false),
        "Retained exit content has no authority"
    );
    app.window()
        .set_size(slint::LogicalSize::new(1000.0, 650.0));
    frames(&app, 1);
    full_viewport(&app, safe);
    frames(&app, 28);
    assert!(
        ElementHandle::find_by_element_id(&app, "AppWindow::backup_host")
            .next()
            .is_none()
    );
}

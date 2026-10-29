use crate::preference_support::{key, reveal, settle, toggle};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{
    ComponentHandle, VecModel,
    platform::{Key, PointerEventButton},
};
use std::{cell::RefCell, rc::Rc};

#[test]
fn specials_default_off_and_setting_routes_pointer_keyboard_and_bridge_state() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::specials_default_off_and_setting_routes_pointer_keyboard_and_bridge_state"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    assert!(!app.get_include_specials_in_progress());
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(3);
    app.set_settings_detail_open(true);
    let fields = Rc::new(RefCell::new(Vec::new()));
    let recorded = fields.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for (width, height, touch) in [(390, 844, true), (1280, 800, false), (900, 360, true)] {
        app.set_is_android(touch);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_include_specials_in_progress(false);
        let control = reveal(
            &app,
            || toggle(&app, "Include specials in progress"),
            "Include specials in progress",
        );
        assert_eq!(control.accessible_checked(), Some(false));
        control.mock_single_click(PointerEventButton::Left);
        settle(&app);
        assert!(app.get_include_specials_in_progress());
        assert!(
            app.global::<nova::SettingsBridge>()
                .get_include_specials_in_progress()
        );
        assert_eq!(
            fields.borrow().last().map(String::as_str),
            Some("include_specials_in_progress")
        );
        key(&app, Key::LeftArrow);
        assert!(!app.get_include_specials_in_progress());
        key(&app, Key::RightArrow);
        assert!(app.get_include_specials_in_progress());
    }
}

fn click(app: &nova::AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| {
            matches!(
                item.accessible_role(),
                Some(AccessibleRole::Button | AccessibleRole::RadioButton)
            )
        })
        .unwrap_or_else(|| panic!("missing button: {label}"))
        .mock_single_click(PointerEventButton::Left);
    settle(app);
}

#[test]
fn title_override_and_episode_exclusion_actions_follow_effective_specials_preference() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::title_override_and_episode_exclusion_actions_follow_effective_specials_preference"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.window().set_size(slint::PhysicalSize::new(400, 1800));
    app.set_touch_menus(true);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_in_library(true);
    app.set_selected_title("A show".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 1".into()])).into());
    app.set_season_cards(
        Rc::new(VecModel::from(vec![nova::SeasonCard {
            name: "Season 1".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_episode_rows(
        Rc::new(VecModel::from(vec![nova::EpisodeRow {
            text: "An episode".into(),
            ep_no: "S1 E1".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_episode_total(1);
    let overrides = Rc::new(RefCell::new(Vec::new()));
    let recorded = overrides.clone();
    app.on_progress_scope_changed(move |index| recorded.borrow_mut().push(index));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_episode_watch_action(move |index, action| recorded.borrow_mut().push((index, action)));
    app.show().unwrap();
    settle(&app);
    click(&app, "Library options");
    click(&app, "Progress options");
    click(&app, "Include specials in progress");
    assert_eq!(*overrides.borrow(), vec![2]);
    for enabled in [false, true, false] {
        app.set_progress_include_specials(enabled);
        ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::ep_card")
            .next()
            .unwrap()
            .mock_single_click(PointerEventButton::Right);
        settle(&app);
        let exclusion = ElementHandle::find_by_accessible_label(&app, "Exclude from progress")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button));
        assert_eq!(exclusion.is_some(), enabled);
        if let Some(button) = exclusion {
            button.mock_single_click(PointerEventButton::Left);
            settle(&app);
            assert_eq!(actions.borrow().last(), Some(&(0, 6)));
        } else {
            key(&app, Key::Escape);
        }
    }
    app.set_progress_include_specials(true);
    app.set_episode_rows(
        Rc::new(VecModel::from(vec![nova::EpisodeRow {
            text: "An episode".into(),
            ep_no: "S1 E1".into(),
            progress_excluded: true,
            ..Default::default()
        }]))
        .into(),
    );
    ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::ep_card")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Right);
    settle(&app);
    click(&app, "Include in progress");
    assert_eq!(actions.borrow().len(), 2);

    // The separately hosted desktop popup uses the same gate and backend action.
    app.set_touch_menus(false);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let recorded = opened.clone();
    app.on_episode_picked(move |index| recorded.borrow_mut().push(index));
    for enabled in [false, true] {
        app.set_progress_include_specials(enabled);
        ElementHandle::find_by_element_id(&app, "DetailEpisodesModule::ep_card")
            .next()
            .unwrap()
            .mock_single_click(PointerEventButton::Right);
        settle(&app);
        let before = actions.borrow().len();
        key(&app, Key::UpArrow);
        key(&app, Key::Return);
        if enabled {
            assert_eq!(actions.borrow().len(), before + 1);
            assert_eq!(actions.borrow().last(), Some(&(0, 6)));
        } else {
            assert_eq!(actions.borrow().len(), before);
            assert_eq!(*opened.borrow(), vec![0]);
        }
    }
}

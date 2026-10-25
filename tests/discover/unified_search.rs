//! The Home entry and Discover scopes share one retained search surface.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{
    ComponentHandle, Model, VecModel,
    platform::{Key, PointerEventButton, WindowEvent},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn frame(app: &nova::AppWindow, ms: u64) {
    // Match the event loop: flush property-change handlers before layout can
    // instantiate the entering controls. Otherwise an empty search can create
    // its controls at the old opacity before its deferred reveal is armed.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
    for item in ElementQuery::from_root(app)
        .match_predicate(|_| true)
        .find_all()
    {
        let _ = (
            item.size(),
            item.absolute_position(),
            item.computed_opacity(),
        );
    }
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        frame(app, 25);
    }
}

fn key(app: &nova::AppWindow, key: Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}

#[test]
fn home_entry_scopes_and_typed_result_actions_work_across_viewports() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::home_entry_scopes_and_typed_result_actions_work_across_viewports"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    assert_eq!(
        app.get_search_scope(),
        1,
        "fresh Home/Discover search starts in Catalogs"
    );
    assert_eq!(app.global::<nova::DiscoverBridge>().get_search_scope(), 1);
    app.global::<nova::Anim>().set_enabled(false);
    let opened = Rc::new(Cell::new(0));
    let count = opened.clone();
    let weak = app.as_weak();
    app.on_home_search_picked(move || {
        count.set(count.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_show_home(false);
        app.set_discover_search_filters_open(true);
    });
    let scopes = Rc::new(RefCell::new(Vec::new()));
    let changes = scopes.clone();
    app.on_search_scope_picked(move |scope| changes.borrow_mut().push(scope));
    let selected = Rc::new(RefCell::new(Vec::new()));
    let actions = selected.clone();
    app.on_item_selected(move |id, index| actions.borrow_mut().push((id.to_string(), index)));
    app.show().unwrap();
    for (width, height, touch) in [(1280, 800, false), (390, 844, true), (900, 480, true)] {
        app.set_is_android(touch);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_show_home(true);
        app.set_home_view(0);
        app.set_home_featured_count(0);
        app.set_home_scroll_y(0.0);
        app.set_home_continue(
            Rc::new(VecModel::from(vec![nova::ContinueRow {
                id: "blue-horizon".into(),
                title: "Blue Horizon".into(),
                episode_id: "blue-horizon:1:7".into(),
                ep_no: "S1 E7".into(),
                remaining: "27 min left".into(),
                ..Default::default()
            }]))
            .into(),
        );
        app.set_discover_search_filters_open(false);
        app.set_discover_search_open(false);
        settle(&app);
        let search = ElementHandle::find_by_accessible_label(&app, "Search Nova")
            .next()
            .expect("Home search icon");
        let rect = (search.absolute_position(), search.size());
        assert!(rect.0.x >= 0.0 && rect.0.x + rect.1.width <= width as f32);
        let see_all = ElementHandle::find_by_accessible_label(&app, "See all")
            .next()
            .expect("nonempty Home section action");
        assert!(
            see_all.absolute_position().y >= rect.0.y + rect.1.height,
            "without a showcase, Home search must not cover the first See all action"
        );
        search.mock_single_click(PointerEventButton::Left);
        settle(&app);
        assert!(!app.get_show_home());
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Search scope")
                .next()
                .is_some()
        );
        app.set_discover_search_open(true);
        app.set_search_scope(1);
        app.set_discover_kb_zone(2);
        app.set_discover_kb_ctl(3);
        app.global::<nova::DiscoverBridge>().invoke_focus_shell();
        key(&app, Key::RightArrow);
        assert_eq!(app.get_discover_kb_ctl(), 4);
        key(&app, Key::Return);
        assert_eq!(app.get_search_scope(), 2);
        assert_eq!(scopes.borrow().last(), Some(&2));
        // Backend publication includes exact target IDs, never a parent title
        // substituted by the page when the user activates an episode or note.
        app.set_search_scope(3);
        app.set_search_results(
            Rc::new(VecModel::from(vec![nova::MediaCard {
                id: "local:episode:exact".into(),
                title: "S2 E4".into(),
                badge: "Episode".into(),
                year: "Saved series".into(),
                ..Default::default()
            }]))
            .into(),
        );
        settle(&app);
        let card = ElementHandle::find_by_accessible_label(&app, "S2 E4, Episode, Saved series")
            .next()
            .expect("typed local result");
        assert!(card.size().width > 0.0);
        app.set_discover_kb_zone(3);
        app.set_discover_kb_idx(0);
        key(&app, Key::Return);
        assert_eq!(
            selected.borrow().last(),
            Some(&("local:episode:exact".into(), 0))
        );
        assert_eq!(
            app.get_catalog().row_count(),
            0,
            "search never changes the browse grid"
        );
    }
    assert_eq!(opened.get(), 3);
}

#[test]
fn unified_scope_and_incremental_results_share_motion_switches_without_retaining_old_titles() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::unified_scope_and_incremental_results_share_motion_switches_without_retaining_old_titles"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.show().unwrap();
    app.set_show_home(false);
    let motion = app.global::<nova::Anim>();
    let card = |id: &str, title: &str| nova::MediaCard {
        id: id.into(),
        title: title.into(),
        badge: "Episode".into(),
        ..Default::default()
    };
    let label = |title: &str| format!("{title}, Episode");
    settle(&app);
    assert_eq!(app.get_catalog().row_count(), 0);
    assert_eq!(app.get_addon_names().row_count(), 0);
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        motion.set_enabled(enabled);
        motion.set_transitions(transitions);
        app.set_discover_search_filters_open(false);
        app.set_discover_search_open(false);
        app.set_discover_search_focused(false);
        app.set_search_results(Default::default());
        settle(&app);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Search scope")
                .next()
                .is_none(),
            "the fixture must leave the search context before testing entry"
        );
        app.set_discover_search_filters_open(true);
        let mut scope_faded = false;
        for _ in 0..20 {
            frame(&app, 16);
            let scope = ElementHandle::find_by_accessible_label(&app, "Search scope")
                .next()
                .unwrap();
            scope_faded |= (0.01..0.99).contains(&scope.computed_opacity());
            if !enabled || !transitions {
                assert!(scope.computed_opacity() > 0.99);
            }
        }
        assert_eq!(scope_faded, enabled && transitions);
        app.set_discover_search_open(true);
        app.set_discover_search_animate_results(true);
        app.set_search_scope(3);
        let model = Rc::new(VecModel::from(vec![card(
            "local:episode:exact",
            "Old episode title",
        )]));
        app.set_search_results(model.clone().into());
        let mut result_faded = false;
        for tick in 0..24 {
            frame(&app, 16);
            if tick == 4 {
                let mut update = model.row_data(0).unwrap();
                update.poster_path = "updated-poster".into();
                model.set_row_data(0, update);
                model.push(card("catalog:incremental", "Incremental result"));
            }
            let result = ElementHandle::find_by_accessible_label(&app, &label("Old episode title"))
                .next()
                .unwrap();
            result_faded |= (0.01..0.99).contains(&result.computed_opacity());
            if !enabled || !transitions {
                assert!(result.computed_opacity() > 0.99);
            }
        }
        assert_eq!(result_faded, enabled && transitions);
        settle(&app);
        for title in ["Old episode title", "Incremental result"] {
            assert!(
                ElementHandle::find_by_accessible_label(&app, &label(title))
                    .next()
                    .unwrap()
                    .computed_opacity()
                    > 0.99
            );
        }
        // A scope replacement removes the old delegate immediately. Its title
        // cannot linger beneath the next result's entrance (including a newly
        // masked spoiler), and asynchronous arrivals leave selector focus alone.
        app.set_discover_kb_zone(2);
        app.set_discover_kb_ctl(4);
        app.set_search_scope(5);
        app.set_search_results(
            Rc::new(VecModel::from(vec![card("local:bookmark:exact", "S1 E2")])).into(),
        );
        frame(&app, 1);
        assert!(
            ElementHandle::find_by_accessible_label(&app, &label("Old episode title"))
                .next()
                .is_none()
        );
        assert_eq!(
            (app.get_discover_kb_zone(), app.get_discover_kb_ctl()),
            (2, 4)
        );
        settle(&app);
        assert_eq!(
            app.get_search_results().row_data(0).unwrap().id.as_str(),
            "local:bookmark:exact"
        );
        motion.set_enabled(false);
        frame(&app, 1);
        motion.set_transitions(true);
        motion.set_enabled(true);
        for _ in 0..16 {
            frame(&app, 16);
            assert!(
                ElementHandle::find_by_accessible_label(&app, &label("S1 E2"))
                    .next()
                    .unwrap()
                    .computed_opacity()
                    > 0.99,
                "re-enabling motion must not replay an already painted result"
            );
        }
    }
}

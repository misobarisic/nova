//! Episode search remains usable beside its icon, clears results and resets paging.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}

fn click(app: &nova::AppWindow, item: &ElementHandle) {
    let origin = item.absolute_position();
    let size = item.size();
    let position = LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
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

#[test]
fn search_icon_focuses_and_clear_restores_the_episode_list() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Search episodes".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 1".into()])).into());
    app.set_season_combo_idx(0);
    app.set_season_cards(
        Rc::new(VecModel::from(
            (1..=5)
                .map(|season| nova::SeasonCard {
                    name: format!("Season {season}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let rows: slint::ModelRc<nova::EpisodeRow> = Rc::new(VecModel::from(vec![
        nova::EpisodeRow {
            text: "Pilot".into(),
            ..Default::default()
        },
        nova::EpisodeRow {
            text: "A new adventure".into(),
            ..Default::default()
        },
    ]))
    .into();
    app.set_episode_rows(rows.clone());
    app.set_episode_total(120);
    app.set_episode_page_count(3);
    app.set_episode_page(1);
    app.window().show().unwrap();

    // Mirror the bridge's synchronous filtered-result publication. The UI
    // must dispatch edits/clear and display the resulting count and page.
    let queries = Rc::new(RefCell::new(Vec::new()));
    let recorded = queries.clone();
    let weak = app.as_weak();
    app.on_episode_filter_changed(move |query| {
        recorded.borrow_mut().push(query.clone());
        let app = weak.upgrade().unwrap();
        app.set_episode_filter(query.clone());
        app.set_episode_page(0);
        app.set_episode_page_start(0);
        app.set_episode_page_count(if query.is_empty() { 3 } else { 1 });
        app.set_episode_total(if query.is_empty() { 120 } else { 0 });
        app.set_episode_rows(if query.is_empty() {
            rows.clone()
        } else {
            Default::default()
        });
    });

    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    settle();
    assert_eq!(
        element(&app, "DetailPage::episode_toolbar").size().height,
        64.0
    );
    assert!(
        ElementHandle::find_by_element_id(&app, "SearchField::inner")
            .all(|item| item.computed_opacity() < 0.1)
    );
    click(&app, &element(&app, "DetailPage::episode_search_toggle"));
    assert_eq!(
        element(&app, "DetailPage::episode_toolbar").size().height,
        120.0
    );

    for width in [320, 390, 620, 900, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        settle();
        let toolbar = element(&app, "DetailPage::episode_toolbar");
        let field = element(&app, "DetailPage::filter_line");
        let glyph = element(&app, "SearchField::search_glyph");
        let input = element(&app, "SearchField::input_viewport");
        let placeholder = element(&app, "SearchField::placeholder_label");
        assert!(
            placeholder.absolute_position().x
                >= glyph.absolute_position().x + glyph.size().width + 4.0
        );
        assert!(
            placeholder.absolute_position().x + placeholder.size().width
                <= field.absolute_position().x + field.size().width
        );
        assert!(
            input.absolute_position().x >= glyph.absolute_position().x + glyph.size().width + 4.0
        );
        for item in [&toolbar, &field] {
            assert!(item.absolute_position().x >= 0.0);
            assert!(item.absolute_position().x + item.size().width <= width as f32 + 0.5);
        }
        // Search and paging share one compact row at every breakpoint.
        let toggle = element(&app, "DetailPage::episode_search_toggle");
        let pager = element(&app, "DetailPage::episode_top_pager");
        let toggle_pos = toggle.absolute_position();
        let pager_pos = pager.absolute_position();
        assert!((toggle_pos.y - pager_pos.y).abs() < 0.5);
        assert!(toggle_pos.x + toggle.size().width <= pager_pos.x);
        assert!((toggle_pos.x - toolbar.absolute_position().x - 10.0).abs() < 0.5);
        assert!(
            (pager_pos.x + pager.size().width
                - toolbar.absolute_position().x
                - toolbar.size().width
                + 10.0)
                .abs()
                < 0.5
        );
        let count = element(&app, "DetailPage::episode_result_count");
        assert_eq!(
            count.accessible_label().as_deref(),
            Some("120 in this season")
        );
    }

    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    settle();
    click(&app, &element(&app, "SearchField::search_touch"));
    let populated_width = element(&app, "DetailPage::episode_toolbar").size().width;
    let populated_season_width = element(&app, "DetailPage::season_picker").size().width;
    for _ in 0..3 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "f".into() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "f".into() });
    }
    settle();
    assert_eq!(queries.borrow().last().map(|q| q.as_str()), Some("fff"));
    assert_eq!(app.get_episode_page(), 0);
    assert_eq!(app.get_episode_page_count(), 1);
    assert_eq!(
        element(&app, "DetailPage::episode_result_count")
            .accessible_label()
            .as_deref(),
        Some("Found: 0")
    );
    assert!(
        ElementHandle::find_by_element_id(&app, "DetailPage::episode_empty")
            .next()
            .is_some()
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "EpisodePager").count(),
        0
    );
    assert!(
        (element(&app, "DetailPage::episode_toolbar").size().width - populated_width).abs() < 0.5,
        "an empty result must not collapse the toolbar"
    );
    assert!(
        (element(&app, "DetailPage::season_picker").size().width - populated_season_width).abs()
            < 0.5,
        "an empty result must not collapse the season rail"
    );
    for width in [320, 390, 620, 900, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 1600));
        settle();
        let expected = width as f32 - if width < 700 { 40.0 } else { 344.0 };
        let toolbar = element(&app, "DetailPage::episode_toolbar");
        let empty = element(&app, "DetailPage::episode_empty");
        assert!((toolbar.size().width - expected).abs() < 0.5);
        assert!((empty.size().width - toolbar.size().width).abs() < 0.5);
        assert!(element(&app, "SearchField::input_viewport").size().width > 100.0);
        for item in [&toolbar, &empty] {
            assert!(item.absolute_position().x >= 0.0);
            assert!(item.absolute_position().x + item.size().width <= width as f32 + 0.5);
        }
        for id in [
            "DetailPage::episode_empty_title",
            "DetailPage::episode_empty_hint",
        ] {
            let copy = element(&app, id);
            assert!(copy.absolute_position().x >= empty.absolute_position().x);
            assert!(
                copy.absolute_position().x + copy.size().width
                    <= empty.absolute_position().x + empty.size().width + 0.5
            );
            assert!(copy.absolute_position().y >= empty.absolute_position().y);
            assert!(
                copy.absolute_position().y + copy.size().height
                    <= empty.absolute_position().y + empty.size().height + 0.5
            );
        }
        assert_eq!(app.get_episode_filter(), "fff");
    }
    app.window().set_size(slint::PhysicalSize::new(390, 1600));
    settle();

    // Hiding the field preserves an active query, and reopening restores it.
    click(&app, &element(&app, "DetailPage::episode_search_toggle"));
    assert_eq!(
        element(&app, "DetailPage::episode_toolbar").size().height,
        64.0
    );
    assert_eq!(app.get_episode_filter(), "fff");
    click(&app, &element(&app, "DetailPage::episode_search_toggle"));
    assert_eq!(app.get_episode_filter(), "fff");
    click(&app, &element(&app, "SearchField::clear_button"));
    assert!(app.get_episode_filter().is_empty());
    assert_eq!(queries.borrow().last().map(|q| q.as_str()), Some(""));
    assert!(
        ElementHandle::find_by_element_id(&app, "DetailPage::episode_empty")
            .next()
            .is_none()
    );
    assert_eq!(
        ElementHandle::find_by_element_id(&app, "DetailPage::ep_card").count(),
        2
    );
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "EpisodePager").count(),
        2
    );
    // The existing keyboard edit command must reveal and focus search too.
    click(&app, &element(&app, "DetailPage::episode_search_toggle"));
    app.set_detail_kb_zone(3);
    app.set_detail_kb_ci(1);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Return.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Return.into(),
        });
    settle();
    assert_eq!(
        element(&app, "DetailPage::episode_toolbar").size().height,
        120.0
    );
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "q".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "q".into() });
    settle();
    assert_eq!(queries.borrow().last().map(|q| q.as_str()), Some("q"));
}

//! Reordered built-in and addon rails must reserve their full card height.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{rc::Rc, time::Duration};

fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
}

#[test]
fn personal_home_sections_use_library_columns_and_addons_use_discover_columns() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::personal_home_sections_use_library_columns_and_addons_use_discover_columns"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    let episodes = (0..8)
        .map(|i| nova::ContinueRow {
            id: format!("episode-{i}").into(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    app.set_home_continue(Rc::new(VecModel::from(episodes.clone())).into());
    app.set_home_new_episodes(Rc::new(VecModel::from(episodes)).into());
    app.set_home_upcoming(Rc::new(VecModel::from(vec![nova::UpcomingRow::default(); 8])).into());
    app.set_home_catalog_sections(
        Rc::new(VecModel::from(vec![
            nova::HomeCatalogSection {
                title: "Previously viewed".into(),
                first_card: 0,
                card_count: 8,
                is_history: true,
            },
            // A provider can name a catalog anything; identity controls its sizing.
            nova::HomeCatalogSection {
                title: "Watch history".into(),
                first_card: 8,
                card_count: 8,
                is_history: false,
            },
        ]))
        .into(),
    );
    let cards = (0..16)
        .map(|i| nova::HomeCatalogCard {
            section_index: i / 8,
            flat_index: i,
            id: format!("card-{i}").into(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    app.set_home_catalog_cards(Rc::new(VecModel::from(cards.clone())).into());
    app.set_home_history_archive_cards(Rc::new(VecModel::from(cards[..8].to_vec())).into());
    app.set_home_catalog_order(Rc::new(VecModel::from(vec![-1, -3, -2, 0, 1])).into());
    app.set_home_catalog_positions(Rc::new(VecModel::from(vec![0, 2, 3, 4, 1])).into());
    app.show().unwrap();
    let grid_width = |view, kind| {
        app.set_home_view(view);
        settle();
        element(&app, "HomePage::sub_layer")
            .query_descendants()
            .match_type_name(kind)
            .find_first()
            .unwrap()
            .size()
            .width
    };
    let rail_widths = || {
        let sections = ElementHandle::find_by_element_id(&app, "HomePage::catalog_section")
            .collect::<Vec<_>>();
        assert_eq!(sections.len(), 2);
        let first_card_width = |section: &ElementHandle| {
            section
                .query_descendants()
                .match_type_name("HomeCatalogPosterCard")
                .find_first()
                .unwrap()
                .size()
                .width
        };
        [
            first_card_width(&sections[0]),
            first_card_width(&sections[1]),
        ]
    };
    for width in [390, 760] {
        app.window().set_size(slint::PhysicalSize::new(width, 4000));
        app.set_library_min_cols(4);
        app.set_discover_min_cols(2);
        app.set_home_view(0);
        settle();
        // The inspection API excludes cards clipped horizontally by each rail.
        let [personal_rail, addon_rail] = rail_widths();
        assert!(personal_rail < addon_rail);
        let personal_grid = grid_width(1, "ContinueCard");
        let addon_grid = grid_width(4, "HomeCatalogPosterCard");
        assert!(personal_grid < addon_grid);
        app.set_discover_min_cols(6);
        for (view, kind) in [
            (1, "ContinueCard"),
            (-3, "ContinueCard"),
            (2, "UpcomingCard"),
            (3, "HomeCatalogPosterCard"),
            (-4, "HomeCatalogPosterCard"),
        ] {
            assert!(
                (grid_width(view, kind) - personal_grid).abs() < 0.5,
                "personal view {view} at {width}px"
            );
        }
        let smaller_addon = grid_width(4, "HomeCatalogPosterCard");
        assert!(smaller_addon < addon_grid);
        app.set_home_view(0);
        settle();
        let [personal_width, addon_width] = rail_widths();
        assert!((personal_width - personal_rail).abs() < 0.5);
        assert!(addon_width < addon_rail);
        app.set_library_min_cols(2);
        for (view, kind) in [
            (1, "ContinueCard"),
            (-3, "ContinueCard"),
            (2, "UpcomingCard"),
            (3, "HomeCatalogPosterCard"),
            (-4, "HomeCatalogPosterCard"),
        ] {
            assert!(
                grid_width(view, kind) > personal_grid,
                "personal view {view} updates live"
            );
        }
        assert!((grid_width(4, "HomeCatalogPosterCard") - smaller_addon).abs() < 0.5);
    }
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle();
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let point = element.absolute_position();
    let size = element.size();
    let position =
        slint::LogicalPosition::new(point.x + size.width / 2.0, point.y + size.height / 2.0);
    let button = slint::platform::PointerEventButton::Left;
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
    settle();
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing visible element: {id}"))
}

fn assert_history_progress(cards: &[ElementHandle]) {
    for (card, fraction) in cards.iter().zip([0.4, 1.0]) {
        let track = card
            .query_descendants()
            .match_id("HomeCatalogPosterCard::history_progress")
            .find_first()
            .unwrap();
        let fill = card
            .query_descendants()
            .match_id("HomeCatalogPosterCard::history_progress_fill")
            .find_first()
            .unwrap();
        assert!((fill.absolute_position().x - track.absolute_position().x).abs() < 0.5);
        assert!((fill.absolute_position().y - track.absolute_position().y).abs() < 0.5);
        assert!((fill.size().width - track.size().width * fraction).abs() < 0.5);
        assert!((fill.size().height - track.size().height).abs() < 0.5);
    }
}

fn assert_separated(app: &nova::AppWindow, ids: &[&str]) {
    let sections: Vec<_> = ids.iter().map(|id| element(app, id)).collect();
    for section in &sections {
        assert!(
            section.size().height > 200.0,
            "rail collapsed: {:?}",
            section.size()
        );
    }
    for pair in sections.windows(2) {
        let bottom = pair[0].absolute_position().y + pair[0].size().height;
        assert!(
            pair[1].absolute_position().y >= bottom - 1.0,
            "catalog sections overlap"
        );
    }
}

#[test]
fn home_catalogs_keep_full_height_when_reordered_and_hidden() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "home_catalogs_keep_full_height_when_reordered_and_hidden"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.set_home_continue(Rc::new(VecModel::from(vec![nova::ContinueRow::default()])).into());
    app.set_home_upcoming(Rc::new(VecModel::from(vec![nova::UpcomingRow::default()])).into());
    app.set_home_catalog_sections(
        Rc::new(VecModel::from(vec![nova::HomeCatalogSection {
            title: "Addon catalog".into(),
            is_history: false,
            first_card: 0,
            card_count: 1,
        }]))
        .into(),
    );
    app.set_home_catalog_cards(
        Rc::new(VecModel::from(vec![nova::HomeCatalogCard::default()])).into(),
    );
    app.window().show().unwrap();
    for width in [1400, 390] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![-1, -2, 0])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![0, 1, 2])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &[
                "HomePage::continue_section",
                "HomePage::upcoming_section",
                "HomePage::catalog_section",
            ],
        );
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![0, -1, -2])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![1, 2, 0])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &[
                "HomePage::catalog_section",
                "HomePage::continue_section",
                "HomePage::upcoming_section",
            ],
        );
        app.set_home_continue(Rc::new(VecModel::from(Vec::<nova::ContinueRow>::new())).into());
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![0, -2])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![-1, 1, 0])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &["HomePage::catalog_section", "HomePage::upcoming_section"],
        );
        assert!(
            ElementHandle::find_by_element_id(&app, "HomePage::continue_section")
                .next()
                .is_none()
        );
        app.set_home_continue(Rc::new(VecModel::from(vec![nova::ContinueRow::default()])).into());
    }
}

#[test]
fn history_catalog_reuses_rail_grid_activation_and_keyboard_without_changing_catalog_geometry() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::history_catalog_reuses_rail_grid_activation_and_keyboard_without_changing_catalog_geometry"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.set_home_catalog_sections(
        Rc::new(VecModel::from(vec![
            nova::HomeCatalogSection {
                title: "Watch history".into(),
                is_history: true,
                first_card: 0,
                card_count: 2,
            },
            nova::HomeCatalogSection {
                title: "Ordinary catalog".into(),
                is_history: false,
                first_card: 2,
                card_count: 1,
            },
        ]))
        .into(),
    );
    app.set_home_catalog_cards(
        Rc::new(VecModel::from(vec![
            nova::HomeCatalogCard {
                section_index: 0,
                flat_index: 41,
                title: "A long history title that wraps in the regular footer".into(),
                year: "S2 E3 · A long episode caption".into(),
                history_action: "Resume".into(),
                progress: 0.4,
                ..Default::default()
            },
            nova::HomeCatalogCard {
                section_index: 0,
                flat_index: 42,
                title: "Completed movie".into(),
                history_action: "Play again".into(),
                progress: 1.0,
                ..Default::default()
            },
            nova::HomeCatalogCard {
                section_index: 1,
                flat_index: 99,
                title: "Ordinary title".into(),
                year: "2026".into(),
                // Ordinary catalogs never gain history chrome from progress alone.
                progress: 0.5,
                ..Default::default()
            },
        ]))
        .into(),
    );
    app.set_home_catalog_order(Rc::new(VecModel::from(vec![0, 1])).into());
    app.set_home_catalog_positions(Rc::new(VecModel::from(vec![-1, -1, 0, 1, -1])).into());
    let picks = Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = picks.clone();
    app.on_home_catalog_card_picked(move |index| captured.borrow_mut().push(index));
    app.window().show().unwrap();

    for width in [1400, 390] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        app.set_home_view(0);
        app.set_home_scroll_y(0.0);
        settle();
        let sections: Vec<_> =
            ElementHandle::find_by_element_id(&app, "HomePage::catalog_section").collect();
        assert_eq!(sections.len(), 2);
        assert!(sections[0].size().height > 200.0);
        assert!(
            sections[1].absolute_position().y
                >= sections[0].absolute_position().y + sections[0].size().height - 1.0
        );
        let cards: Vec<_> = ElementHandle::find_by_element_type_name(&app, "HomeCatalogPosterCard")
            .filter(|card| {
                card.computed_opacity() > 0.0 && card.absolute_position().x < width as f32
            })
            .collect();
        assert_eq!(cards.len(), 3);
        assert_history_progress(&cards[..2]);
        for card in &cards {
            assert!((card.size().height - card.size().width * 1.5 - 54.0).abs() < 1.0);
        }
        click(&app, &cards[0]);
        assert_eq!(*picks.borrow().last().unwrap(), 41);
        app.set_home_kb_zone(3);
        app.set_home_kb_idx(0);
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(*picks.borrow().last().unwrap(), 42);
        key(&app, slint::platform::Key::DownArrow);
        assert_eq!(app.get_home_kb_idx(), 2);
        key(&app, slint::platform::Key::Return);
        assert_eq!(*picks.borrow().last().unwrap(), 99);
        key(&app, slint::platform::Key::UpArrow);

        let section = element(&app, "HomePage::catalog_section");
        let header = ElementHandle::find_by_element_type_name(&app, "SectionHeader")
            .find(|header| {
                let y = header.absolute_position().y;
                y >= section.absolute_position().y - 1.0 && y < section.absolute_position().y + 50.0
            })
            .unwrap();
        click(&app, &header);
        assert_eq!(
            app.get_home_view(),
            3,
            "See all uses the generic catalog grid"
        );
        let cards: Vec<_> = ElementHandle::find_by_element_type_name(&app, "HomeCatalogPosterCard")
            .filter(|card| {
                let point = card.absolute_position();
                // The retained landing only slides 28% left while fading out;
                // its transparent rails can still have on-screen geometry.
                card.computed_opacity() > 0.0
                    && point.x >= 0.0
                    && point.x < width as f32
                    && point.y > 40.0
            })
            .collect();
        assert_eq!(cards.len(), 2);
        assert_history_progress(&cards);
        for card in &cards {
            assert!((card.size().height - card.size().width * 1.5 - 54.0).abs() < 1.0);
        }
        click(&app, &cards[0]);
        assert_eq!(*picks.borrow().last().unwrap(), 41);
        app.set_home_kb_idx(0);
        key(&app, slint::platform::Key::RightArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(*picks.borrow().last().unwrap(), 42);
        key(&app, slint::platform::Key::Back);
        assert_eq!(app.get_home_view(), 0);

        // The action and progress are poster overlays, never extra footer rows.
        for footer in
            ElementHandle::find_by_element_id(&app, "HomeCatalogPosterCard::catalog_footer")
        {
            assert!((footer.size().height - 54.0).abs() < 0.5);
            let origin = footer.absolute_position();
            for (id, top, height) in [
                ("HomeCatalogPosterCard::catalog_title", 5.0, 30.0),
                ("HomeCatalogPosterCard::catalog_caption", 35.0, 14.0),
            ] {
                let label = ElementHandle::find_by_element_id(&app, id)
                    .find(|label| {
                        let point = label.absolute_position();
                        (point.x - origin.x - 8.0).abs() < 0.5
                            && (point.y - origin.y - top).abs() < 0.5
                    })
                    .unwrap();
                assert!((label.size().height - height).abs() < 0.5);
                assert!(label.size().width <= footer.size().width - 15.5);
            }
        }
        assert_eq!(
            ElementHandle::find_by_element_id(&app, "HomeCatalogPosterCard::history_action_badge")
                .count(),
            2
        );
        assert_eq!(
            ElementHandle::find_by_element_id(&app, "HomeCatalogPosterCard::history_progress")
                .count(),
            2
        );
    }
}

#[test]
fn tv_home_featured_actions_are_directionally_reachable() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "tv_home_featured_actions_are_directionally_reachable"
    )) {
        return;
    }
    use slint::platform::Key;
    use std::cell::Cell;
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_home_featured_count(2);
    app.set_home_featured_title("Featured TV title".into());
    app.set_home_continue(Rc::new(VecModel::from(vec![nova::ContinueRow::default()])).into());
    let played = Rc::new(Cell::new(0));
    let record = played.clone();
    app.on_home_featured_watch_now(move || record.set(record.get() + 1));
    let detailed = Rc::new(Cell::new(0));
    let record = detailed.clone();
    app.on_home_featured_picked(move || record.set(record.get() + 1));
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.window().show().unwrap();
    settle();
    app.set_home_kb_zone(0);
    key(&app, Key::DownArrow);
    assert_eq!(app.get_home_kb_zone(), 6);
    key(&app, Key::Return);
    assert_eq!(played.get(), 1);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(detailed.get(), 1);
    key(&app, Key::DownArrow);
    assert_eq!(app.get_home_kb_zone(), 1);
    key(&app, Key::UpArrow);
    assert_eq!(app.get_home_kb_zone(), 6);
    key(&app, Key::UpArrow);
    assert_eq!(app.get_home_kb_zone(), 0);
}

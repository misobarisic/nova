//! Discover shares Library's collapsing hero and pinned search/filter controls.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(ms));
}

/// Evaluate deferred bindings/layout without allowing a reveal timer to fire.
fn settle_without_time(app: &nova::AppWindow) {
    for _ in 0..4 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        idle(0);
    }
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        // No renderer evaluates the restored page's layout in this backend.
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        idle(16);
    }
}

/// A timed drag followed by a stationary hold preserves a precise offset,
/// independent of native Flickable momentum.
fn drag_up(app: &nova::AppWindow, from: LogicalPosition, dy: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    // Transparent navigation reserves zero layout height while its real 68px
    // tap targets overlay the scroll viewport. Begin above that tap bar so
    // these gestures exercise scrolling rather than navigation hit testing.
    let window = app.window().size();
    let from = LogicalPosition::new(
        from.x,
        if window.width < 700 {
            from.y.min(window.height as f32 - 68.0 - 30.0)
        } else {
            from.y
        },
    );
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: from,
        button: PointerEventButton::Left,
    });
    idle(120);
    for step in 1..=4 {
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(from.x, from.y - dy * step as f32 / 4.0),
        });
        idle(20);
    }
    idle(160);
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(from.x, from.y - dy),
        button: PointerEventButton::Left,
    });
}

fn card(id: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: s(format!("id{id}").as_str()),
        title: s(format!("Title {id}").as_str()),
        year: s("2024"),
        poster_path: SharedString::default(),
        poster: Default::default(),
        is_loaded: false,
        badge: SharedString::default(),
        watched: false,
        ..Default::default()
    }
}

fn find(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

#[test]
fn discover_hero_collapses_while_search_and_filters_stay_pinned() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "discover_hero_collapses_while_search_and_filters_stay_pinned"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_catalog(Rc::new(VecModel::from((0..60).map(card).collect::<Vec<_>>())).into());
    let names = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<SharedString>>(),
        ))
        .into()
    };
    app.set_addon_names(names(&["All addons", "Cinemeta"]));
    app.set_type_names(names(&["movie", "series"]));
    app.set_catalog_names(names(&["Popular", "Top rated"]));
    app.set_genre_names(names(&["All genres", "Action"]));
    app.window().show().unwrap();
    for width in [320, 390, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        for scroll_page in [false, true] {
            app.set_discover_scroll_page(scroll_page);
            app.set_discover_scroll_y(0.0);
            settle(&app);
            let scroll = find(
                &app,
                if scroll_page {
                    "DiscoverPage::page_scroll"
                } else {
                    "DiscoverPage::grid_scroll"
                },
            );
            let pos = scroll.absolute_position();
            let size = scroll.size();
            let hero = find(&app, "DiscoverHeader::discover_hero");
            let search = find(&app, "DiscoverHeader::search_line");
            let filters = find(&app, "DiscoverHeader::filter_flick");
            let original_y = search.absolute_position().y;
            assert!(hero.absolute_position().y >= pos.y - 0.5);
            drag_up(
                &app,
                LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height - 30.0),
                280.0,
            );
            settle(&app);
            assert!(
                (app.get_discover_scroll_y() + 280.0).abs() < 2.0,
                "initial pointer drag preserves distance: width={width}, scroll_page={scroll_page}, offset={}, viewport={pos:?} {size:?}",
                app.get_discover_scroll_y()
            );
            assert!(
                hero.absolute_position().y + hero.size().height <= pos.y + 0.5,
                "hero must scroll away at {width}px, mode={scroll_page}"
            );
            let pinned_search_y = search.absolute_position().y;
            let pinned_filter_y = filters.absolute_position().y;
            assert!(pinned_search_y < original_y - 40.0);
            assert!(pinned_search_y >= pos.y - 0.5);
            assert!(pinned_filter_y > pinned_search_y);
            drag_up(
                &app,
                LogicalPosition::new(pos.x + size.width / 2.0, pos.y + size.height - 30.0),
                180.0,
            );
            settle(&app);
            assert!((search.absolute_position().y - pinned_search_y).abs() < 0.5);
            assert!((filters.absolute_position().y - pinned_filter_y).abs() < 0.5);
            assert_eq!(scroll.absolute_position(), pos);
            assert_eq!(scroll.size(), size);
            if width < 700 {
                let bottom = find(
                    &app,
                    if scroll_page {
                        "DiscoverPage::bottom_scroll"
                    } else {
                        "DiscoverPage::bottom_fix"
                    },
                );
                assert!((bottom.absolute_position().y + bottom.size().height - 900.0).abs() < 0.5);
                assert!(pos.y + size.height <= bottom.absolute_position().y + 0.5);
            }
            app.set_discover_scroll_y(0.0);
            settle(&app);
            assert!((search.absolute_position().y - original_y).abs() < 0.5);
        }
    }
}

#[test]
fn discover_viewport_retires_only_inner_controls_and_preserves_focus_and_slot_geometry() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "discover_viewport_retires_only_inner_controls_and_preserves_focus_and_slot_geometry"
    )) {
        return;
    }
    use slint::{
        Model,
        platform::{Key, WindowEvent},
    };
    use std::cell::RefCell;
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_catalog(Rc::new(VecModel::from((0..1000).map(card).collect::<Vec<_>>())).into());
    let selected = Rc::new(RefCell::new(None));
    let chosen = selected.clone();
    app.on_item_selected(move |id, index| *chosen.borrow_mut() = Some((id.to_string(), index)));
    app.show().unwrap();
    for (width, height, tv) in [
        (390, 844, false),
        (844, 390, false),
        (1280, 720, false),
        (1920, 1080, true),
    ] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for scroll_page in [false, true] {
            app.set_discover_scroll_page(scroll_page);
            app.set_kb_active(false);
            app.set_discover_kb_zone(0);
            app.set_discover_scroll_y(0.0);
            settle(&app);
            let controls = ElementHandle::find_by_accessible_label(&app, "Title 0")
                .next()
                .expect("first visible card controls");
            let slot = find(&app, "DiscoverGrid::card_slot");
            let size = slot.size();
            let position = slot.absolute_position();
            app.set_discover_scroll_y(-1_000_000.0);
            settle(&app);
            // ElementQuery omits invisible rows, so counts cannot prove native
            // retirement. A previously captured weak handle does: only the
            // inner conditional tree dies; the outer slot must remain alive.
            assert!(
                !controls.is_valid(),
                "offscreen controls retained at {width}px, page={scroll_page}"
            );
            assert!(
                slot.is_valid(),
                "outer geometry must survive unmounting controls"
            );
            assert_eq!(slot.size(), size);
            let bottom = app.get_discover_scroll_y();
            assert!(bottom < -1000.0);
            app.set_discover_scroll_y(0.0);
            settle(&app);
            let returned = ElementHandle::find_by_accessible_label(&app, "Title 0")
                .next()
                .expect("scroll-back remount");
            assert!(returned.computed_opacity() > 0.99);
            assert_eq!(slot.size(), size);
            assert!((slot.absolute_position().y - position.y).abs() < 0.5);
            // The logical target is included before keyboard focus/Enter,
            // even though it started far outside the viewport's keep window.
            app.set_kb_active(true);
            app.set_discover_kb_zone(3);
            app.set_discover_kb_idx(999);
            app.global::<nova::DiscoverBridge>().invoke_focus_shell();
            for event in [
                WindowEvent::KeyPressed {
                    text: Key::Return.into(),
                },
                WindowEvent::KeyReleased {
                    text: Key::Return.into(),
                },
            ] {
                app.window().dispatch_event(event);
            }
            assert_eq!(*selected.borrow(), Some(("id999".into(), 999)));
            settle(&app);
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Title 999")
                    .next()
                    .is_some()
            );
            assert_eq!(app.get_catalog().row_count(), 1000);
            assert!(
                (app.get_discover_scroll_y() - bottom).abs() < 2.0,
                "focus preserves full scroll extent"
            );
        }
    }
}

#[test]
fn discover_remount_preserves_one_shot_reveal_and_offscreen_motion_switch_state() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "discover_remount_preserves_one_shot_reveal_and_offscreen_motion_switch_state"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.set_show_home(false);
    app.set_discover_scroll_page(true);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    app.set_search_results(Rc::new(VecModel::from((0..1000).map(card).collect::<Vec<_>>())).into());
    app.set_discover_search_open(true);
    app.show().unwrap();
    for _ in 0..8 {
        settle(&app);
        if let Some(first) = ElementHandle::find_by_accessible_label(&app, "Title 0").next() {
            let _ = first.computed_opacity();
        }
    }
    let first = ElementHandle::find_by_accessible_label(&app, "Title 0")
        .next()
        .unwrap();
    assert!(first.computed_opacity() > 0.99);
    app.set_discover_search_scroll_y(-1_000_000.0);
    settle(&app);
    assert!(!first.is_valid());
    app.set_discover_search_scroll_y(0.0);
    // Flush the deferred native scroll/layout restoration without advancing
    // the clock: a newly armed40ms reveal must still be unable to fire.
    settle_without_time(&app);
    let returned = ElementHandle::find_by_accessible_label(&app, "Title 0")
        .next()
        .unwrap();
    assert!(returned.computed_opacity() > 0.99);
    app.set_discover_search_scroll_y(-1_000_000.0);
    settle(&app);
    app.global::<nova::Anim>().set_enabled(false);
    settle(&app);
    app.global::<nova::Anim>().set_enabled(true);
    app.set_kb_active(true);
    app.set_discover_kb_zone(3);
    app.set_discover_kb_idx(20);
    app.global::<nova::DiscoverBridge>().invoke_focus_shell();
    settle_without_time(&app);
    let settled = ElementHandle::find_by_accessible_label(&app, "Title 20")
        .next()
        .expect("focused previously unmounted row");
    assert!(
        settled.computed_opacity() > 0.99,
        "offscreen rows settled by disabling motion must not replay"
    );
}

#[test]
fn discover_pointer_modality_keeps_selected_inner_tree_while_retiring_unrelated_rows() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "discover_pointer_modality_keeps_selected_inner_tree_while_retiring_unrelated_rows"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_catalog(Rc::new(VecModel::from((0..1000).map(card).collect::<Vec<_>>())).into());
    app.set_kb_active(true);
    app.set_discover_kb_zone(3);
    app.set_discover_kb_idx(0);
    app.show().unwrap();
    settle(&app);
    let selected = ElementHandle::find_by_accessible_label(&app, "Title 0")
        .next()
        .unwrap();
    let unrelated = ElementHandle::find_by_accessible_label(&app, "Title 1")
        .next()
        .unwrap();
    // Pointer modality hides the ring; it does not discard the selected
    // logical target that Detail/back or later keyboard focus can restore.
    app.set_kb_active(false);
    app.set_discover_scroll_y(-1_000_000.0);
    settle(&app);
    assert!(app.get_discover_scroll_y() < -1000.0);
    assert!(
        selected.is_valid(),
        "selected controls must survive pointer modality outside the viewport"
    );
    assert!(
        !unrelated.is_valid(),
        "unrelated offscreen controls must still retire"
    );
    assert_eq!(app.get_discover_kb_zone(), 3);
    assert_eq!(app.get_discover_kb_idx(), 0);
    assert!(!app.get_kb_active());
    app.set_discover_kb_zone(0);
    settle(&app);
    assert!(
        !selected.is_valid(),
        "clearing logical focus releases the extra retained tree"
    );
}

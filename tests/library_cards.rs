//! Responsive library cards show progress only for watched episodes.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle() {
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
}
fn elements(app: &nova::AppWindow, id: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_id(app, &format!("LibraryPage::{id}")).collect()
}
fn click(_app: &nova::AppWindow, item: ElementHandle) {
    item.mock_single_click(slint::platform::PointerEventButton::Left);
    settle();
}
fn label(app: &nova::AppWindow, text: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, text)
        .last()
        .expect(text)
}

#[test]
fn library_cards_fit_and_dispatch_progress_search_views_and_menus() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_touch_menus(true);
    app.set_library(
        Rc::new(VecModel::from(
            [
                (0, 13, "Plan to Watch"),
                (5, 13, "Watching"),
                (13, 13, "Completed"),
                (0, 0, "Watching"),
            ]
            .into_iter()
            .enumerate()
            .map(
                |(i, (watched_count, episode_count, status))| nova::MediaCard {
                    id: format!("s{i}").into(),
                    title: "A long series title that must stay inside the card".into(),
                    year: "2026".into(),
                    media_type: "TV".into(),
                    status: status.into(),
                    badge: if i == 1 { "▶ Resume New Days" } else { "" }.into(),
                    watched_count,
                    episode_count,
                    watched: i == 2,
                    ..Default::default()
                },
            )
            .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.window().show().unwrap();
    for list in [false, true] {
        app.set_library_list_view(list);
        for width in [320, 390, 620, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 2000));
            settle();
            let cards = elements(&app, "library_card");
            assert_eq!(cards.len(), 4);
            if width < 700 {
                let bottom = elements(&app, "bottom").remove(0);
                assert!(bottom.computed_opacity() > 0.99);
                assert!((bottom.absolute_position().y + bottom.size().height - 2000.0).abs() < 0.5);
            }
            assert_eq!(elements(&app, "library_progress").len(), 2);
            assert_eq!(elements(&app, "library_progress_track").len(), 2);
            for ((track, fill), fraction) in elements(&app, "library_progress_track")
                .into_iter()
                .zip(elements(&app, "library_progress_fill"))
                .zip([5.0 / 13.0, 1.0])
            {
                assert!((fill.absolute_position().x - track.absolute_position().x).abs() < 0.5);
                assert!((fill.absolute_position().y - track.absolute_position().y).abs() < 0.5);
                assert!((fill.size().width - track.size().width * fraction).abs() < 0.5);
            }
            for (track, count) in elements(&app, "library_progress_track")
                .into_iter()
                .zip(elements(&app, "library_episode_count"))
            {
                let rail_center = track.absolute_position().y + track.size().height / 2.0;
                let count_center = count.absolute_position().y + count.size().height / 2.0;
                assert!(
                    (rail_center - count_center).abs() < 0.5,
                    "progress rail and count must align at {width}px, list={list}"
                );
            }
            let counts: Vec<_> = elements(&app, "library_episode_count")
                .iter()
                .filter_map(|e| e.accessible_label())
                .collect();
            assert_eq!(counts, ["5 / 13", "13 / 13"]);
            for name in [
                "library_title",
                "library_metadata",
                "library_episode_count",
                "library_caption",
                "library_card_menu",
            ] {
                for child in elements(&app, name) {
                    let p = child.absolute_position();
                    let s = child.size();
                    assert!(
                        cards.iter().any(|c| {
                            let cp = c.absolute_position();
                            let cs = c.size();
                            p.x >= cp.x - 0.5
                                && p.y >= cp.y - 0.5
                                && p.x + s.width <= cp.x + cs.width + 0.5
                                && p.y + s.height <= cp.y + cs.height + 0.5
                        }),
                        "{name} must fit at {width}px, list={list}"
                    );
                }
            }
        }
    }
    // The view controls are real, and opening a card still uses its model index.
    app.window().set_size(slint::PhysicalSize::new(390, 1000));
    settle();
    click(&app, label(&app, "Grid view"));
    assert!(!app.get_library_list_view());
    click(&app, label(&app, "List view"));
    assert!(app.get_library_list_view());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let record = opened.clone();
    app.on_library_item_picked(move |i| record.borrow_mut().push(i));
    click(&app, elements(&app, "library_title").remove(1));
    assert_eq!(*opened.borrow(), [1]);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let record = actions.clone();
    app.on_library_watch_action(move |i, a| record.borrow_mut().push((i, a)));
    click(&app, elements(&app, "library_card_menu").remove(1));
    click(&app, label(&app, "Mark series as watched"));
    assert_eq!(*actions.borrow(), [(1, 1)]);
    assert_eq!(*opened.borrow(), [1], "menu taps must not open the title");
    click(&app, label(&app, "Search library"));
    assert!(app.get_library_search_open());
    app.set_library_query("test".into());
    let changes = Rc::new(RefCell::new(0));
    let record = changes.clone();
    app.on_library_view_changed(move || *record.borrow_mut() += 1);
    click(&app, elements(&app, "library_search_button").remove(0));
    assert!(!app.get_library_search_open());
    assert!(app.get_library_query().is_empty());
    assert_eq!(*changes.borrow(), 1);

    // Sorting and text edits must reach the backend refresh callback.
    click(&app, elements(&app, "library_sort").remove(0));
    click(&app, label(&app, "Title"));
    assert_eq!(app.get_library_sort(), 1);
    assert_eq!(*changes.borrow(), 2);
    click(&app, elements(&app, "library_search_button").remove(0));
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "f".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "f".into() });
    settle();
    assert_eq!(app.get_library_query(), "f");
    assert_eq!(*changes.borrow(), 3);
    // Android Back closes the search before leaving the library.
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle();
    assert!(!app.get_library_search_open());
    assert!(app.get_library_query().is_empty());
    assert_eq!(*changes.borrow(), 4);

    click(&app, elements(&app, "library_card_menu").remove(1));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark series as watched")
            .next()
            .is_some()
    );
    // A filtered/sorted refresh may reuse the slot for a different identity.
    // The pending sheet must close instead of applying its action to that row.
    let model = app.get_library();
    let replacement = vec![model.row_data(1).unwrap(), model.row_data(0).unwrap()];
    app.set_library(Rc::new(VecModel::from(replacement)).into());
    settle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Mark series as watched")
            .next()
            .is_none()
    );
    assert_eq!(*actions.borrow(), [(1, 1)]);
}

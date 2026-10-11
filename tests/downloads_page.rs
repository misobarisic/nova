//! The device Downloads page dispatches stable IDs at phone and desktop widths.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn click(app: &nova::AppWindow, label: &str) {
    let element = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap();
    click_element(app, &element);
}

fn click_element(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let size = element.size();
    assert!(size.height >= 40.0);
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
}

fn key(app: &nova::AppWindow, text: SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
}

#[test]
fn completed_episode_actions_keep_job_identity_across_layouts() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_downloads(true);
    app.set_downloads_page_counts(Rc::new(VecModel::from(vec![1, 0, 1, 0])).into());
    app.set_downloads_page_summary("1 download · 1 series · 42 MiB".into());
    app.set_downloads_page_groups(
        Rc::new(VecModel::from(vec![nova::DownloadGroup {
            id: "series-season-2".into(),
            title: "Downloaded series".into(),
            subtitle: "Season 2 · 1 · 42 MiB".into(),
            episodes: Rc::new(VecModel::from(vec![nova::DownloadEpisode {
                id: "download-stable-job".into(),
                title: "Real episode title".into(),
                badge: "S2 E3".into(),
                details: "42 MiB".into(),
                state: 1,
                progress: 0.42,
                action_label: "Resume".into(),
                action_kind: 4,
            }]))
            .into(),
            ..Default::default()
        }]))
        .into(),
    );
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_downloads_page_action(move |id, kind| recorded.borrow_mut().push((id, kind)));
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let recorded = toggles.clone();
    app.on_downloads_page_group_toggled(move |id| recorded.borrow_mut().push(id));
    let filters = Rc::new(RefCell::new(Vec::new()));
    let recorded = filters.clone();
    app.on_downloads_page_filter_changed(move |filter| recorded.borrow_mut().push(filter));
    let sorts = Rc::new(RefCell::new(Vec::new()));
    let recorded = sorts.clone();
    app.on_downloads_page_sort_changed(move |recent| recorded.borrow_mut().push(recent));
    let queries = Rc::new(RefCell::new(Vec::new()));
    let recorded = queries.clone();
    app.on_downloads_page_query_changed(move |query| recorded.borrow_mut().push(query));
    let backs = Rc::new(RefCell::new(0));
    let recorded = backs.clone();
    app.on_home_picked(move || *recorded.borrow_mut() += 1);
    for width in [390, 650, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.window().show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "BottomNav").count(),
            usize::from(width < 700)
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "SideNav").count(),
            usize::from(width >= 700)
        );
        for component in ["DownloadSeriesCard", "DownloadEpisodeRow"] {
            for element in ElementHandle::find_by_element_type_name(&app, component) {
                let p = element.absolute_position();
                assert!(p.x >= 0.0);
                assert!(p.x + element.size().width <= width as f32 + 1.0);
            }
        }
        for label in ["All 1", "Unwatched 0", "In progress 1", "Watched 0"] {
            click(&app, label);
        }
        if width < 700 {
            let before_back = *backs.borrow();
            click(&app, "Back");
            assert_eq!(*backs.borrow(), before_back + 1, "header Back routes Home");
        }
        assert_eq!(
            filters.borrow().last(),
            Some(&3),
            "every visible filter must dispatch its index"
        );
        click(&app, "Newest first");
        let search = ElementHandle::find_by_element_type_name(&app, "SearchField")
            .next()
            .unwrap();
        click_element(&app, &search);
        key(&app, "x".into());
        assert_eq!(app.get_downloads_page_query().as_str(), "x");
        let before_back = *backs.borrow();
        key(&app, slint::platform::Key::Backspace.into());
        assert_eq!(app.get_downloads_page_query().as_str(), "");
        assert_eq!(
            *backs.borrow(),
            before_back,
            "text Backspace must not leave Downloads"
        );
        key(&app, slint::platform::Key::Escape.into());
        assert_eq!(
            *backs.borrow(),
            before_back + 1,
            "Escape from search routes Back"
        );
        click(&app, "Resume");
        // The timer updates episode models in place: the existing focused
        // Resume control must continue to activate after a progress update.
        let episodes = app
            .get_downloads_page_groups()
            .row_data(0)
            .unwrap()
            .episodes;
        let mut episode = episodes.row_data(0).unwrap();
        episode.progress = 0.6;
        episodes.set_row_data(0, episode);
        key(&app, slint::platform::Key::Return.into());
        key(&app, slint::platform::Key::Escape.into());
        assert_eq!(
            *backs.borrow(),
            before_back + 2,
            "Escape from control routes Back"
        );
        click(&app, "Remove download");
        click(&app, "Collapse series");
    }
    assert_eq!(
        filters.borrow().as_slice(),
        &[0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]
    );
    assert_eq!(sorts.borrow().as_slice(), &[false, false, false]);
    assert!(!queries.borrow().is_empty());
    let expected_actions = (0..3)
        .flat_map(|_| {
            [
                (SharedString::from("download-stable-job"), 0),
                (SharedString::from("download-stable-job"), 0),
                (SharedString::from("download-stable-job"), 1),
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(actions.borrow().as_slice(), expected_actions.as_slice());
    assert_eq!(
        toggles.borrow().as_slice(),
        &[
            SharedString::from("series-season-2"),
            SharedString::from("series-season-2"),
            SharedString::from("series-season-2")
        ]
    );
    assert_eq!(*backs.borrow(), 8);
}

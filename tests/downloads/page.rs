//! The device Downloads page dispatches stable IDs at phone and desktop widths.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn click(app: &nova::AppWindow, label: &str) {
    let element = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap_or_else(|| {
            panic!(
                "missing Downloads control {label:?} at {:?}; downloads mounted: {}",
                app.window().size(),
                app.get_show_downloads()
            )
        });
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

// Back really leaves this root destination. Re-enter it before exercising
// another control; callbacks that record Home dispatch do not keep it mounted.
fn reopen_downloads(app: &nova::AppWindow) {
    app.invoke_navigate_section(5);
    let _ = ElementHandle::find_by_element_type_name(app, "DownloadsPage")
        .next()
        .expect("Downloads remounted after Back")
        .size();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
}

fn key(app: &nova::AppWindow, text: SharedString) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
}

#[test]
fn completed_episode_actions_keep_job_identity_across_layouts() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "completed_episode_actions_keep_job_identity_across_layouts"
    )) {
        return;
    }
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
    let weak = app.as_weak();
    app.on_downloads_page_filter_changed(move |filter| {
        recorded.borrow_mut().push(filter);
        weak.upgrade().unwrap().set_downloads_page_filter(filter);
    });
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
            0,
            "Downloads hides the phone bar and retains its header Back control"
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
        for (index, label) in ["All 1", "Unwatched 0", "In progress 1", "Watched 0"]
            .into_iter()
            .enumerate()
        {
            // The horizontal rail reveals a selected chip, including filters
            // restored by the backend or carried across a phone rotation.
            app.set_downloads_page_filter(index as i32);
            // The compiled page bridge publishes selected changes on the UI
            // tick; wait for its reveal callback before measuring the pill.
            for _ in 0..2 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
            }
            let chip = ElementHandle::find_by_accessible_label(&app, label)
                .next()
                .unwrap();
            let p = chip.absolute_position();
            assert_eq!(chip.size().height, 40.0, "shared Library pill height");
            let rail = ElementHandle::find_by_element_id(&app, "DownloadsPage::filter_rail")
                .next()
                .unwrap();
            let rail_left = rail.absolute_position().x;
            assert!(
                p.x >= rail_left && p.x + chip.size().width <= rail_left + rail.size().width + 1.0,
                "{label} at width {width}: {p:?}, {:?}",
                chip.size()
            );
            click_element(&app, &chip);
        }
        assert_eq!(
            ElementHandle::find_by_accessible_label(&app, "Back").count(),
            usize::from(width < 700),
            "sidebar destinations omit the header Back arrow"
        );
        key(&app, slint::platform::Key::Return.into());
        assert_eq!(
            filters.borrow().last(),
            Some(&3),
            "focused pill activates with Return"
        );
        if width < 700 {
            let before_back = *backs.borrow();
            click(&app, "Back");
            assert_eq!(*backs.borrow(), before_back + 1, "header Back routes Home");
            assert!(!app.get_show_downloads());
            reopen_downloads(&app);
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
        assert!(!app.get_show_downloads());
        reopen_downloads(&app);
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
        assert!(!app.get_show_downloads());
        reopen_downloads(&app);
        click(&app, "Remove download");
        click(&app, "Collapse series");
        let groups = app.get_downloads_page_groups();
        let mut group = groups.row_data(0).unwrap();
        group.collapsed = true;
        groups.set_row_data(0, group.clone());
        let chevron = ElementHandle::find_by_element_type_name(&app, "IcChevronDown")
            .next()
            .unwrap();
        assert_eq!(
            chevron.size().width,
            24.0,
            "collapse control uses a vector icon"
        );
        group.collapsed = false;
        groups.set_row_data(0, group);
    }
    assert_eq!(
        filters.borrow().as_slice(),
        &[0, 1, 2, 3, 3, 0, 1, 2, 3, 3, 0, 1, 2, 3, 3]
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

#[test]
fn group_expansion_retargets_without_squeezing_rows_or_animating_model_updates() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "group_expansion_retargets_without_squeezing_rows_or_animating_model_updates"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_downloads(true);
    let episodes = Rc::new(VecModel::from(vec![
        nova::DownloadEpisode {
            id: "episode".into(),
            title: "Episode".into(),
            action_label: "Play".into(),
            ..Default::default()
        };
        2
    ]));
    let groups = Rc::new(VecModel::from(vec![nova::DownloadGroup {
        id: "group".into(),
        title: "Series".into(),
        episodes: episodes.clone().into(),
        ..Default::default()
    }]));
    app.set_downloads_page_groups(groups.clone().into());
    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = actions.clone();
    app.on_downloads_page_action(move |id, kind| recorded.borrow_mut().push((id, kind)));
    let height = || {
        ElementHandle::find_by_element_type_name(&app, "DownloadSeriesCard")
            .next()
            .unwrap()
            .size()
            .height
    };
    let advance = |ms| {
        for _ in 0..ms {
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
            let _ = height();
            let card = ElementHandle::find_by_element_type_name(&app, "DownloadSeriesCard")
                .next()
                .unwrap();
            let toggle = ElementHandle::find_by_accessible_label(
                &app,
                if groups.row_data(0).unwrap().collapsed {
                    "Expand series"
                } else {
                    "Collapse series"
                },
            )
            .next()
            .unwrap();
            let card_top = card.absolute_position().y;
            let toggle_top = toggle.absolute_position().y;
            assert!(
                toggle_top >= card_top && toggle_top + toggle.size().height <= card_top + 86.0,
                "chevron must stay inside the fixed header throughout expansion: card top={card_top}, toggle top={toggle_top}"
            );
        }
    };
    let collapse = |collapsed| {
        let mut group = groups.row_data(0).unwrap();
        group.collapsed = collapsed;
        groups.set_row_data(0, group);
    };
    app.show().unwrap();
    for (width, viewport_height) in [(390, 900), (820, 390), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, viewport_height));
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_transitions(true);
        advance(300);
        assert_eq!(height(), 250.0);
        collapse(true);
        advance(70);
        assert!(
            height() > 86.0 && height() < 250.0,
            "collapse must be gradual"
        );
        for row in ElementHandle::find_by_element_type_name(&app, "DownloadEpisodeRow") {
            assert_eq!(row.size().height, 82.0, "clipped rows keep their geometry");
        }
        if let Some(play) = ElementHandle::find_by_accessible_label(&app, "Play").next() {
            click_element(&app, &play);
            assert!(actions.borrow().is_empty(), "closing rows are inert");
        }
        let halfway = height();
        collapse(false);
        advance(70);
        assert!(
            height() > halfway && height() < 250.0,
            "reopening reverses the exit"
        );
        advance(300);
        assert_eq!(height(), 250.0);
        // Periodic backend progress publication must keep focus and height.
        let mut row = episodes.row_data(0).unwrap();
        row.progress = 0.6;
        episodes.set_row_data(0, row);
        advance(1);
        assert_eq!(height(), 250.0);
        for (master, transitions) in [(true, false), (false, true)] {
            app.global::<nova::Anim>().set_enabled(master);
            app.global::<nova::Anim>().set_transitions(transitions);
            collapse(true);
            advance(1);
            assert_eq!(height(), 86.0, "disabled motion collapses immediately");
            collapse(false);
            advance(1);
            assert_eq!(height(), 250.0);
        }
        app.global::<nova::Anim>().set_enabled(true);
        app.global::<nova::Anim>().set_transitions(true);
        collapse(true);
        advance(60);
        app.global::<nova::Anim>().set_enabled(false);
        advance(1);
        assert_eq!(height(), 86.0, "disabling mid-exit snaps to the target");
        collapse(false);
        advance(1);
    }
}

#[test]
#[ignore = "requires a desktop display; SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn chevron_rotation_paints_intermediate_frames_inside_the_header() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "chevron_rotation_paints_intermediate_frames_inside_the_header"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    app.set_show_home(false);
    app.set_show_downloads(true);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    let groups = Rc::new(VecModel::from(vec![nova::DownloadGroup {
        id: "series".into(),
        title: "Downloaded series".into(),
        episodes: Rc::new(VecModel::from(vec![nova::DownloadEpisode {
            id: "episode".into(),
            title: "Episode".into(),
            action_label: "Play".into(),
            ..Default::default()
        }]))
        .into(),
        ..Default::default()
    }]));
    app.set_downloads_page_groups(groups.clone().into());
    app.show().unwrap();
    let weak = app.as_weak();
    let frames = Rc::new(RefCell::new(Vec::<Vec<bool>>::new()));
    let started = Rc::new(std::cell::Cell::new(None::<std::time::Instant>));
    let attempts = Rc::new(std::cell::Cell::new(0));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(25),
        move || {
            let app = weak.upgrade().unwrap();
            if started.get().is_none() {
                attempts.set(attempts.get() + 1);
                assert!(attempts.get() < 40, "chevron fixture never painted");
                // The GPU canvas is created on the first presentation. Let
                // that and any page entrance finish before sampling rotation.
                if attempts.get() < 14 {
                    app.window().request_redraw();
                    return;
                }
            }
            let collapsed = groups.row_data(0).unwrap().collapsed;
            let toggle = ElementHandle::find_by_accessible_label(
                &app,
                if collapsed {
                    "Expand series"
                } else {
                    "Collapse series"
                },
            )
            .next()
            .unwrap();
            let p = toggle.absolute_position();
            let card = ElementHandle::find_by_element_type_name(&app, "DownloadSeriesCard")
                .next()
                .unwrap();
            assert!(
                p.y >= card.absolute_position().y
                    && p.y + toggle.size().height <= card.absolute_position().y + 86.0
            );
            let snapshot = app.window().take_snapshot().unwrap();
            let scale = app.window().scale_factor();
            // Crop the stationary button interior, excluding its border and title.
            // Comparing painted masks catches a snapping/non-rendered rotation,
            // which inspecting the animation property alone cannot establish.
            let x = ((p.x + 6.0) * scale).round() as u32;
            let y = ((p.y + 6.0) * scale).round() as u32;
            let side = (32.0 * scale).round() as u32;
            let mask = (y..y + side)
                .flat_map(|row| (x..x + side).map(move |col| (row, col)))
                .map(|(row, col)| {
                    let pixel = snapshot.as_slice()[(row * snapshot.width() + col) as usize];
                    pixel.r > 200 && pixel.g > 200 && pixel.b > 200
                })
                .collect::<Vec<_>>();
            if started.get().is_none() {
                if mask.iter().filter(|pixel| **pixel).count() < 8 {
                    return;
                }
                frames.borrow_mut().push(mask);
                started.set(Some(std::time::Instant::now()));
                let mut group = groups.row_data(0).unwrap();
                group.collapsed = true;
                groups.set_row_data(0, group);
            } else {
                frames.borrow_mut().push(mask);
                if started.get().unwrap().elapsed() >= std::time::Duration::from_millis(350) {
                    let frames = frames.borrow();
                    let first = frames.first().unwrap();
                    let last = frames.last().unwrap();
                    assert_ne!(first, last, "expanded and collapsed chevrons differ");
                    assert!(
                        frames.iter().any(|frame| frame != first && frame != last),
                        "GPU must paint intermediate chevron angles"
                    );
                    slint::quit_event_loop().unwrap();
                }
            }
            app.window().request_redraw();
        },
    );
    app.run().unwrap();
}

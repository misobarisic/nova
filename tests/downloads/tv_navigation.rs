//! Calendar and Downloads remote control paths require no Tab key or pointer.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{
    ComponentHandle, VecModel,
    platform::{Key, WindowEvent},
};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..6 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.absolute_position(), element.size());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
}
fn key(app: &nova::AppWindow, code: Key) {
    let text: slint::SharedString = code.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}
fn setup() -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app
}

#[test]
fn downloads_arrows_reach_filters_sort_series_episode_actions_and_transfers() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::downloads_arrows_reach_filters_sort_series_episode_actions_and_transfers"
    )) {
        return;
    }
    let app = setup();
    app.set_downloads_page_counts(Rc::new(VecModel::from(vec![12, 12, 0, 0])).into());
    app.set_downloads_page_groups(
        Rc::new(VecModel::from(
            (0..12)
                .map(|index| nova::DownloadGroup {
                    id: format!("series-{index}").into(),
                    title: format!("Series {index}").into(),
                    episodes: Rc::new(VecModel::from(vec![nova::DownloadEpisode {
                        id: format!("episode-{index}").into(),
                        title: format!("Episode {index}").into(),
                        action_label: format!("Play {index}").into(),
                        ..Default::default()
                    }]))
                    .into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_downloads_page_transfers(
        Rc::new(VecModel::from(vec![nova::DownloadEpisode {
            id: "transfer".into(),
            action_label: "Pause transfer".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let actions = Rc::new(RefCell::new(Vec::new()));
    let captured = actions.clone();
    app.on_downloads_page_action(move |id, kind| {
        captured.borrow_mut().push((id.to_string(), kind))
    });
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let captured = toggles.clone();
    app.on_downloads_page_group_toggled(move |id| captured.borrow_mut().push(id.to_string()));
    let filters = Rc::new(RefCell::new(Vec::new()));
    let captured = filters.clone();
    app.on_downloads_page_filter_changed(move |index| captured.borrow_mut().push(index));
    let sorts = Rc::new(RefCell::new(Vec::new()));
    let captured = sorts.clone();
    app.on_downloads_page_sort_changed(move |recent| captured.borrow_mut().push(recent));
    app.invoke_navigate_section(5);
    app.show().unwrap();
    settle(&app);
    key(&app, Key::DownArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(&*filters.borrow(), &[1]);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(sorts.borrow().len(), 1);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(&*toggles.borrow(), &["series-0"]);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(
        &*actions.borrow(),
        &[("episode-0".into(), 0), ("episode-0".into(), 1)]
    );
    // Every following header and episode remains reachable as the list scrolls.
    for _ in 0..22 {
        key(&app, Key::DownArrow);
    }
    let last = ElementHandle::find_by_accessible_label(&app, "Play 11")
        .next()
        .unwrap();
    let pos = last.absolute_position();
    let size = last.size();
    assert!(
        pos.y >= 0.0 && pos.y + size.height <= 720.0,
        "focused episode action must scroll into view: {pos:?} {size:?}"
    );
    key(&app, Key::Return);
    assert_eq!(actions.borrow().last(), Some(&("episode-11".into(), 0)));
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(actions.borrow().last(), Some(&("transfer".into(), 0)));
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(actions.borrow().last(), Some(&("transfer".into(), 1)));
}

#[test]
fn failed_downloads_offer_reselection_in_groups_and_transfers() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::failed_downloads_offer_reselection_in_groups_and_transfers"
    )) {
        return;
    }
    let app = setup();
    let failed = |id: &str| nova::DownloadEpisode {
        id: id.into(),
        title: "Episode with a failed stream".into(),
        action_kind: 3,
        action_label: "Retry download".into(),
        ..Default::default()
    };
    app.set_downloads_page_groups(
        Rc::new(VecModel::from(vec![nova::DownloadGroup {
            id: "series".into(),
            title: "Series".into(),
            failed_count: 1,
            episodes: Rc::new(VecModel::from(vec![failed("group-episode")])).into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_downloads_page_transfers(Rc::new(VecModel::from(vec![failed("transfer")])).into());
    let actions = Rc::new(RefCell::new(Vec::new()));
    let captured = actions.clone();
    app.on_downloads_page_action(move |id, kind| {
        captured.borrow_mut().push((id.to_string(), kind))
    });
    app.invoke_navigate_section(5);
    app.show().unwrap();
    settle(&app);
    let recovery = ElementHandle::find_by_accessible_label(&app, "Choose another stream")
        .next()
        .expect("failed episode recovery action");
    assert!(recovery.size().height >= 56.0);
    recovery.mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(actions.borrow().last(), Some(&("group-episode".into(), 2)));
    app.recover_back_focus();
    // Search -> filters -> sort -> group heading -> failed episode.
    for _ in 0..4 {
        key(&app, Key::DownArrow);
    }
    key(&app, Key::RightArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(actions.borrow().last(), Some(&("group-episode".into(), 2)));
    key(&app, Key::DownArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(actions.borrow().last(), Some(&("transfer".into(), 2)));
}

#[test]
fn calendar_arrows_select_view_day_and_release() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::calendar_arrows_select_view_day_and_release"
    )) {
        return;
    }
    let app = setup();
    app.set_calendar_week(
        Rc::new(VecModel::from(
            (0..7)
                .map(|index| nova::CalendarDay {
                    cell: nova::CalCell {
                        day: index + 1,
                        epoch: 100 + index,
                        in_month: true,
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_calendar_rows(
        Rc::new(VecModel::from(vec![nova::CalendarRelease {
            key: "release-key".into(),
            title: "Remote release".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let days = Rc::new(RefCell::new(Vec::new()));
    let captured = days.clone();
    app.on_calendar_pick(move |epoch| captured.borrow_mut().push(epoch));
    let releases = Rc::new(RefCell::new(Vec::new()));
    let captured = releases.clone();
    app.on_calendar_release_picked(move |id| captured.borrow_mut().push(id.to_string()));
    app.invoke_navigate_section(4);
    app.show().unwrap();
    settle(&app);
    key(&app, Key::RightArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(app.get_calendar_view(), 1);
    key(&app, Key::DownArrow);
    key(&app, Key::RightArrow);
    key(&app, Key::Return);
    assert_eq!(&*days.borrow(), &[101]);
    key(&app, Key::DownArrow);
    key(&app, Key::Return);
    assert_eq!(&*releases.borrow(), &["release-key"]);
}

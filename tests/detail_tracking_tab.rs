//! Dedicated Tracking navigation, responsive confirmation, and service changes.
//! Set NOVA_TRACKING_PREVIEW_DIR to render the same fixture with a desktop backend.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let timer_ready = ready.clone();
    let timer_waker = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(350), move || {
        timer_ready.set(true);
        if let Some(waker) = timer_waker.borrow_mut().take() {
            waker.wake();
        }
    });
    std::future::poll_fn(|cx| {
        if ready.get() {
            std::task::Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
    .await;
}

async fn scroll_to_end(app: &nova::AppWindow) {
    let scroll = ElementHandle::find_by_element_id(app, "DetailPage::det_scroll")
        .next()
        .unwrap();
    let p = scroll.absolute_position();
    let size = scroll.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0),
            delta_x: 0.0,
            delta_y: -10_000.0,
        });
    settle().await;
}

async fn click_tab(app: &nova::AppWindow, label: &str) {
    // The tabs now scroll with the shared Detail hero.
    let scroll = ElementHandle::find_by_element_id(app, "DetailPage::det_scroll")
        .next()
        .unwrap();
    let p = scroll.absolute_position();
    let size = scroll.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0),
            delta_x: 0.0,
            delta_y: 10_000.0,
        });
    settle().await;
    let label = label.to_string();
    let tabs = ElementHandle::find_by_element_type_name(app, "DetailTabs")
        .find(|e| e.size().height > 0.0 && e.absolute_position().y < 800.0)
        .expect("visible detail tabs");
    tabs.query_descendants()
        .match_predicate(move |e: &ElementHandle| {
            e.accessible_label().as_deref() == Some(label.as_str())
                && e.accessible_role() == Some(AccessibleRole::Button)
        })
        .find_first()
        .expect("tab button")
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
}

#[test]
fn tracking_is_a_detail_tab_with_reachable_confirmation() {
    let preview_dir = std::env::var_os("NOVA_TRACKING_PREVIEW_DIR").map(std::path::PathBuf::from);
    if preview_dir.is_none() {
        i_slint_backend_testing::init_integration_test_with_system_time();
    }
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_in_library(true);
    app.set_detail_tab(3);
    app.set_selected_title("Demon Slayer: Kimetsu no Yaiba".into());
    app.set_selected_year("2019–".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 2".into()])).into());
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.window().show().unwrap();

    let shows = Rc::new(Cell::new(0));
    let recorded_shows = shows.clone();
    let weak = app.as_weak();
    app.on_tracking_show(move || {
        recorded_shows.set(recorded_shows.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_tracking_return_tab(app.get_detail_tab());
        app.set_tracking_open(true);
        app.set_detail_tab(4);
    });
    let weak = app.as_weak();
    app.on_detail_tab_picked(move |tab| {
        let app = weak.upgrade().unwrap();
        if tab == 4 {
            app.invoke_tracking_show();
        } else {
            app.invoke_tracking_close();
            app.set_detail_tab(tab);
        }
    });
    let weak = app.as_weak();
    app.on_tracking_close(move || {
        let app = weak.upgrade().unwrap();
        app.set_tracking_open(false);
        app.set_detail_tab(app.get_tracking_return_tab());
    });
    let searches = Rc::new(RefCell::new(Vec::new()));
    let recorded_searches = searches.clone();
    app.on_tracking_search(move |service, query| {
        recorded_searches
            .borrow_mut()
            .push((service, query.to_string()));
    });
    let accepts = Rc::new(RefCell::new(Vec::new()));
    let recorded_accepts = accepts.clone();
    app.on_tracking_accept_setup(move |revision, history| {
        recorded_accepts
            .borrow_mut()
            .push((revision.to_string(), history));
    });

    let weak = app.as_weak();
    let recorded_accepts = accepts.clone();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        settle().await;
        let tabs_y = ElementHandle::find_by_element_type_name(&app, "DetailTabs")
            .next()
            .unwrap()
            .absolute_position()
            .y;
        click_tab(&app, "Tracking").await;
        let tracking_tabs_y = ElementHandle::find_by_element_type_name(&app, "DetailTabs")
            .next()
            .unwrap()
            .absolute_position()
            .y;
        assert!((tracking_tabs_y - tabs_y).abs() < 1.0, "Tracking preserves the shared hero height");
        assert_eq!(app.get_detail_tab(), 4);
        assert!(app.get_tracking_open());
        assert_eq!(app.get_tracking_return_tab(), 3);
        assert_eq!(shows.get(), 1);
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "TrackingPanel").count(),
            1
        );
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "TrackingSheet").count(),
            0
        );
        app.set_tracking_setup_active(true);
        app.set_tracking_setup_revision("season-2".into());
        app.set_tracking_setup_summary("2 releases · 18 episodes · 0 unmapped".into());
        app.set_tracking_setup(
            Rc::new(VecModel::from(vec![
                nova::TrackingSetupRow {
                    title: "Mugen Train Arc".into(),
                    season: "Season 2".into(),
                    source_range: "1–7".into(),
                    target_range: "1–7".into(),
                    reason: "Check this split".into(),
                    history: "Saved watched progress: at least 7".into(),
                    ..Default::default()
                },
                nova::TrackingSetupRow {
                    title: "Entertainment District Arc".into(),
                    season: "Season 2".into(),
                    source_range: "8–18".into(),
                    target_range: "1–11".into(),
                    reason: "Check this split".into(),
                    history: "Saved watched progress: at least 3".into(),
                    ..Default::default()
                },
            ]))
            .into(),
        );
        settle().await;
        for (width, height) in [(1280, 900), (1024, 720), (700, 420), (360, 568), (390, 800)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle().await;
            scroll_to_end(&app).await;
            let action = ElementHandle::find_by_accessible_label(&app, "Start tracking")
                .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
                .expect("confirmation action");
            let p = action.absolute_position();
            let size = action.size();
            assert!(
                p.x >= 0.0 && p.x + size.width <= width as f32 + 1.0,
                "action width at {width}: {p:?} {size:?}"
            );
            assert!(
                p.y >= 0.0 && p.y + size.height <= height as f32 + 1.0,
                "action height at {width}x{height}: {p:?} {size:?}"
            );
            let scroll = ElementHandle::find_by_element_id(&app, "DetailPage::det_scroll")
                .next()
                .unwrap();
            let body = ElementHandle::find_by_element_id(&app, "TrackingPanel::tracking_body")
                .next()
                .unwrap();
            assert!(body.size().width <= 1040.0, "Tracking has a readable content width");
            assert!(
                scroll.size().height >= 80.0,
                "review viewport at {width}x{height}: {:?}",
                scroll.size()
            );
            assert!(
                p.y >= scroll.absolute_position().y
                    && p.y + size.height
                        <= scroll.absolute_position().y + scroll.size().height - 8.0,
                "confirmation scrolls fully into view with bottom clearance"
            );
            if let Some(dir) = &preview_dir {
                std::fs::create_dir_all(dir).unwrap();
                let snapshot = app.window().take_snapshot().unwrap();
                let bytes: Vec<u8> = snapshot
                    .as_slice()
                    .iter()
                    .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
                    .collect();
                image::save_buffer(
                    dir.join(format!("tracking-{width}x{height}.png")),
                    &bytes,
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        assert!(
            recorded_accepts.borrow().is_empty(),
            "opening and resizing cannot confirm links"
        );
        ElementHandle::find_by_accessible_label(&app, "Start tracking")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap()
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        assert_eq!(
            *recorded_accepts.borrow(),
            vec![("season-2".to_string(), false)]
        );
        let service = ElementHandle::find_by_accessible_label(&app, "AniList")
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        let scroll = ElementHandle::find_by_element_id(&app, "DetailPage::det_scroll")
            .next()
            .unwrap();
        let p = scroll.absolute_position();
        let size = scroll.size();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0),
                delta_x: 0.0,
                delta_y: p.y + size.height / 2.0 - service.absolute_position().y - service.size().height / 2.0,
            });
        settle().await;
        service.single_click(slint::platform::PointerEventButton::Left).await;
        settle().await;
        assert_eq!(*searches.borrow(), vec![(1, String::new())]);
        // A linked title can have several release cards. Its library actions
        // scroll with the entries and remain fully reachable at the end.
        app.set_tracking_setup_active(false);
        app.set_tracking_links(
            Rc::new(VecModel::from(
                (0..3)
                    .map(|index| nova::TrackingLinkRow {
                        id: format!("linked-{index}").into(),
                        title: format!(
                            "MyAnimeList · A Wild Last Boss Appeared! Season {}",
                            index + 1
                        )
                        .into(),
                        coverage: "12 confirmed source rows → episodes 1–12".into(),
                        state: "Up to date".into(),
                        progress: "12".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        for (width, height) in [(1280, 900), (360, 568), (390, 800)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle().await;
            let scroll = ElementHandle::find_by_element_id(&app, "DetailPage::det_scroll")
                .next()
                .unwrap();
            assert!(scroll.size().height >= 80.0);
            scroll_to_end(&app).await;
            for label in ["Choose another release", "Add missing releases"] {
                let action = ElementHandle::find_by_accessible_label(&app, label)
                    .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
                    .expect("linked-title action");
                let p = action.absolute_position();
                assert!(
                    p.y >= scroll.absolute_position().y,
                    "{label} stays inside the link viewport"
                );
                assert!(
                    p.y + action.size().height <= height as f32 - 8.0,
                    "{label} has bottom clearance at {width}x{height}: position={p:?}, size={:?}, actual-window={:?}, detail={:?}, viewport={:?}/{:?}", action.size(), app.window().size(), ElementHandle::find_by_element_type_name(&app, "DetailPage").map(|e| (e.absolute_position(), e.size())).collect::<Vec<_>>(), scroll.absolute_position(), scroll.size()
                );
                assert!(
                    p.y + action.size().height
                        <= scroll.absolute_position().y + scroll.size().height - 8.0,
                    "{label} scrolls inside the entries viewport: action={p:?}/{:?}, viewport={:?}/{:?}, body={:?}, end={:?}", action.size(), scroll.absolute_position(), scroll.size(), ElementHandle::find_by_element_id(&app, "TrackingPanel::tracking_body").map(|e| (e.absolute_position(),e.size())).collect::<Vec<_>>(), ElementHandle::find_by_element_id(&app, "TrackingPanel::content_end").map(|e| (e.absolute_position(),e.size())).collect::<Vec<_>>()
                );
            }
            if let Some(dir) = &preview_dir {
                let snapshot = app.window().take_snapshot().unwrap();
                let bytes: Vec<u8> = snapshot
                    .as_slice()
                    .iter()
                    .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
                    .collect();
                image::save_buffer(
                    dir.join(format!("linked-tracking-{width}x{height}.png")),
                    &bytes,
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        click_tab(&app, "Overview").await;
        assert_eq!(app.get_detail_tab(), 0);
        assert!(!app.get_tracking_open());
        app.invoke_tracking_show();
        settle().await;
        assert_eq!(app.get_tracking_return_tab(), 0);
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle().await;
        assert_eq!(app.get_detail_tab(), 0);
        assert!(!app.get_tracking_open());
        assert!(app.get_modal_visible());
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}

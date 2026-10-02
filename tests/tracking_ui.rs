//! Tracking uses the real Slint pages; verify narrow layouts, explicit history
//! confirmation, suggestion selection and sheet-first Back without network access.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
fn s(value: &str) -> SharedString {
    value.into()
}
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
async fn click(app: &nova::AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap_or_else(|| panic!("missing action {label}"))
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
}
fn check_width(app: &nova::AppWindow, failures: &RefCell<Vec<String>>) {
    let width = app.window().size().width as f32;
    for element in ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
    {
        let p = element.absolute_position();
        let size = element.size();
        if p.x >= 0.0 && p.x < width && size.width > 0.0 && p.x + size.width > width + 1.0 {
            failures.borrow_mut().push(format!(
                "element extends to {}, viewport {}",
                p.x + size.width,
                width
            ));
        }
    }
}
#[test]
fn tracking_settings_and_sheet_fit_phone_and_require_history_confirmation() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_tracking_accounts(
        Rc::new(VecModel::from(vec![
            nova::TrackingAccountRow {
                service: 0,
                name: s(&"long-account-name".repeat(8)),
                state: s("Reconnect to send retained updates"),
                client_id: s("public-client"),
                redirect_uri: s("http://127.0.0.1:53926/callback"),
                ..Default::default()
            },
            nova::TrackingAccountRow {
                service: 1,
                redirect_uri: s("https://anilist.co/api/v2/oauth/pin"),
                ..Default::default()
            },
        ]))
        .into(),
    );
    let failures = Rc::new(RefCell::new(Vec::new()));
    let actions = Rc::new(Cell::new(0));
    let actions2 = actions.clone();
    app.on_tracking_action(move |_, action| {
        if action == 2 {
            actions2.set(actions2.get() + 1);
        }
    });
    let weak = app.as_weak();
    app.on_tracking_show(move || {
        weak.upgrade().unwrap().set_tracking_open(true);
    });
    let weak = app.as_weak();
    app.on_tracking_close(move || {
        weak.upgrade().unwrap().set_tracking_open(false);
    });
    let searches = Rc::new(RefCell::new(Vec::new()));
    let recorded_searches = searches.clone();
    app.on_tracking_search(move |service, query| {
        recorded_searches
            .borrow_mut()
            .push((service, query.to_string()));
    });
    let picks = Rc::new(RefCell::new(Vec::new()));
    let recorded_picks = picks.clone();
    app.on_tracking_pick(move |index| recorded_picks.borrow_mut().push(index));
    let confirms = Rc::new(Cell::new(0));
    let recorded_confirms = confirms.clone();
    app.on_tracking_confirm(move || recorded_confirms.set(recorded_confirms.get() + 1));
    let weak = app.as_weak();
    let f = failures.clone();
    let a = actions.clone();
    slint::spawn_local(async move {
        settle().await;
        let app = weak.upgrade().unwrap();
        // The tracking link follows Downloads; reveal it before clicking.
        let from = slint::LogicalPosition::new(220.0, 680.0);
        let to = slint::LogicalPosition::new(220.0, 180.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: from,
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position: to });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: to,
                button: slint::platform::PointerEventButton::Left,
            });
        click(&app, "Tracking").await;
        check_width(&app, &f);
        assert_eq!(
            ElementHandle::find_by_element_type_name(&app, "AccountEditor").count(),
            2
        );
        app.set_show_settings(false);
        app.set_modal_visible(true);
        app.set_selected_title(s("An anime title"));
        app.set_tracking_links(
            Rc::new(VecModel::from(vec![nova::TrackingLinkRow {
                id: s("binding"),
                title: s(&"A long release title with words ".repeat(8)),
                state: s("Update queued"),
                progress: s("8"),
                coverage: s("12 confirmed source rows → episodes 1–12"),
                history_preview: s(
                    "Saved history will set progress to at least 10. Confirm to send this update.",
                ),
                ..Default::default()
            }]))
            .into(),
        );
        click(&app, "Tracking").await;
        assert!(app.get_tracking_open());
        check_width(&app, &f);
        click(&app, "Apply Nova history").await;
        assert_eq!(a.get(), 0, "preview must not send history");
        click(&app, "Confirm history update").await;
        assert_eq!(a.get(), 1);
        app.set_system_back_request(app.get_system_back_request() + 1);
        settle().await;
        assert!(!app.get_tracking_open());
        assert!(app.get_modal_visible(), "Back closes sheet before detail");
        app.set_tracking_links(Rc::new(VecModel::from(Vec::<nova::TrackingLinkRow>::new())).into());
        app.set_tracking_service(1);
        app.set_tracking_candidates(
            Rc::new(VecModel::from(vec![nova::TrackingCandidateRow {
                title: s("Suggested anime release"),
                reason: s("Title or alias matches; check year, format, and episode coverage."),
                detail: s("TV · 12 episodes · 2025"),
            }]))
            .into(),
        );
        app.set_tracking_can_confirm(false);
        click(&app, "Tracking").await;
        assert!(
            picks.borrow().is_empty(),
            "opening suggestions must not select a release"
        );
        assert_eq!(confirms.get(), 0, "suggestions must not create links");
        assert_eq!(app.get_tracking_service(), 1);
        click(&app, "Suggest releases").await;
        assert_eq!(
            *searches.borrow(),
            vec![(1, String::new())],
            "request suggestions for the displayed service"
        );
        assert!(
            ElementHandle::find_by_accessible_label(
                &app,
                "Title or alias matches; check year, format, and episode coverage."
            )
            .next()
            .is_some()
        );
        ElementHandle::find_by_accessible_label(&app, "Suggested anime release")
            .last()
            .expect("candidate selection button")
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        settle().await;
        assert_eq!(*picks.borrow(), vec![0]);
        assert_eq!(
            confirms.get(),
            0,
            "selecting a release must still require alignment confirmation"
        );
        check_width(&app, &f);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "{}",
        failures.borrow().join("\n")
    );
}

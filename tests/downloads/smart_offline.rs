//! Covered download controls stay inert while the offline management page owns input.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, Model, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

#[test]
fn smart_downloads_blocks_covered_actions_and_back_closes_only_one_layer() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "smart_downloads_blocks_covered_actions_and_back_closes_only_one_layer"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_downloads(true);
    app.set_downloads_page_counts(Rc::new(VecModel::from(vec![0, 0, 0, 0])).into());
    app.window().set_size(slint::LogicalSize::new(420.0, 840.0));
    let sorted = ElementHandle::find_by_accessible_label(&app, "Newest first")
        .next()
        .expect("underlying Downloads sort control");
    let position = sorted.absolute_position();
    let size = sorted.size();
    let sorts = Rc::new(Cell::new(0));
    let seen = sorts.clone();
    app.on_downloads_page_sort_changed(move |_| seen.set(seen.get() + 1));
    let offline = app.global::<nova::OfflineBridge>();
    offline.set_opened(true);
    settle(&app);
    let _ = ElementHandle::find_by_element_type_name(&app, "SmartDownloads")
        .next()
        .expect("offline page mounted")
        .size();
    let point = LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position: point,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
    settle(&app);
    assert_eq!(
        sorts.get(),
        0,
        "covered download sort must not receive taps"
    );
    let weak = app.as_weak();
    offline.on_closed(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<nova::OfflineBridge>().set_opened(false);
        }
    });
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!app.global::<nova::OfflineBridge>().get_opened());
    assert!(app.get_show_downloads());
    assert_eq!(
        app.global::<nova::OfflineBridge>().get_rows().row_count(),
        0
    );

    offline.set_rows(
        Rc::new(VecModel::from(vec![nova_ui_shared::SmartOfflineRow {
            id: "observatory".into(),
            title: "The Observatory — a longer series title".into(),
            status: "2 ready · 1 waiting for unmetered Wi-Fi".into(),
            enabled: true,
            source_ready: true,
            keep_count: 3,
            cleanup: true,
            ..Default::default()
        }]))
        .into(),
    );
    offline.set_opened(true);
    for (width, tv) in [(360., false), (1280., false), (1920., true)] {
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        app.window().set_size(slint::LogicalSize::new(width, 1080.));
        settle(&app);
        let card = ElementHandle::find_by_element_id(&app, "SmartDownloads::rule_card")
            .next()
            .expect("populated offline rule card");
        let origin = card.absolute_position();
        let card_size = card.size();
        let labels = card.query_descendants().match_type_name("Text").find_all();
        assert!(
            labels.len() >= 10,
            "measure all rule actions and supporting text"
        );
        for text in labels {
            let position = text.absolute_position();
            let size = text.size();
            assert!(
                position.x >= origin.x + 11.5
                    && position.x + size.width <= origin.x + card_size.width - 11.5,
                "offline rule text stays within horizontal card padding at {width}px"
            );
            assert!(
                position.y >= origin.y + 11.5
                    && position.y + size.height <= origin.y + card_size.height - 11.5,
                "offline rule action stays above card bottom padding at {width}px: {position:?} {size:?}, card {origin:?} {card_size:?}"
            );
        }
        let status = ElementHandle::find_by_element_id(&app, "SmartDownloads::rule_status")
            .next()
            .expect("offline rule status");
        assert!(
            status.size().height >= if tv { 20. } else { 14. },
            "supporting status uses readable phone/TV typography"
        );
    }
}

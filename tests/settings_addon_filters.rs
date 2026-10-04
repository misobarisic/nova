//! Installed-addon filters are wired through AppWindow and filtered actions
//! retain the selected row's URL on phone and desktop layouts.
#[path = "support/destinations.rs"]
mod destinations;
#[path = "support/settings.rs"]
mod settings;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let ready2 = ready.clone();
    let waker2 = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(80), move || {
        ready2.set(true);
        if let Some(waker) = waker2.borrow_mut().take() {
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
fn row(name: &str, url: &str) -> nova::AddonRow {
    nova::AddonRow {
        label: name.into(),
        url: url.into(),
        enabled: false,
        config_url: SharedString::default(),
        capabilities: SharedString::default(),
    }
}

#[test]
fn installed_addon_filters_and_actions_survive_layout_changes() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);

    let all = vec![
        row("Streams", "https://streams.example"),
        row("Metadata", "https://metadata.example"),
    ];
    app.set_addon_rows(Rc::new(VecModel::from(all.clone())).into());
    let weak = app.as_weak();
    let filter_calls = Rc::new(Cell::new(0));
    let calls = filter_calls.clone();
    app.on_addon_filter_changed(move || {
        calls.set(calls.get() + 1);
        let app = weak.upgrade().unwrap();
        let rows = all
            .iter()
            .filter(|r| {
                (app.get_addon_capability_filter() != 2 || r.label == "Metadata")
                    && r.label
                        .to_lowercase()
                        .contains(&app.get_addon_search_text().to_lowercase())
            })
            .cloned()
            .collect::<Vec<_>>();
        app.set_addon_rows(Rc::new(VecModel::from(rows)).into());
    });
    let removed = Rc::new(RefCell::new(Vec::new()));
    let removed2 = removed.clone();
    let weak = app.as_weak();
    app.on_addon_remove(move |i| {
        removed2.borrow_mut().push(
            weak.upgrade()
                .unwrap()
                .get_addon_rows()
                .row_data(i as usize)
                .unwrap()
                .url
                .to_string(),
        )
    });
    let weak = app.as_weak();
    let completed = Rc::new(Cell::new(false));
    let completed2 = completed.clone();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        settle().await;
        app.set_settings_search_query("Addons".into());
        settle().await;
        let addons = destinations::find(&app, "settings:addons")
            .next()
            .expect("Addons navigation");
        settings::click(&app, &addons).await;
        settle().await;
        let metadata = ElementHandle::find_by_accessible_label(&app, "Metadata")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .expect("metadata capability filter");
        settings::click(&app, &metadata).await;
        settle().await;
        assert_eq!(app.get_addon_capability_filter(), 2);
        assert_eq!(app.get_addon_rows().row_count(), 1);
        assert!(!app.get_addon_rows().row_data(0).unwrap().enabled);
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        settle().await;
        assert!(
            app.get_settings_detail_open(),
            "Addons detail must remain open"
        );
        let streams = ElementHandle::find_by_accessible_label(&app, "Streams")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .expect("phone streams capability filter");
        settings::click(&app, &streams).await;
        settle().await;
        assert_eq!(app.get_addon_capability_filter(), 3);
        let metadata = ElementHandle::find_by_accessible_label(&app, "Metadata")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .expect("phone metadata capability filter");
        settings::click(&app, &metadata).await;
        settle().await;
        assert_eq!(app.get_addon_capability_filter(), 2);
        app.set_settings_scroll_y(-500.0);
        settle().await;
        let remove = ElementHandle::find_by_accessible_label(&app, "Remove")
            .find(|e| e.size().width > 0.0)
            .expect("filtered remove action");
        settings::click(&app, &remove).await;
        settle().await;
        assert_eq!(&*removed.borrow(), &["https://metadata.example"]);
        app.set_addon_search_text("missing".into());
        app.invoke_addon_filter_changed();
        settle().await;
        assert_eq!(app.get_addon_rows().row_count(), 0);
        assert!(filter_calls.get() >= 2);
        completed2.set(true);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    app.run().unwrap();
    assert!(completed.get());
}

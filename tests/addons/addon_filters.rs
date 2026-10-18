//! Installed-addon filters are wired through AppWindow and filtered actions
//! retain the selected row's URL on phone and desktop layouts.
use crate::destinations;
use crate::settings;
use i_slint_backend_testing::{ElementHandle, ElementQuery};
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
        initial: name.chars().next().unwrap().to_string().into(),
        url: url.into(),
        enabled: false,
        config_url: SharedString::default(),
        capabilities: SharedString::default(),
        ..Default::default()
    }
}

#[test]
fn installed_addon_filters_and_actions_survive_layout_changes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "installed_addon_filters_and_actions_survive_layout_changes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    // Filter actions need settled geometry; page motion is covered separately.
    app.global::<nova::Anim>().set_enabled(false);
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
        // A renderer resolves the new layout after a resize. The headless
        // backend needs geometry reads before targeting the split filters.
        for element in ElementQuery::from_root(&app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        assert!(
            app.get_settings_detail_open(),
            "Addons detail must remain open"
        );
        // Natural-width capability pills stay in one horizontal rail. Scroll
        // that rail to reach Streams rather than changing the document offset.
        let rail = ElementHandle::find_by_element_id(&app, "SettingsAddonsModule::capability_rail")
            .next()
            .unwrap();
        let rail_position = rail.absolute_position();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(
                    rail_position.x + rail.size().width / 2.0,
                    rail_position.y + 20.0,
                ),
                delta_x: -300.0,
                delta_y: 0.0,
            });
        settle().await;
        let streams = ElementHandle::find_by_accessible_label(&app, "Streams")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .expect("phone streams capability filter");
        assert!(streams.absolute_position().x + streams.size().width <= 360.5);
        settings::click(&app, &streams).await;
        settle().await;
        assert_eq!(app.get_addon_capability_filter(), 3);
        let metadata = ElementHandle::find_by_accessible_label(&app, "Metadata")
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
            .expect("phone metadata capability filter");
        settings::click(&app, &metadata).await;
        settle().await;
        assert_eq!(app.get_addon_capability_filter(), 2);
        // Resolve the newly wrapped suggestion and row layout before changing
        // the viewport; the headless backend has no renderer to do this for us.
        for element in ElementQuery::from_root(&app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        app.set_settings_scroll_y(0.0);
        let viewport = ElementHandle::find_by_element_id(&app, "SettingsPage::set_scroll")
            .next()
            .expect("the Addons detail scroll viewport");
        let mut visible_remove = None;
        for _ in 0..20 {
            settle().await;
            let remove = ElementHandle::find_by_accessible_label(&app, "Remove").find(|e| {
                e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
            });
            if let Some(remove) = remove {
                let top = remove.absolute_position().y;
                let viewport_top = viewport.absolute_position().y;
                let viewport_bottom = viewport_top + viewport.size().height;
                if top >= viewport_top && top + remove.size().height <= viewport_bottom {
                    visible_remove = Some(remove);
                    break;
                }
                app.set_settings_scroll_y(
                    app.get_settings_scroll_y() + (viewport_top + viewport_bottom) / 2.0
                        - top
                        - remove.size().height / 2.0,
                );
            } else {
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(180.0, 400.0),
                        delta_x: 0.0,
                        delta_y: -80.0,
                    });
            }
        }
        let remove = visible_remove.expect("filtered remove action must be reachable by scrolling");
        let remove_position = remove.absolute_position();
        let viewport_position = viewport.absolute_position();
        assert!(remove_position.y >= viewport_position.y - 0.5);
        assert!(
            remove_position.y + remove.size().height
                <= viewport_position.y + viewport.size().height + 0.5,
            "the whole Remove target must be reachable after the Cinemeta description wraps"
        );
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

#[test]
fn cinemeta_install_button_stays_below_wrapped_description() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::cinemeta_install_button_stays_below_wrapped_description"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(0);
    app.set_settings_detail_open(true);
    app.set_cinemeta_installed(false);
    let installs = Rc::new(RefCell::new(Vec::new()));
    let recorded = installs.clone();
    app.on_addon_added(move |url| recorded.borrow_mut().push(url.to_string()));
    app.show().unwrap();
    for locale in ["en", "hr"] {
        slint::select_bundled_translation(locale).unwrap();
        for android in [false, true] {
            app.set_is_android(android);
            for width in [320, 360, 1280] {
                app.window().set_size(slint::PhysicalSize::new(width, 900));
                app.set_settings_scroll_y(0.0);
                for _ in 0..12 {
                    for element in ElementQuery::from_root(&app)
                        .match_predicate(|_| true)
                        .find_all()
                    {
                        let _ = (element.absolute_position(), element.size());
                    }
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
                }
                let description = ElementHandle::find_by_element_id(
                    &app,
                    "SettingsAddonsModule::cinemeta_description",
                )
                .next()
                .expect("Cinemeta description");
                let button =
                    ElementHandle::find_by_element_id(&app, "SettingsAddonsModule::cinemeta_add")
                        .next()
                        .expect("Add Cinemeta button");
                let text_bottom = description.absolute_position().y + description.size().height;
                assert!(
                    button.absolute_position().y >= text_bottom + 7.5,
                    "{locale}, Android={android}, width={width}: button overlaps description"
                );
                assert!(
                    (button.absolute_position().x - description.absolute_position().x).abs() < 0.5,
                    "button and description must share their left edge"
                );
                assert!(button.size().height >= 44.0);
                assert!(button.absolute_position().x + button.size().width <= width as f32);
                let before = installs.borrow().len();
                button.mock_single_click(slint::platform::PointerEventButton::Left);
                assert_eq!(installs.borrow().len(), before + 1);
                assert_eq!(
                    installs.borrow().last().unwrap(),
                    "https://v3-cinemeta.strem.io"
                );
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

//! Optional detail sections share the production page factory and fit phones.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn tap(app: &nova::AppWindow, label: &str) {
    let button = ElementHandle::find_by_accessible_label(app, label)
        .last()
        .unwrap();
    let p = button.absolute_position();
    let s = button.size();
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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

#[test]
fn rich_sections_fit_resize_dispatch_and_clear_with_sparse_titles() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::rich_sections_fit_resize_dispatch_and_clear_with_sparse_titles"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_detail_tab(0);
    app.set_selected_title("Example".into());
    app.global::<nova::Anim>().set_enabled(false);
    let action = |title: &str, subtitle: &str, target: &str| nova::DetailActionRow {
        title: title.into(),
        subtitle: subtitle.into(),
        target: target.into(),
    };
    app.set_selected_details(nova::DetailInfo {
        facts: Rc::new(VecModel::from(vec![
            action("Runtime", "120 min", ""),
            action("IMDb rating", "8.2", ""),
            action("Country", "A country with a long display name", ""),
        ]))
        .into(),
        cast: Rc::new(VecModel::from(vec![
            "An exceptionally long actor name".into(),
            "Second actor".into(),
        ]))
        .into(),
        directors: Rc::new(VecModel::from(vec!["Director name".into()])).into(),
        trailers: Rc::new(VecModel::from(vec![
            action(
                "Official trailer",
                "Opens in your browser",
                "https://example.org/trailer",
            ),
            action(
                "Teaser",
                "Opens in your browser",
                "https://example.org/teaser",
            ),
        ]))
        .into(),
        related: Rc::new(VecModel::from(vec![action(
            "Related title with a long descriptive name",
            "2024",
            "related",
        )]))
        .into(),
        ..Default::default()
    });
    let called = Rc::new(RefCell::new(None));
    let sink = called.clone();
    app.on_detail_extra_picked(move |kind, target| {
        *sink.borrow_mut() = Some((kind, target.to_string()))
    });
    app.show().unwrap();
    for (width, height) in [(360, 800), (760, 390), (1280, 900)] {
        app.window().set_size(slint::PhysicalSize::new(width, 5000));
        for _ in 0..10 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        }
        // Introspection excludes clipped elements. Measure the full overview
        // first, then verify the same sections are reachable by scrolling a
        // real phone/landscape viewport.
        let panel = ElementHandle::find_by_element_id(&app, "DetailPage::extras_content")
            .next()
            .expect("rich overview panel");
        assert!(panel.size().height > 100.0);
        for element in panel
            .query_descendants()
            .match_predicate(|_: &ElementHandle| true)
            .find_all()
        {
            let p = element.absolute_position();
            let s = element.size();
            assert!(
                p.x >= -1.0 && p.x + s.width <= width as f32 + 1.0,
                "{:?} overflows at {width}: {p:?} {s:?}",
                element.type_name()
            );
        }
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Official trailer")
                .next()
                .is_some()
        );
        if width == 360 {
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Teaser")
                    .next()
                    .is_none()
            );
            tap(&app, "More trailers");
            for _ in 0..10 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
            }
            assert!(
                ElementHandle::find_by_accessible_label(&app, "Teaser")
                    .next()
                    .is_some()
            );
        }
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        (*app)
            .global::<nova_ui_shared::DetailContent>()
            .invoke_scroll_list_to_top(100_000.0);
        for _ in 0..10 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        }
        assert!(
            ElementHandle::find_by_accessible_label(
                &app,
                "Related title with a long descriptive name"
            )
            .next()
            .is_some()
        );
    }
    app.window().set_size(slint::PhysicalSize::new(360, 5000));
    (*app)
        .global::<nova_ui_shared::DetailContent>()
        .invoke_scroll_list_to_top(-100_000.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let viewport = || {
        ElementHandle::find_by_element_id(&app, "DetailExtrasPanel::trailer_viewport")
            .next()
            .unwrap()
    };
    let expanded_height = viewport().size().height;
    // Establish the collapsed measurement without motion, then exercise both
    // directions with mock time rather than only checking the final rows.
    tap(&app, "Show less");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let collapsed_height = viewport().size().height;
    assert!(expanded_height > collapsed_height + 40.0);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    tap(&app, "More trailers");
    let _ = viewport().size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let midway = viewport().size().height;
    assert!(midway > collapsed_height + 1.0 && midway < expanded_height - 1.0);
    let teaser = ElementHandle::find_by_accessible_label(&app, "Teaser")
        .next()
        .unwrap();
    assert!(teaser.computed_opacity() > 0.0 && teaser.computed_opacity() < 1.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!((viewport().size().height - expanded_height).abs() < 1.0);
    tap(&app, "Show less");
    let _ = viewport().size();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    let midway = viewport().size().height;
    assert!(midway > collapsed_height + 1.0 && midway < expanded_height - 1.0);
    app.global::<nova::Anim>().set_transitions(false);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    assert!((viewport().size().height - collapsed_height).abs() < 1.0);
    tap(&app, "More trailers");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
    assert!((viewport().size().height - expanded_height).abs() < 1.0);
    app.global::<nova::DetailBridge>()
        .invoke_detail_extra_picked(1, "https://example.org/trailer".into());
    assert_eq!(
        *called.borrow(),
        Some((1, "https://example.org/trailer".into()))
    );
    app.set_selected_details(Default::default());
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
    }
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Official trailer")
            .next()
            .is_none()
    );
}

//! Reordered built-in and addon rails must reserve their full card height.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{rc::Rc, time::Duration};

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing visible element: {id}"))
}

fn assert_separated(app: &nova::AppWindow, ids: &[&str]) {
    let sections: Vec<_> = ids.iter().map(|id| element(app, id)).collect();
    for section in &sections {
        assert!(
            section.size().height > 200.0,
            "rail collapsed: {:?}",
            section.size()
        );
    }
    for pair in sections.windows(2) {
        let bottom = pair[0].absolute_position().y + pair[0].size().height;
        assert!(
            pair[1].absolute_position().y >= bottom - 1.0,
            "catalog sections overlap"
        );
    }
}

#[test]
fn home_catalogs_keep_full_height_when_reordered_and_hidden() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(true);
    app.set_home_continue(Rc::new(VecModel::from(vec![nova::ContinueRow::default()])).into());
    app.set_home_upcoming(Rc::new(VecModel::from(vec![nova::UpcomingRow::default()])).into());
    app.set_home_catalog_sections(
        Rc::new(VecModel::from(vec![nova::HomeCatalogSection {
            title: "Addon catalog".into(),
            first_card: 0,
            card_count: 1,
        }]))
        .into(),
    );
    app.set_home_catalog_cards(
        Rc::new(VecModel::from(vec![nova::HomeCatalogCard::default()])).into(),
    );
    app.window().show().unwrap();
    for width in [1400, 390] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![-1, -2, 0])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![0, 1, 2])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &[
                "HomePage::continue_section",
                "HomePage::upcoming_section",
                "HomePage::catalog_section",
            ],
        );
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![0, -1, -2])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![1, 2, 0])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &[
                "HomePage::catalog_section",
                "HomePage::continue_section",
                "HomePage::upcoming_section",
            ],
        );
        app.set_home_continue(Rc::new(VecModel::from(Vec::<nova::ContinueRow>::new())).into());
        app.set_home_catalog_order(Rc::new(VecModel::from(vec![0, -2])).into());
        app.set_home_catalog_positions(Rc::new(VecModel::from(vec![-1, 1, 0])).into());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert_separated(
            &app,
            &["HomePage::catalog_section", "HomePage::upcoming_section"],
        );
        assert!(
            ElementHandle::find_by_element_id(&app, "HomePage::continue_section")
                .next()
                .is_none()
        );
        app.set_home_continue(Rc::new(VecModel::from(vec![nova::ContinueRow::default()])).into());
    }
}

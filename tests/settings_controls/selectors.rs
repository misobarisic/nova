//! Actual Theme selectors retain one selection across split rows and resizes.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::time::Duration;

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn options(app: &nova::AppWindow, title: &str) -> Vec<ElementHandle> {
    let row = ElementHandle::find_by_element_type_name(app, "SettingsRow")
        .find(|row| {
            let title = title.to_owned();
            row.query_descendants()
                .match_predicate(move |e| e.accessible_label().as_deref() == Some(title.as_str()))
                .find_first()
                .is_some()
        })
        .expect("theme setting row");
    row.query_descendants()
        .match_accessible_role(AccessibleRole::Button)
        .find_all()
}

fn selection(app: &nova::AppWindow, title: &str, index: usize) {
    let choices = options(app, title);
    assert_eq!(choices.len(), 5);
    for (i, option) in choices.iter().enumerate() {
        assert_eq!(option.accessible_checked(), Some(i == index));
    }
}

#[test]
fn split_selection_follows_clicks_and_external_updates_with_spaced_targets() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "split_selection_follows_clicks_and_external_updates_with_spaced_targets"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(14);
    app.set_settings_detail_open(true);
    app.global::<nova::Anim>().set_enabled(false);
    app.show().unwrap();
    for (language, title) in [
        ("en", "Card corner radius"),
        ("hr", "Polumjer kutova kartica"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [320, 360, 390] {
            app.window().set_size(slint::PhysicalSize::new(width, 1900));
            app.set_settings_scroll_y(0.0);
            settle(&app);
            let choices = options(&app, title);
            assert_eq!(choices.len(), 5);
            for option in &choices {
                let p = option.absolute_position();
                assert!(p.x >= 0.0 && p.x + option.size().width <= width as f32 + 0.5);
                assert!(option.size().height >= 44.0);
            }
            for (left, right) in [(0, 1), (1, 2), (3, 4)] {
                let a = choices[left].absolute_position();
                let b = choices[right].absolute_position();
                assert!(b.x - a.x - choices[left].size().width >= 7.5);
            }
            let top = choices[0].absolute_position();
            let bottom = choices[3].absolute_position();
            assert!(bottom.y - top.y - choices[0].size().height >= 7.5);
            // Clicking both halves used to leave two independently selected options.
            for index in [4, 2, 3, 0, 4, 1] {
                choices[index].mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(
                    app.get_card_corner_radius(),
                    [0.0, 4.0, 10.0, 16.0, 24.0][index]
                );
                selection(&app, title, index);
            }
            // External owner updates must still reach both derived rows.
            for (index, value) in [0.0, 4.0, 10.0, 16.0, 24.0].into_iter().enumerate() {
                app.set_card_corner_radius(value);
                settle(&app);
                selection(&app, title, index);
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

//! Detail top bar regression (headless): it must be icon-only — no
//! "Add to library"/"Remove from library"/"Categories" text buttons — and
//! stay within the viewport horizontally at phone width.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn max_right_edge(app: &nova::AppWindow) -> f32 {
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .map(|e| e.absolute_position().x + e.size().width)
        .fold(0.0, f32::max)
}

/// Text `Text` elements currently laid out (non-zero size).
fn visible_texts(app: &nova::AppWindow) -> Vec<String> {
    ElementQuery::from_root(app)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .filter(|e| e.size().width > 0.0 && e.size().height > 0.0)
        .filter_map(|e| e.accessible_label().map(|l| l.to_string()))
        .collect()
}

fn show_series(app: &nova::AppWindow) {
    app.set_show_settings(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_detail_tab(0);
    app.set_selected_title(s("A Series Title"));
    app.set_selected_description(s(""));
    app.set_streams(Rc::new(VecModel::from(Vec::new())).into());
    app.set_in_library(true);
    app.set_category_rows(
        Rc::new(VecModel::from(vec![nova::CategoryRow { name: s("Anime") }])).into(),
    );
    app.set_selected_category_count(1);
}

#[test]
fn detail_top_bar_is_icon_only_and_fits() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    show_series(&app);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();

        let labels = visible_texts(&app).join(" | ");
        for banned in [
            "Add to library",
            "Remove from library",
            "Categories",
            "Back",
        ] {
            if labels.contains(banned) {
                failures1
                    .borrow_mut()
                    .push(format!("top bar still shows text button {banned:?}"));
            }
        }

        // In library + categories present: back, library, categories.
        let buttons = ElementHandle::find_by_element_type_name(&app, "TopIconButton").count();
        if buttons < 3 {
            failures1
                .borrow_mut()
                .push(format!("expected 3 icon buttons, found {buttons}"));
        }

        if max_right_edge(&app) > 361.0 {
            failures1.borrow_mut().push(format!(
                "top bar overflows 360px (edge {})",
                max_right_edge(&app)
            ));
        }

        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "detail top bar failures:\n  {}",
        failures.join("\n  ")
    );
}

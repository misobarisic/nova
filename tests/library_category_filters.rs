//! My Library category filter bar (headless).
//!
//! The filter bar holds the "All" chip plus a dropdown listing the automatic
//! buckets (Plan to Watch / Watching / Completed / On Hold / Dropped) followed
//! by the user's own categories. The backend always sends those built-in names
//! (`Bridge::apply_category_rows`), so the bar must render on a fresh install
//! too — it used to be gated on the user-category model, which is empty until a
//! category is created, leaving the automatic buckets unreachable.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn filter_bar_lists_auto_buckets_without_user_categories() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_show_settings(false);

    // Exactly what `apply_category_rows` pushes: the built-in filters first,
    // then user categories (none here).
    let names: Vec<SharedString> = [
        "Plan to Watch",
        "Watching",
        "Completed",
        "On Hold",
        "Dropped",
    ]
    .iter()
    .map(|n| SharedString::from(*n))
    .collect();
    app.set_library_category_names(Rc::new(VecModel::from(names)).into());
    // No user categories: the model the filter bar used to be gated on.
    let no_cats: Vec<nova::CategoryRow> = Vec::new();
    app.set_category_rows(Rc::new(VecModel::from(no_cats)).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let failures1 = failures.clone();
    let app1 = app.as_weak();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        if ElementHandle::find_by_element_type_name(&app, "Dropdown")
            .next()
            .is_none()
        {
            failures1
                .borrow_mut()
                .push("the Library filter dropdown must show the automatic buckets with no user categories".to_string());
        }
        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "library category filter failures:\n  {}",
        failures.join("\n  ")
    );
}

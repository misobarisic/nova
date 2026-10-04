//! Automatic category pills must work with no user-created categories.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

#[test]
fn filter_bar_lists_auto_buckets_without_user_categories() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.set_show_home(false);
    app.set_show_library(true);
    let names: Vec<SharedString> = [
        "Watching",
        "Completed",
        "On Hold",
        "Dropped",
        "Plan to Watch",
    ]
    .iter()
    .map(|n| SharedString::from(*n))
    .collect();
    app.set_library_category_names(Rc::new(VecModel::from(names)).into());
    app.set_category_rows(Rc::new(VecModel::from(Vec::<nova::CategoryRow>::new())).into());
    let picked = Rc::new(RefCell::new(Vec::new()));
    let recorded = picked.clone();
    let weak = app.as_weak();
    app.on_library_filter_picked(move |name| {
        recorded.borrow_mut().push(name.to_string());
        weak.upgrade().unwrap().set_library_filter_category(name);
    });
    app.window().show().unwrap();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let rail = ElementHandle::find_by_element_id(&app, "LibraryPage::filter_rail")
        .next()
        .unwrap();
    assert!(rail.size().height <= 44.0);
    for name in ["Watching", "All"] {
        let pill = ElementHandle::find_by_accessible_label(&app, name)
            .find(|e| e.type_name().as_deref() == Some("LibraryFilterChip"))
            .expect(name);
        let p = pill.absolute_position();
        let s = pill.size();
        let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
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
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    }
    assert_eq!(*picked.borrow(), ["Watching", ""]);

    // Select a pill at the end of the rail and rotate in both directions.
    // Resizing must clamp the old offset and keep the selection visible.
    app.set_library_filter_category("Plan to Watch".into());
    for width in [360, 1000, 320, 620] {
        app.window().set_size(slint::PhysicalSize::new(width, 800));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
        let rail = ElementHandle::find_by_element_id(&app, "LibraryPage::filter_rail")
            .next()
            .unwrap();
        let pill = ElementHandle::find_by_accessible_label(&app, "Plan to Watch")
            .find(|e| e.type_name().as_deref() == Some("LibraryFilterChip"))
            .unwrap();
        assert!(pill.absolute_position().x >= rail.absolute_position().x - 0.5);
        assert!(
            pill.absolute_position().x + pill.size().width
                <= rail.absolute_position().x + rail.size().width + 0.5,
            "selected pill must fit at {width}px"
        );
    }
}

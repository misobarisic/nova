//! Duplicate suggestions and the move warning fit phone/desktop viewports.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{rc::Rc, time::Duration};

#[test]
fn duplicate_sheet_and_move_review_fit_and_back_preserves_detail() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_modal_visible(true);
    app.set_library_duplicates(
        Rc::new(VecModel::from(vec![nova::MediaCard {
            id: "saved".into(),
            title: "A saved title with a very long subtitle".into(),
            year: "2026".into(),
            media_type: "TV".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_library_duplicates_open(true);
    app.window().show().unwrap();
    for width in [320, 390, 700, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 720));
        for selection in [-1, 0] {
            app.set_library_duplicate_selection(selection);
            app.set_library_duplicate_unmatched(12);
            app.set_library_duplicate_matched(3);
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
            let panel = ElementHandle::find_by_element_id(&app, "LibraryDuplicates::panel")
                .next()
                .unwrap();
            let position = panel.absolute_position();
            let size = panel.size();
            assert!(position.x >= -0.5 && position.y >= -0.5);
            assert!(position.x + size.width <= width as f32 + 0.5);
            assert!(position.y + size.height <= 720.5);
        }
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
    assert_eq!(app.get_library_duplicate_selection(), -1);
    assert!(app.get_library_duplicates_open());
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
    assert!(!app.get_library_duplicates_open());
    assert!(app.get_modal_visible());
}

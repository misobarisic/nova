//! Library search uses the same expandable/fading transition as episode search.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::time::Duration;

fn tick(app: &nova::AppWindow, ms: u64) {
    for _ in 0..ms.div_ceil(10) {
        for e in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (e.size(), e.absolute_position(), e.computed_opacity());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}
fn element(app: &nova::AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, &format!("LibraryPage::{name}"))
        .next()
        .expect(name)
}
fn toggle(app: &nova::AppWindow) {
    element(app, "library_search_button")
        .mock_single_click(slint::platform::PointerEventButton::Left);
}
#[test]
fn search_expands_and_fades_then_closes_with_motion_settings_respected() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_library(true);
    app.set_animations(true);
    app.set_anim_transitions(true);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    tick(&app, 400);
    toggle(&app);
    let mut grew = false;
    let mut faded = false;
    for _ in 0..35 {
        tick(&app, 10);
        if let Some(panel) =
            ElementHandle::find_by_element_id(&app, "LibraryPage::library_search_reveal").next()
        {
            grew |= panel.size().height > 0.5 && panel.size().height < 57.5;
            faded |= panel.computed_opacity() > 0.01 && panel.computed_opacity() < 0.99;
        }
    }
    assert!(
        grew,
        "opening must pass through intermediate search heights"
    );
    assert!(faded, "opening must fade through intermediate opacity");
    let panel = element(&app, "library_search_reveal");
    assert!((panel.size().height - 58.0).abs() < 0.5);
    let button = element(&app, "library_search_button");
    assert!(panel.absolute_position().y >= button.absolute_position().y + button.size().height);
    // Resizing preserves the mounted field and the desktop ordering.
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    tick(&app, 100);
    assert!(panel.absolute_position().y + panel.size().height <= button.absolute_position().y);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    tick(&app, 100);
    // Delayed focus still allows typing as soon as the search unfolds.
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "x".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
    tick(&app, 20);
    assert_eq!(app.get_library_query(), "x");
    toggle(&app);
    let mut shrank = false;
    for _ in 0..35 {
        tick(&app, 10);
        shrank |= panel.size().height > 0.5 && panel.size().height < 57.5;
    }
    assert!(
        shrank,
        "closing must pass through intermediate search heights"
    );
    assert!(panel.size().height < 0.5);
    assert!(app.get_library_query().is_empty());
    assert!(!app.get_library_search_open());
    app.set_animations(false);
    // Settings applies the switch to the shared animation global in Rust.
    app.global::<nova::Anim>().set_enabled(false);
    tick(&app, 20);
    toggle(&app);
    tick(&app, 20);
    assert!(
        (panel.size().height - 58.0).abs() < 0.5,
        "motion disabled must open immediately"
    );
    toggle(&app);
    tick(&app, 20);
    assert!(
        panel.size().height < 0.5,
        "motion disabled must close immediately"
    );
}

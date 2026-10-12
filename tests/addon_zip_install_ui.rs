//! ZIP selection uses the installed Settings submenu and prevents duplicate
//! requests while an import is active, including at phone widths.
#[path = "support/destinations.rs"]
mod destinations;
#[path = "support/settings.rs"]
mod settings;

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};

#[test]
fn addon_zip_request_is_bridged_and_disabled_while_busy() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1100, 900));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_settings(true);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let weak = app.as_weak();
    app.on_addon_zip_import_requested(move || {
        observed.set(observed.get() + 1);
        weak.upgrade().unwrap().set_addon_zip_import_busy(true);
    });
    let weak = app.as_weak();
    let completed = Rc::new(Cell::new(false));
    let completed2 = completed.clone();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        app.set_settings_search_query("Addons".into());
        settings::settle().await;
        let addons = destinations::find(&app, "settings:addons").next().unwrap();
        settings::click(&app, &addons).await;
        settings::settle().await;
        for width in [1100, 360] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_addon_zip_import_busy(false);
            app.set_addon_zip_import_status("The addon ZIP exceeds the 8 MiB size limit".into());
            settings::settle().await;
            for element in ElementQuery::from_root(&app)
                .match_predicate(|_| true)
                .find_all()
            {
                let _ = (element.size(), element.absolute_position());
            }
            let button = ElementHandle::find_by_accessible_label(&app, "Install addon ZIP…")
                .find(|e| {
                    e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
                })
                .unwrap();
            assert!(button.absolute_position().x + button.size().width <= width as f32 + 0.5);
            let before = calls.get();
            settings::click(&app, &button).await;
            settings::settle().await;
            assert_eq!(calls.get(), before + 1);
            settings::click(&app, &button).await;
            assert_eq!(
                calls.get(),
                before + 1,
                "busy import must reject a second click"
            );
        }
        completed2.set(true);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
    assert!(completed.get());
}

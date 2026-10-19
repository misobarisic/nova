//! Public directory actions retain their URL and the manual install controls
//! remain available after returning to Installed on phone and desktop.
use crate::settings;
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing button: {label}"))
}

#[test]
fn directory_cards_and_tabs_keep_actions_and_manual_installation() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::directory_cards_and_tabs_keep_actions_and_manual_installation"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(0);
    app.set_settings_detail_open(true);
    app.set_cinemeta_installed(true);
    app.set_addon_browser_rows(
        Rc::new(VecModel::from(vec![nova::AddonBrowserRow {
            label: "Fixture addon".into(),
            description: "A directory description that wraps safely on a narrow screen.".into(),
            capabilities: "Streams · Subtitles".into(),
            initial: "F".into(),
            url: "https://fixture.example/addon".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_addon_browser_source_name("Fixture directory".into());
    app.set_addon_browser_source_url("https://directory.example".into());
    let actions = Rc::new(RefCell::new(Vec::new()));
    let actions2 = actions.clone();
    app.on_addon_browser_action(move |url| actions2.borrow_mut().push(url.to_string()));
    let loads = Rc::new(Cell::new(0));
    let loads2 = loads.clone();
    app.on_addon_browser_load(move |_| loads2.set(loads2.get() + 1));
    let done = Rc::new(Cell::new(false));
    let completed = done.clone();
    let weak = app.as_weak();
    app.show().unwrap();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        for width in [1100, 360] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.set_settings_scroll_y(0.0);
            settings::settle().await;
            settings::click(&app, &button(&app, "Browse")).await;
            settings::settle().await;
            assert_eq!(app.get_addon_browser_tab(), 1);
            // Geometry reads settle the renderer-free backend after resize.
            for element in ElementQuery::from_root(&app)
                .match_predicate(|_| true)
                .find_all()
            {
                let _ = (element.size(), element.absolute_position());
            }
            let install = button(&app, "Install");
            settings::click(&app, &install).await;
            settings::settle().await;
            let position = install.absolute_position();
            assert!(position.x >= -0.5);
            assert!(position.x + install.size().width <= width as f32 + 0.5);
            app.set_settings_scroll_y(0.0);
            settings::settle().await;
            settings::click(&app, &button(&app, "Installed")).await;
            settings::settle().await;
            assert_eq!(app.get_addon_browser_tab(), 0);
            assert!(button(&app, "Install addon ZIP…").size().height >= 44.0);
        }
        assert_eq!(
            &*actions.borrow(),
            &[
                "https://fixture.example/addon",
                "https://fixture.example/addon"
            ]
        );
        assert_eq!(loads.get(), 2);
        completed.set(true);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    app.run().unwrap();
    assert!(done.get());
}

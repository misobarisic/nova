//! Settings subpage horizontal-overflow regression test (headless).
//!
//! The Addons subpage lists each addon with its install URL as the row
//! description. A long, unbreakable URL used to give the row a minimum width
//! larger than a phone viewport, which widened the whole settings content
//! layer and let the page pan sideways. Asserts nothing extends past the
//! window edge on the Addons subpage at phone and desktop widths.

use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Rightmost edge over elements whose left edge is on screen. The inactive
/// landing layer (slid left) and the parked subpage layer are ignored by the
/// `x` bound; a real overflow keeps its left edge visible while its right
/// edge runs past the window.
fn max_right_edge(app: &nova::AppWindow) -> f32 {
    use i_slint_backend_testing::{ElementHandle, ElementQuery};
    let width = app.window().size().width as f32;
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let sz = e.size();
            let p = e.absolute_position();
            (p.x >= 0.0 && p.x < width && sz.width > 0.0).then_some(p.x + sz.width)
        })
        .fold(0.0f32, f32::max)
}

#[test]
fn settings_addons_subpage_has_no_horizontal_overflow() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    // A realistic addon URL: long and unbreakable.
    app.set_addon_rows(
        Rc::new(VecModel::from(vec![nova::AddonRow {
            label: s("Cinemeta"),
            url: s("https://v3-cinemeta.strem.io/manifest.json"),
            enabled: true,
            config_url: s("https://v3-cinemeta.strem.io/configure"),
        }]))
        .into(),
    );

    let failures: Rc<std::cell::RefCell<Vec<String>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Open the first landing entry (Addons).
        use i_slint_backend_testing::ElementHandle;
        let first = ElementHandle::find_by_element_type_name(&app, "SettingsLink")
            .next()
            .expect("landing has SettingsLinks");
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            first
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            // Let the slide-in transition settle before measuring.
            after(400, move || {
                let app = app2.upgrade().unwrap();
                let edge = max_right_edge(&app);
                if edge > 361.0 {
                    failures2.borrow_mut().push(format!(
                        "360px addons: content reaches {edge:.1}, want <= 360"
                    ));
                }
                app.window().set_size(slint::PhysicalSize::new(500, 800));
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    let edge = max_right_edge(&app);
                    if edge > 501.0 {
                        failures3.borrow_mut().push(format!(
                            "500px addons: content reaches {edge:.1}, want <= 500"
                        ));
                    }
                    app.window().set_size(slint::PhysicalSize::new(1100, 800));
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        let edge = max_right_edge(&app);
                        if edge > 1101.0 {
                            failures4.borrow_mut().push(format!(
                                "1100px addons: content reaches {edge:.1}, want <= 1100"
                            ));
                        }
                        slint::quit_event_loop().unwrap();
                    });
                });
            });
        })
        .unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "overflow failures:\n  {}",
        failures.join("\n  ")
    );
}

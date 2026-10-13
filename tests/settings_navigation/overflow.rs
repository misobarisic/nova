//! Settings subpage horizontal-overflow regression test (headless).
//!
//! The Addons subpage lists each addon with its install URL as the row
//! description. A long, unbreakable URL used to give the row a minimum width
//! larger than a phone viewport, which widened the whole settings content
//! layer and let the page pan sideways. The capability pills deliberately pan
//! inside their bounded horizontal rail; all other descendants and the page's
//! own scroll content must stay within the viewport.

use crate::destinations;
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
    use std::collections::HashSet;
    let width = app.window().size().width as f32;
    let rail = ElementHandle::find_by_element_id(app, "SettingsAddonsModule::capability_rail")
        .next()
        .expect("bounded capability rail");
    let rail_position = rail.absolute_position();
    assert!(
        rail_position.x >= 0.0 && rail_position.x + rail.size().width <= width + 1.0,
        "the horizontal rail's viewport must fit the window"
    );
    let viewport = ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll")
        .next()
        .expect("Settings document viewport");
    let content = ElementHandle::find_by_element_id(app, "SettingsPage::sub_content")
        .next()
        .expect("Settings document content");
    assert!(
        content.size().width <= viewport.size().width + 1.0,
        "the Settings document must not widen to fit horizontal pill content"
    );

    // The testing API has no public handle identity/equality operation. Match
    // exact debug names, types and geometry collected only from this rail's
    // descendants; other chip instances or page controls remain checked.
    let key = |element: &ElementHandle| {
        let p = element.absolute_position();
        let s = element.size();
        (
            element.id().map(|s| s.to_string()),
            element.type_name().map(|s| s.to_string()),
            [
                p.x.to_bits(),
                p.y.to_bits(),
                s.width.to_bits(),
                s.height.to_bits(),
            ],
        )
    };
    let clipped_descendants = rail
        .query_descendants()
        .match_predicate(|_| true)
        .find_all()
        .iter()
        .map(&key)
        .collect::<HashSet<_>>();
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .filter_map(|e| {
            let sz = e.size();
            let p = e.absolute_position();
            if clipped_descendants.contains(&key(&e)) {
                return None;
            }
            (p.x >= 0.0 && p.x < width && sz.width > 0.0).then_some(p.x + sz.width)
        })
        .fold(0.0f32, f32::max)
}

#[test]
fn settings_addons_subpage_has_no_horizontal_overflow() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "settings_addons_subpage_has_no_horizontal_overflow"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    // A realistic addon URL: long and unbreakable.
    app.set_addon_rows(
        Rc::new(VecModel::from(vec![nova::AddonRow {
            label: s("Cinemeta"),
            initial: s("C"),
            capabilities: s(""),
            url: s("https://v3-cinemeta.strem.io/manifest.json"),
            enabled: true,
            config_url: s("https://v3-cinemeta.strem.io/configure"),
            ..Default::default()
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
        let first = ({
            app.set_settings_search_query("Addons".into());
            destinations::find(&app, "settings:addons").next()
        })
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

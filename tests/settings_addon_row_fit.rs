//! Vertical-fit regression for the Settings → Addons rows (headless): on
//! narrow screens each row is an `AddonRowCard` (name + move arrows on the
//! first line, icon actions + Copy link underneath), on wide screens a
//! `SettingsRow` with one pill line. Every control must draw inside its row
//! at phone and desktop widths, and the name must sit on the arrows' line
//! without running under them or into the action line. One test function:
//! the testing backend initializes once per process.

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

type Rect = (f32, f32, f32, f32);

fn rect(e: &ElementHandle) -> Rect {
    let p = e.absolute_position();
    let sz = e.size();
    (p.x, p.y, sz.width, sz.height)
}

/// Addon-row controls: the toggle, the Copy link pill and the icon buttons
/// (configure cog, refresh, remove, move arrows). The install box's "Add"
/// pill is not in a row and is skipped by the caller.
fn row_controls(app: &nova::AppWindow) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    for kind in ["ToggleSwitch", "PillButton", "AddonIconButton"] {
        for e in ElementHandle::find_by_element_type_name(app, kind) {
            let r = rect(&e);
            if r.2 <= 0.0 || r.3 <= 0.0 {
                continue; // parked layer / not laid out
            }
            let name = e
                .accessible_label()
                .map(|l| l.to_string())
                .unwrap_or_default();
            if name == "Add" {
                continue;
            }
            out.push((format!("{kind} {name:?}"), r));
        }
    }
    if let Some(copy) = ElementHandle::find_by_element_id(app, "AddonRowCard::addon_copy").next() {
        out.push(("Copy link".into(), rect(&copy)));
    }
    out
}

/// The smallest addon row (`SettingsRow` on wide, `AddonRowCard` on narrow)
/// containing the element's centre, if any.
fn row_of(app: &nova::AppWindow, target: Rect) -> Option<Rect> {
    let (tx, ty, tw, th) = target;
    let centre = (tx + tw / 2.0, ty + th / 2.0);
    let mut rows = Vec::new();
    for kind in ["SettingsRow", "AddonRowCard"] {
        rows.extend(ElementHandle::find_by_element_type_name(app, kind).map(|e| rect(&e)));
    }
    rows.into_iter()
        .filter(|(x, y, w, h)| {
            centre.0 >= *x && centre.0 <= x + w && centre.1 >= *y && centre.1 <= y + h
        })
        .min_by(|a, b| (a.2 * a.3).partial_cmp(&(b.2 * b.3)).unwrap())
}

/// Controls of a row draw inside it: `(x, y, w, h)` within the row bounds,
/// allowing half a pixel of rounding.
fn inside(control: Rect, row: Rect) -> bool {
    let (cx, cy, cw, ch) = control;
    let (rx, ry, rw, rh) = row;
    cx >= rx - 0.5 && cy >= ry - 0.5 && cx + cw <= rx + rw + 0.5 && cy + ch <= ry + rh + 0.5
}

fn check(app: &nova::AppWindow, label: &str, failures: &Rc<RefCell<Vec<String>>>) {
    let controls = row_controls(app);
    if controls.len() < 7 {
        // A row with a configure page: toggle, Configure, Copy link,
        // Refresh, Remove, Move up, Move down.
        failures.borrow_mut().push(format!(
            "{label}: measured only {} row controls — the addon row was not rendered as expected",
            controls.len()
        ));
        return;
    }
    for (name, r) in &controls {
        let Some(row) = row_of(app, *r) else { continue };
        if !inside(*r, row) {
            failures.borrow_mut().push(format!(
                "{label}: {name} at {r:?} does not fit its row {row:?}"
            ));
        }
    }

    // Identity text stays above the action bands and to the left of its toggle.
    let title = ElementHandle::find_by_element_type_name(app, "Text")
        .find(|element| element.accessible_label().as_deref() == Some("Cinemeta"));
    let toggle = controls
        .iter()
        .find(|(name, _)| name.starts_with("ToggleSwitch"));
    if let (Some(title), Some((_, toggle))) = (title, toggle) {
        let (x, y, width, height) = rect(&title);
        assert!(x + width <= toggle.0 + 0.5, "title must clear the toggle");
        let title_row = row_of(app, rect(&title)).expect("title belongs to an addon card");
        let actions_top = controls
            .iter()
            .filter(|(name, rect)| !name.starts_with("ToggleSwitch") && inside(*rect, title_row))
            .map(|(_, rect)| rect.1)
            .fold(f32::MAX, f32::min);
        assert!(
            y + height <= actions_top + 0.5,
            "{label}: title bottom {} must clear actions at {actions_top}; controls {controls:?}",
            y + height
        );
    }
}

#[test]
fn addon_row_controls_fit_their_row() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    // A row with a configure page renders every control (Configure included).
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

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Open the first landing entry (Addons).
        let addons = ({
            app.set_settings_search_query("Addons".into());
            destinations::find(&app, "settings:addons").next()
        })
        .expect("landing has SettingsLinks");
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        slint::spawn_local(async move {
            addons
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                check(&app, "360px", &failures2);
                // Deskop width: the controls sit in a single row instead.
                app.window().set_size(slint::PhysicalSize::new(1100, 800));
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    check(&app, "1100px", &failures3);
                    // Translated labels are longer than their English sources
                    // ("Konfiguriraj" → "Konfigurirajte"), and this row is the
                    // tightest in the app: re-check it in Croatian. Skipped in
                    // the interpreter build, which has no catalogs.
                    if slint::select_bundled_translation("hr").is_err() {
                        slint::quit_event_loop().unwrap();
                        return;
                    }
                    app.window().set_size(slint::PhysicalSize::new(360, 800));
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        check(&app, "hr 360px", &failures4);
                        app.window().set_size(slint::PhysicalSize::new(1100, 800));
                        let app5 = app.as_weak();
                        let failures5 = failures4.clone();
                        after(400, move || {
                            let app = app5.upgrade().unwrap();
                            check(&app, "hr 1100px", &failures5);
                            slint::quit_event_loop().unwrap();
                        });
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
        "addon row fit failures:\n  {}",
        failures.join("\n  ")
    );
}

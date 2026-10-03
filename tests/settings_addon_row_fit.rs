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

fn by_label(app: &nova::AppWindow, kind: &str, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, kind)
        .into_iter()
        .find(|e| e.accessible_label().map(|l| l == label).unwrap_or(false))
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

    // The name shares its line with the arrows: find the title Text (wide,
    // vertically aligned with the toggle) and require it to end where the
    // move arrows begin and to stay above the action line.
    let toggle = row_controls(app)
        .into_iter()
        .find(|(name, _)| name.starts_with("ToggleSwitch"));
    let up = by_label(app, "AddonIconButton", "Move up");
    if app.window().size().width < 700
        && let (Some((_, t)), Some(up)) = (toggle, up)
    {
        let (ux, _, _, _) = rect(&up);
        let band = (t.1, t.1 + t.3);
        let title = ElementHandle::find_by_element_type_name(app, "Text")
            .map(|e| rect(&e))
            .find(|(x, y, w, h)| {
                *w > 60.0
                    && *x > t.0 + t.2
                    && y + h / 2.0 >= band.0 - 4.0
                    && y + h / 2.0 <= band.1 + 4.0
            });
        match title {
            Some((x, y, w, h)) => {
                if x + w > ux + 0.5 {
                    failures.borrow_mut().push(format!(
                        "{label}: addon name runs under the move arrows (title right {rx:.1}, arrows at {ux:.1})",
                        rx = x + w,
                    ));
                }
                // The action line is the lowest control band in the row:
                // controls below the title band.
                let actions_top = row_controls(app)
                    .into_iter()
                    .filter(|(_, r)| r.1 > band.1 - 4.0)
                    .map(|(_, r)| r.1)
                    .fold(f32::MAX, f32::min);
                if actions_top.is_finite() && y + h > actions_top + 0.5 {
                    failures.borrow_mut().push(format!(
                        "{label}: addon name reaches into the action line (title bottom {:.1}, actions at {:.1})",
                        y + h,
                        actions_top
                    ));
                }
            }
            None => failures.borrow_mut().push(format!(
                "{label}: DIAG: no title Text found on the toggle line"
            )),
        }
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
            url: s("https://v3-cinemeta.strem.io/manifest.json"),
            enabled: true,
            config_url: s("https://v3-cinemeta.strem.io/configure"),
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

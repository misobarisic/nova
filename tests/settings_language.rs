//! Settings → Display → Language (headless).
//!
//! Guards the i18n plumbing end to end: the Display subpage renders the
//! Language row whose picker labels come from the backend's language list
//! (`language_names`, i.e. `nova_config::Language::ALL`), a pick reaches the
//! settings autosave with the picked index, and the bundled catalogs are
//! really in the binary — switching to Croatian re-renders the section header
//! and the row title, the rest of the app follows (My Library's heading, chip
//! and pluralized item count), and the automatic category *values* stay
//! English while only their labels are localized. English (the source
//! language, whose catalog is header-only) reads back as the source text.
//!
//! The interpreter-backed UI (`cargo test --features live-preview` on Linux)
//! has no catalogs at all — `src/app/i18n.rs` reports that once and the strings
//! stay English — so this test expects the Croatian renderings only when the
//! build bundles them, and asserts the English fallback otherwise.
//!
//! One test function: the testing backend initializes once per process.

#[path = "support/destinations.rs"]
mod destinations;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn fail(failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str) {
    if !cond {
        failures.borrow_mut().push(msg.to_string());
    }
}

/// A minimal library card (the Library page's item count and grid).
fn card(id: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: s(&format!("id{id}")),
        title: s(&format!("Title {id}")),
        year: s("2024"),
        poster_path: SharedString::default(),
        poster: Default::default(),
        is_loaded: false,
        badge: SharedString::default(),
        watched: false,
        ..Default::default()
    }
}

fn rendered(app: &nova::AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(app, text).count() >= 1
}

/// The language names in the picker are written in their own language and are
/// never translated, so both are on screen in every language.
fn check_picker_chips(app: &nova::AppWindow, failures: &Rc<RefCell<Vec<String>>>) {
    for chip in ["English", "Hrvatski"] {
        fail(
            failures,
            rendered(app, chip),
            &format!("the picker must offer {chip:?} in every language"),
        );
    }
}

/// The Settings page in one language: the section header and the row title it
/// should show, plus the strings that must *not* be on screen.
fn check_language(
    app: &nova::AppWindow,
    failures: &Rc<RefCell<Vec<String>>>,
    language: &str,
    header: &str,
    title: &str,
    absent: &[&str],
) {
    for text in [header, title] {
        fail(
            failures,
            rendered(app, text),
            &format!("{language}: the Display subpage must render {text:?}"),
        );
    }
    for text in absent {
        fail(
            failures,
            !rendered(app, text),
            &format!("{language}: {text:?} must not be on screen"),
        );
    }
    check_picker_chips(app, failures);
}

/// The row itself must render at this width and stay inside the window (the
/// picker shares its row width with the grid-column pickers above it).
fn check_row_fits(
    app: &nova::AppWindow,
    width: f32,
    failures: &Rc<RefCell<Vec<String>>>,
    title: &str,
) {
    let label = format!("{width}px");
    fail(
        failures,
        rendered(app, title),
        &format!("{label}: the Language row must render {title:?}"),
    );
    for text in [title, "English", "Hrvatski"] {
        for element in ElementHandle::find_by_accessible_label(app, text) {
            let x = element.absolute_position().x;
            let right = x + element.size().width;
            fail(
                failures,
                x >= -0.5 && right <= width + 0.5,
                &format!("{label}: {text:?} must stay inside the window ({x:.1}..{right:.1})"),
            );
        }
    }
}

#[test]
fn display_language_picker_renders_and_reaches_the_backend() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.on_settings_search_matches(|query, haystack| {
        nova_ui::settings_search_matches(&query, &haystack)
    });
    app.window().set_size(slint::PhysicalSize::new(1100, 800));
    app.window().show().unwrap();
    app.set_show_settings(true);
    app.set_show_home(false);
    // The backend pushes the picker list (`settings_to_ui`) from
    // `Language::ALL`; the component's own default carries the same entries.
    app.set_language_names(Rc::new(VecModel::from(vec![s("English"), s("Hrvatski")])).into());

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // The autosave callback the backend wires to persist the pick; the value
    // it reads back is the language_index the row wrote.
    let saved: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let saved = saved.clone();
        let weak = app.as_weak();
        app.on_save_settings(move || {
            if let Some(app) = weak.upgrade() {
                saved.borrow_mut().push(app.get_language_index());
            }
        });
    }

    // The bundled catalogs are what make the setting do anything: a selectable
    // code must be bundled, and nothing else may claim to be. The
    // interpreter-backed UI (`--features live-preview`) bundles none, so the
    // Croatian expectations below are keyed off this rather than assumed.
    let catalogs = slint::select_bundled_translation("en").is_ok();
    fail(
        &failures,
        catalogs == slint::select_bundled_translation("hr").is_ok(),
        "both selectable languages must be bundled (or neither, in the interpreter build)",
    );
    fail(
        &failures,
        slint::select_bundled_translation("zz-ZZ").is_err(),
        "an unbundled language must not be selectable",
    );
    // English is the source language: selecting it restores the source strings.
    let _ = slint::select_bundled_translation("en");
    // What the Display subpage shows after switching to Croatian: with the
    // catalogs bundled, the Croatian strings; without them, the English source
    // text the interpreter build falls back to.
    let (hr_header, hr_title): (&'static str, &'static str) = if catalogs {
        ("Prikaz", "Jezik")
    } else {
        ("Display", "Language")
    };
    let hr_absent: &'static [&'static str] = if catalogs {
        &["Display", "Language"]
    } else {
        &[]
    };
    // My Library in the same two flavours: translated (heading, "All" chip,
    // pluralized item count) or the English source text the interpreter build
    // falls back to.
    let library_expected: &'static [&'static str] = if catalogs {
        &["Moja biblioteka", "Sve", "(3 stavke)"]
    } else {
        &["My Library", "All", "(3 items)"]
    };

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();
        // Display is the 4th landing entry (index 3).
        let Some(display) = ({
            app.set_settings_search_query("Display".into());
            destinations::find(&app, "settings:display").next()
        }) else {
            fail(&failures1, false, "DIAG: no Display landing entry");
            slint::quit_event_loop().unwrap();
            return;
        };
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        let saved2 = saved.clone();
        slint::spawn_local(async move {
            display
                .single_click(slint::platform::PointerEventButton::Left)
                .await;
            after(400, move || {
                let app = app2.upgrade().unwrap();
                // English: the marked strings render their source text.
                check_language(
                    &app,
                    &failures2,
                    "en",
                    "Display",
                    "Language",
                    &["Prikaz", "Jezik"],
                );
                check_row_fits(&app, 1100.0, &failures2, "Language");

                // Croatian, exactly as `Bridge::apply_language` switches it:
                // the same window re-renders the section header and the row
                // title. With the catalogs bundled that is the Croatian text
                // (`hr_*` above); the interpreter build keeps English.
                let _ = slint::select_bundled_translation("hr");
                let app3 = app.as_weak();
                let failures3 = failures2.clone();
                let saved3 = saved2.clone();
                after(400, move || {
                    let app = app3.upgrade().unwrap();
                    check_language(&app, &failures3, "hr", hr_header, hr_title, hr_absent);

                    // Back to English.
                    let _ = slint::select_bundled_translation("en");
                    let app4 = app.as_weak();
                    let failures4 = failures3.clone();
                    let saved4 = saved3.clone();
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        check_language(
                            &app,
                            &failures4,
                            "en",
                            "Display",
                            "Language",
                            &["Prikaz", "Jezik"],
                        );

                        // Phone width: the picker has to fit next to the row
                        // title, so scroll the Display card down to its last row.
                        app.window().set_size(slint::PhysicalSize::new(360, 800));
                        let app5 = app.as_weak();
                        let failures5 = failures4.clone();
                        let saved5 = saved4.clone();
                        after(400, move || {
                            let app = app5.upgrade().unwrap();
                            app.set_settings_scroll_y(0.0);
                            let app6 = app.as_weak();
                            let failures6 = failures5.clone();
                            let saved6 = saved5.clone();
                            after(300, move || {
                                let app = app6.upgrade().unwrap();
                                check_row_fits(&app, 360.0, &failures6, "Language");

                                // The pick reaches the settings autosave with
                                // the language's picker index (what
                                // `Bridge::save_settings` persists and applies).
                                let Some(chip) =
                                    ElementHandle::find_by_accessible_label(&app, "Hrvatski").next()
                                else {
                                    fail(&failures6, false, "DIAG: no Hrvatski segment");
                                    slint::quit_event_loop().unwrap();
                                    return;
                                };
                                let app7 = app.as_weak();
                                let failures7 = failures6.clone();
                                let saved7 = saved6.clone();
                                slint::spawn_local(async move {
                                    chip.single_click(slint::platform::PointerEventButton::Left)
                                        .await;
                                    after(700, move || {
                                        let app = app7.upgrade().unwrap();
                                        fail(
                                            &failures7,
                                            app.get_language_index() == 1,
                                            &format!(
                                                "picking Croatian must select index 1 (got {})",
                                                app.get_language_index()
                                            ),
                                        );
                                        fail(
                                            &failures7,
                                            saved7.borrow().last() == Some(&1),
                                            &format!(
                                                "the pick must reach the settings autosave with index 1 (got {:?})",
                                                saved7.borrow()
                                            ),
                                        );

                                        // The rest of the app follows the same
                                        // catalog: My Library renders (and
                                        // pluralizes) in Croatian, with the
                                        // automatic bucket localized for the
                                        // label only — `category_names` keeps
                                        // the stored identifier the filter and
                                        // `auto_bucket` are keyed on.
                                        let _ = slint::select_bundled_translation("hr");
                                        app.set_language_names(
                                            Rc::new(VecModel::from(vec![
                                                s("English"),
                                                s("Hrvatski"),
                                            ]))
                                            .into(),
                                        );
                                        app.set_library_category_names(
                                            Rc::new(VecModel::from(vec![
                                                s("Plan to Watch"),
                                                s("Watching"),
                                            ]))
                                            .into(),
                                        );
                                        app.set_library_category_labels(
                                            Rc::new(VecModel::from(vec![
                                                s("Za pogledati"),
                                                s("Gledam"),
                                            ]))
                                            .into(),
                                        );
                                        app.set_library(
                                            Rc::new(VecModel::from(vec![
                                                card(1),
                                                card(2),
                                                card(3),
                                            ]))
                                            .into(),
                                        );
                                        app.set_show_settings(false);
                                        app.set_show_library(true);
                                        let app8 = app.as_weak();
                                        let failures8 = failures7.clone();
                                        after(400, move || {
                                            let app = app8.upgrade().unwrap();
                                            for text in library_expected {
                                                fail(
                                                    &failures8,
                                                    rendered(&app, text),
                                                    &format!(
                                                        "My Library must render {text:?}"
                                                    ),
                                                );
                                            }
                                            fail(
                                                &failures8,
                                                app.get_library_category_names().row_count() == 2,
                                                "the stored category values must not be translated",
                                            );

                                            // And English comes back.
                                            let _ = slint::select_bundled_translation("en");
                                            let app9 = app.as_weak();
                                            let failures9 = failures8.clone();
                                            after(400, move || {
                                                let app = app9.upgrade().unwrap();
                                                fail(
                                                    &failures9,
                                                    rendered(&app, "My Library")
                                                        && !rendered(&app, "Moja biblioteka"),
                                                    "switching back to English must restore the source text",
                                                );
                                                // (Both hold in the interpreter
                                                // build, which never leaves
                                                // English.)
                                                slint::quit_event_loop().unwrap();
                                            });
                                        });
                                    });
                                })
                                .unwrap();
                            });
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
        "settings language failures:\n  {}",
        failures.join("\n  ")
    );
}

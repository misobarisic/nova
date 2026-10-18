//! Whole-library consent stays account-specific and separate from existing
//! title links; new-release prompts wait until the current activity is closed.
#![cfg(not(feature = "live-preview"))]

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..16 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn control(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label).find(|element| {
        matches!(
            element.accessible_role(),
            Some(AccessibleRole::Button | AccessibleRole::Checkbox)
        )
    })
}

fn assert_reachable(target: &ElementHandle, width: u32, height: u32) {
    let position = target.absolute_position();
    let size = target.size();
    assert!(
        size.width >= 44.0 && size.height >= 44.0,
        "touch target shrank: {size:?}"
    );
    assert!(position.x >= 0.0 && position.x + size.width <= width as f32 + 0.5);
    assert!(position.y >= 0.0 && position.y + size.height <= height as f32 + 0.5);
}

fn settings_control(app: &nova::AppWindow, label: &str) -> ElementHandle {
    for attempt in 0..16 {
        if let Some(target) = control(app, label) {
            let y = target.absolute_position().y;
            if (160.0..=650.0).contains(&y) {
                return target;
            }
            app.set_settings_scroll_y((app.get_settings_scroll_y() + 340.0 - y).min(0.0));
        } else {
            app.set_settings_scroll_y(if attempt == 0 {
                0.0
            } else {
                app.get_settings_scroll_y() - 180.0
            });
        }
        settle(app);
    }
    panic!("cannot reveal Settings action {label}");
}

fn click_settings(app: &nova::AppWindow, label: &str, width: u32) {
    let target = settings_control(app, label);
    assert_reachable(&target, width, 900);
    target.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}

fn app() -> nova::AppWindow {
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.window().set_size(slint::PhysicalSize::new(360, 900));
    app.window().show().unwrap();
    settle(&app);
    app
}

#[test]
fn library_review_requires_a_revision_and_explicit_history_consent_for_each_account() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::library_review_requires_a_revision_and_explicit_history_consent_for_each_account"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = app();
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.on_tracking_library_action(move |service, action, revision, history| {
        recorded
            .borrow_mut()
            .push((service, action, revision.to_string(), history));
    });
    let title_actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = title_actions.clone();
    app.on_tracking_action(move |id, action| recorded.borrow_mut().push((id.to_string(), action)));
    let manual_links = Rc::new(VecModel::from(vec![nova::TrackingLinkRow {
        id: "manually-approved-season".into(),
        title: "Already approved season".into(),
        ..Default::default()
    }]));
    app.set_tracking_links(manual_links.clone().into());

    for (language, review, start, history_label, per_title) in [
        (
            "en",
            "Review whole library",
            "Start whole-library tracking",
            "Include existing watched history",
            "Use per-title tracking",
        ),
        (
            "hr",
            "Pregledaj cijelu zbirku",
            "Pokreni praćenje cijele zbirke",
            "Uključi postojeću povijest gledanja",
            "Koristi praćenje po naslovu",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [360, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            for (service, name) in [(0, "MyAnimeList"), (1, "AniList")] {
                let mut row = nova::TrackingAccountRow {
                    service,
                    name: "viewer".into(),
                    connected: true,
                    ..Default::default()
                };
                let model = Rc::new(VecModel::from(vec![row.clone()]));
                app.set_tracking_accounts(model.clone().into());
                app.set_settings_scroll_y(0.0);
                settle(&app);
                let before = events.borrow().len();
                if control(&app, review).is_none() {
                    click_settings(&app, name, width);
                }
                assert_eq!(
                    events.borrow().len(),
                    before,
                    "opening the account is not consent"
                );
                click_settings(&app, review, width);
                assert_eq!(
                    &events.borrow()[before..],
                    &[(service, 0, String::new(), false)]
                );

                // A review still being prepared cannot authorize a stale or
                // empty source snapshot, even if the card is already visible.
                row.library_review = true;
                model.set_row_data(0, row.clone());
                settle(&app);
                assert_eq!(
                    settings_control(&app, start).accessible_enabled(),
                    Some(false)
                );
                let revision = format!("{language}:{width}:{service}:source-a");
                row.library_revision = revision.clone().into();
                row.library_details = "One confirmed release; other titles remain pending".into();
                model.set_row_data(0, row.clone());
                settle(&app);
                assert_eq!(
                    settings_control(&app, history_label).accessible_checked(),
                    Some(false)
                );
                assert_eq!(
                    events.borrow().len(),
                    before + 1,
                    "publishing a review cannot start tracking"
                );
                click_settings(&app, start, width);
                assert_eq!(events.borrow().last(), Some(&(service, 1, revision, false)));

                click_settings(&app, history_label, width);
                assert_eq!(
                    settings_control(&app, history_label).accessible_checked(),
                    Some(true)
                );
                let revised = format!("{language}:{width}:{service}:source-b");
                row.library_revision = revised.clone().into();
                model.set_row_data(0, row.clone());
                settle(&app);
                assert_eq!(
                    settings_control(&app, history_label).accessible_checked(),
                    Some(false),
                    "a new source revision requires fresh history consent"
                );
                click_settings(&app, history_label, width);
                click_settings(&app, start, width);
                assert_eq!(events.borrow().last(), Some(&(service, 1, revised, true)));

                row.library_enabled = true;
                row.library_review = false;
                model.set_row_data(0, row);
                settle(&app);
                let before_off = events.borrow().len();
                click_settings(&app, per_title, width);
                assert_eq!(
                    &events.borrow()[before_off..],
                    &[(service, 2, String::new(), false)]
                );
                assert!(
                    title_actions.borrow().is_empty(),
                    "changing account mode must not invoke title unlink actions"
                );
                assert_eq!(
                    manual_links.row_data(0).unwrap().id,
                    "manually-approved-season"
                );
            }
        }
    }
}

#[test]
fn title_exclusion_uses_the_reviewed_source_token_and_selected_anime_service() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::title_exclusion_uses_the_reviewed_source_token_and_selected_anime_service"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = app();
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_tracking_open(true);
    app.set_tracking_library_title_full(true);
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.on_tracking_library_action(move |service, action, token, history| {
        recorded
            .borrow_mut()
            .push((service, action, token.to_string(), history));
    });
    for (language, exclude, allow) in [
        (
            "en",
            "Exclude this title from whole-library discovery",
            "Allow this title in whole-library discovery",
        ),
        (
            "hr",
            "Isključi ovaj naslov iz praćenja cijele zbirke",
            "Dopusti ovaj naslov u praćenju cijele zbirke",
        ),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for width in [360, 1280] {
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            for service in [0, 1] {
                app.set_tracking_service(service);
                app.set_tracking_library_title_excluded(false);
                app.set_tracking_library_title_token("".into());
                settle(&app);
                assert!(
                    control(&app, exclude).is_none(),
                    "an unversioned source cannot be excluded"
                );
                let token = format!("source-native:{service}:{width}:revision-2");
                app.set_tracking_library_title_token(token.clone().into());
                settle(&app);
                let before = events.borrow().len();
                let target = control(&app, exclude).expect("title-specific exclusion action");
                assert_reachable(&target, width, 900);
                target.mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(
                    &events.borrow()[before..],
                    &[(service, 7, token.clone(), false)]
                );
                app.set_tracking_library_title_excluded(true);
                settle(&app);
                control(&app, allow)
                    .expect("reverse exclusion action")
                    .mock_single_click(slint::platform::PointerEventButton::Left);
                settle(&app);
                assert_eq!(events.borrow().last(), Some(&(service, 7, token, false)));
            }
        }
    }
}

#[test]
fn new_release_prompt_waits_for_playback_and_settings_and_routes_revision_bound_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::new_release_prompt_waits_for_playback_and_settings_and_routes_revision_bound_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = app();
    app.set_tracking_library_prompt_title(
        "A newly watched season with a long descriptive release name, an alternate-language subtitle, and several cour and edition notes that must wrap without pushing the review and dismissal buttons outside a short landscape phone window".into(),
    );
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let weak = app.as_weak();
    app.on_tracking_library_action(move |service, action, revision, history| {
        recorded
            .borrow_mut()
            .push((service, action, revision.to_string(), history));
        if action == 5 || action == 6 {
            weak.upgrade()
                .unwrap()
                .set_tracking_library_prompt_open(false);
        }
    });

    app.set_tracking_library_prompt_open(true);
    app.set_player_open(true);
    settle(&app);
    assert!(!app.get_tracking_library_prompt_visible());
    assert!(
        control(&app, "Review new releases").is_none(),
        "a new-release prompt must not cover video"
    );
    app.set_player_open(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    settle(&app);
    assert!(!app.get_tracking_library_prompt_visible());
    assert!(control(&app, "Review new releases").is_none());
    assert!(
        control(&app, "Stremio").is_some(),
        "ordinary Settings must remain usable"
    );
    app.set_show_settings(false);
    app.set_stream_selector_open(true);
    settle(&app);
    assert!(!app.get_tracking_library_prompt_visible());
    app.set_stream_selector_open(false);
    app.set_completion_rating_prompts_enabled(true);
    app.set_completion_prompt_open(true);
    settle(&app);
    assert!(
        !app.get_tracking_library_prompt_visible(),
        "completion ratings keep their active layer"
    );
    app.set_completion_prompt_open(false);
    app.set_tracking_library_prompt_open(false);
    settle(&app);
    assert!(
        events.borrow().is_empty(),
        "suppressing a prompt must not approve or dismiss it"
    );

    for (language, review, later) in [
        ("en", "Review new releases", "Not now"),
        ("hr", "Pregledaj nova izdanja", "Ne sada"),
    ] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height) in [(360, 900), (1280, 900), (720, 320)] {
            app.set_is_android(width != 1280);
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for service in [0, 1] {
                for (label, action) in [(review, 5), (later, 6)] {
                    let revision = format!("{language}:{width}:{service}:new-season-{action}");
                    app.set_tracking_library_prompt_service(service);
                    app.set_tracking_library_prompt_revision(revision.clone().into());
                    app.set_tracking_library_prompt_open(true);
                    settle(&app);
                    assert!(app.get_tracking_library_prompt_visible());
                    let target = control(&app, label).expect("new-release prompt action");
                    assert_reachable(&target, width, height);
                    let before = events.borrow().len();
                    target.mock_single_click(slint::platform::PointerEventButton::Left);
                    settle(&app);
                    assert_eq!(
                        &events.borrow()[before..],
                        &[(service, action, revision, false)]
                    );
                    assert!(!app.get_tracking_library_prompt_visible());
                }
            }
        }
    }
}

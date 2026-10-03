//! Narrow season picker: one clipped row, independent horizontal/vertical
//! gestures, selected-card reveal, and the desktop grid after a resize.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn idle(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(ms));
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, &format!("DetailPage::{id}"))
        .next()
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn season(app: &nova::AppWindow, number: usize) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, &format!("Season {number}"))
        .find(|element| {
            element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
        })
        .expect("season card")
}

fn center(element: &ElementHandle) -> LogicalPosition {
    let p = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0)
}

fn drag(app: &nova::AppWindow, start: LogicalPosition, dx: f32, dy: f32) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: start,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(150);
    for step in 1..=6 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                position: LogicalPosition::new(
                    start.x + dx * step as f32 / 6.0,
                    start.y + dy * step as f32 / 6.0,
                ),
            });
        idle(25);
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: LogicalPosition::new(start.x + dx, start.y + dy),
            button: slint::platform::PointerEventButton::Left,
        });
    idle(500);
}

fn setup() -> (nova::AppWindow, Rc<RefCell<Vec<i32>>>) {
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 1000));
    app.set_animations(false);
    app.set_touch_menus(true);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Season rail".into());
    app.set_season_names(
        Rc::new(VecModel::from(
            (1..=8)
                .map(|n| format!("Season {n}").into())
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_season_cards(
        Rc::new(VecModel::from(
            (1..=8)
                .map(|n| nova::SeasonCard {
                    name: format!("Season {n}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_season_combo_idx(0);
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (1..=15)
                .map(|n| nova::EpisodeRow {
                    text: format!("Episode {n}").into(),
                    lines: 2,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let picks = Rc::new(RefCell::new(Vec::new()));
    let picked = picks.clone();
    app.on_season_picked(move |index| picked.borrow_mut().push(index));
    app.window().show().unwrap();
    idle(400);
    (app, picks)
}

#[test]
fn narrow_seasons_stay_in_one_row_and_swipes_do_not_select_or_trap_the_page() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    quick_touch_swipes_keep_their_axis();
    let (app, picks) = setup();
    let rail = element(&app, "season_flick");
    let grid = element(&app, "season_grid");
    let first = season(&app, 1);
    let first_y = first.absolute_position().y;
    assert!(
        (season(&app, 2).absolute_position().y - first_y).abs() < 1.0,
        "narrow seasons must share one row"
    );
    assert!((rail.size().height - first.size().height).abs() < 1.0);
    assert!(rail.absolute_position().x + rail.size().width <= 360.0);
    assert!(grid.size().width > rail.size().width);
    assert!(
        (grid.size().height - first.size().height).abs() < 1.0,
        "all season cards must fit in the single row"
    );

    let before = grid.absolute_position();
    drag(
        &app,
        LogicalPosition::new(
            rail.absolute_position().x + 200.0,
            first_y + first.size().height / 2.0,
        ),
        -150.0,
        12.0,
    );
    let after = grid.absolute_position();
    assert!(
        after.x < before.x - 60.0,
        "horizontal drag must move seasons"
    );
    assert!(
        (after.y - before.y).abs() < 1.0,
        "horizontal drag must not move the page"
    );
    assert!(
        picks.borrow().is_empty(),
        "a swipe must not select a season"
    );

    // Keyboard/restored selection must expose the selected season fully.
    app.set_season_combo_idx(7);
    idle(100);
    let last = season(&app, 8);
    assert!(last.absolute_position().x >= rail.absolute_position().x - 1.0);
    assert!(
        last.absolute_position().x + last.size().width
            <= rail.absolute_position().x + rail.size().width + 1.0
    );

    let before_y = grid.absolute_position().y;
    drag(&app, center(&last), 2.0, -100.0);
    assert!(
        grid.absolute_position().y < before_y - 30.0,
        "vertical drag over seasons must scroll the page"
    );
    assert!(picks.borrow().is_empty());

    // A stationary tap still selects the card after the preceding swipes.
    let tap = center(&last);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: tap,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(150);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: tap,
            button: slint::platform::PointerEventButton::Left,
        });
    idle(100);
    assert_eq!(*picks.borrow(), vec![7]);

    app.window().set_size(slint::PhysicalSize::new(1200, 1000));
    idle(200);
    assert!(
        element(&app, "season_grid").size().height > season(&app, 1).size().height + 12.0,
        "desktop seasons must use multiple rows"
    );
    assert!(
        element(&app, "season_grid").size().width
            <= element(&app, "season_flick").size().width + 1.0
    );
}

fn touch(app: &nova::AppWindow, position: LogicalPosition, phase: i_slint_core::input::TouchPhase) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::internal(
            slint::platform::InternalEvent::Touch {
                id: 1,
                position: i_slint_core::lengths::LogicalPoint::new(position.x, position.y),
                phase,
            },
        ));
}

fn quick_touch_swipes_keep_their_axis() {
    use i_slint_core::input::TouchPhase;
    for scenario in ["diagonal", "pause", "vertical"] {
        let (app, picks) = setup();
        let rail = element(&app, "season_flick");
        let grid = element(&app, "season_grid");
        let before = grid.absolute_position();
        let start = LogicalPosition::new(rail.absolute_position().x + 180.0, center(&rail).y);
        touch(&app, start, TouchPhase::Started);
        // Move immediately, including across the gap between cards, before
        // Slint's delayed press arrives. No stationary 150ms pre-drag hold.
        idle(16);
        let mut end = start;
        for step in 1..=8 {
            let (dx, dy) = match scenario {
                "vertical" => (step as f32 * 0.5, step as f32 * -15.0),
                _ => (step as f32 * -18.0, step as f32 * 3.0),
            };
            end = LogicalPosition::new(start.x + dx, start.y + dy);
            touch(&app, end, TouchPhase::Moved);
            idle(16);
            if scenario == "pause" && step == 7 {
                // Pausing a captured drag must not hand it to the page.
                idle(350);
            }
        }
        touch(&app, end, TouchPhase::Ended);
        idle(16);
        let after = grid.absolute_position();
        if scenario == "vertical" {
            assert!(
                after.y < before.y - 30.0,
                "vertical touch must scroll the page"
            );
            assert!((after.x - before.x).abs() < 1.0);
        } else {
            assert!(
                after.x < before.x - 30.0,
                "{scenario} touch must move seasons"
            );
            assert!(
                (after.y - before.y).abs() < 1.0,
                "{scenario} touch must hold the page"
            );
        }
        idle(800);
        assert!(
            picks.borrow().is_empty(),
            "{scenario} touch must not select a season"
        );
        assert!(
            ElementHandle::find_by_element_type_name(&app, "MenuSheet")
                .next()
                .is_none(),
            "drag must not open the hold menu"
        );
        let tap = LogicalPosition::new(rail.absolute_position().x + 100.0, center(&rail).y);
        touch(&app, tap, TouchPhase::Started);
        idle(150);
        touch(&app, tap, TouchPhase::Ended);
        idle(16);
        assert_eq!(
            picks.borrow().len(),
            1,
            "tap after {scenario} must select a season"
        );
        app.window().hide().unwrap();
    }
}

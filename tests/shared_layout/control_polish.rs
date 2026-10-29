use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{
    ComponentHandle, Model, VecModel,
    platform::{PointerEventButton, WindowEvent},
};
use std::{cell::Cell, rc::Rc, time::Duration};

fn tick(app: &nova::AppWindow, ms: u64) {
    for _ in 0..ms.div_ceil(10) {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (
                item.absolute_position(),
                item.size(),
                item.computed_opacity(),
            );
        }
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .expect(label)
}
fn child(app: &nova::AppWindow, parent: &ElementHandle, id: &str) -> ElementHandle {
    let p = parent.absolute_position();
    let size = parent.size();
    ElementHandle::find_by_element_id(app, id)
        .find(|item| {
            let point = item.absolute_position();
            point.x >= p.x - 0.5
                && point.y >= p.y - 0.5
                && point.x + item.size().width <= p.x + size.width + 0.5
                && point.y + item.size().height <= p.y + size.height + 0.5
        })
        .expect(id)
}
fn click(app: &nova::AppWindow, item: &ElementHandle) {
    let p = item.absolute_position();
    let size = item.size();
    let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}
fn setup() -> nova::AppWindow {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_hover(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.window().set_size(slint::PhysicalSize::new(1280, 1000));
    app
}

#[test]
fn library_selection_and_search_animate_without_delaying_actions_or_replaying_disabled_motion() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::library_selection_and_search_animate_without_delaying_actions_or_replaying_disabled_motion"
    )) {
        return;
    }
    let app = setup();
    app.set_show_library(true);
    app.set_library_category_names(
        Rc::new(VecModel::from(vec!["Watching".into(), "Completed".into()])).into(),
    );
    app.set_library_category_labels(app.get_library_category_names());
    app.set_library_filter_category("Watching".into());
    let calls = Rc::new(Cell::new(0));
    let counted = calls.clone();
    let weak = app.as_weak();
    app.on_library_filter_picked(move |name| {
        counted.set(counted.get() + 1);
        weak.upgrade().unwrap().set_library_filter_category(name);
    });
    app.show().unwrap();
    tick(&app, 300);
    let completed = button(&app, "Completed");
    let fill = child(&app, &completed, "LibraryFilterChip::selection_fill");
    let geometry = (completed.absolute_position(), completed.size());
    assert!(fill.computed_opacity() < 0.01);
    click(&app, &completed);
    assert_eq!(app.get_library_filter_category(), "Completed");
    assert_eq!(
        calls.get(),
        1,
        "selection dispatch precedes its visual tween"
    );
    tick(&app, 50);
    let middle = fill.computed_opacity();
    assert!(
        middle > 0.02 && middle < 0.98,
        "selection stays animated with Hover effects and Page transitions off: {middle}"
    );
    assert_eq!((completed.absolute_position(), completed.size()), geometry);
    click(&app, &button(&app, "Watching"));
    tick(&app, 30);
    assert!(
        fill.computed_opacity() < middle,
        "rapid selection reverses the current tween"
    );
    app.global::<nova::Anim>().set_enabled(false);
    tick(&app, 10);
    assert!(fill.computed_opacity() < 0.01);
    app.global::<nova::Anim>().set_enabled(true);
    tick(&app, 10);
    assert!(
        fill.computed_opacity() < 0.01,
        "re-enabling must not expose a cancelled selection tween"
    );
    tick(&app, 180);
    assert!(
        fill.computed_opacity() < 0.01,
        "old selection motion must not resume"
    );

    click(&app, &button(&app, "List view"));
    assert!(app.get_library_list_view(), "layout selection is immediate");
    let search = element(&app, "LibraryPage::library_search_button");
    let close = element(&app, "LibraryPage::search_close_icon");
    assert!(close.computed_opacity() < 0.01);
    click(&app, &search);
    assert!(app.get_library_search_open());
    tick(&app, 50);
    assert!(close.computed_opacity() > 0.02 && close.computed_opacity() < 0.98);
    app.global::<nova::Anim>().set_enabled(false);
    tick(&app, 10);
    assert!(close.computed_opacity() > 0.99);
    app.global::<nova::Anim>().set_enabled(true);
    tick(&app, 10);
    assert!(
        close.computed_opacity() > 0.99,
        "re-enabling must not revive an old icon crossfade"
    );
    tick(&app, 180);
    assert!(close.computed_opacity() > 0.99);

    // Shared dropdowns still open, select and close immediately during motion.
    click(&app, &element(&app, "LibraryPage::library_sort"));
    tick(&app, 20);
    let title = ElementHandle::find_by_accessible_label(&app, "Title")
        .last()
        .unwrap();
    // Popups can use a distinct testing window; dispatch through the row's
    // adapter rather than interpreting its coordinates in the parent window.
    title.mock_single_click(PointerEventButton::Left);
    assert_eq!(app.get_library_sort(), 1);
    tick(&app, 20);
    assert!(
        ElementHandle::find_by_element_id(&app, "Dropdown::popup_scope")
            .next()
            .is_none()
    );
}

#[test]
fn calendar_selection_fades_in_retained_date_controls_with_immediate_picks() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::calendar_selection_fades_in_retained_date_controls_with_immediate_picks"
    )) {
        return;
    }
    let app = setup();
    app.set_show_calendar(true);
    app.set_calendar_view(0);
    let rows = Rc::new(VecModel::from(
        (0..42)
            .map(|epoch| nova::CalendarDay {
                cell: nova::CalCell {
                    epoch,
                    day: epoch + 1,
                    in_month: true,
                    selected: epoch == 0,
                    ..Default::default()
                },
                weekday: "Mon".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    app.set_calendar_cells(rows.clone().into());
    let picked = Rc::new(Cell::new(-1));
    let recorded = picked.clone();
    let model = rows.clone();
    app.on_calendar_pick(move |day| {
        recorded.set(day);
        for index in 0..model.row_count() {
            let mut row = model.row_data(index).unwrap();
            row.cell.selected = row.cell.epoch == day;
            model.set_row_data(index, row);
        }
    });
    app.show().unwrap();
    tick(&app, 300);
    let target = button(&app, "Mon 2");
    let fill = child(&app, &target, "PlannerDay::selection_fill");
    let geometry = (target.absolute_position(), target.size());
    assert!(fill.computed_opacity() < 0.01);
    click(&app, &target);
    assert_eq!(picked.get(), 1);
    tick(&app, 50);
    assert!(fill.computed_opacity() > 0.02 && fill.computed_opacity() < 0.98);
    assert_eq!((target.absolute_position(), target.size()), geometry);
    app.global::<nova::Anim>().set_enabled(false);
    tick(&app, 10);
    assert!(fill.computed_opacity() > 0.99);
    app.global::<nova::Anim>().set_enabled(true);
    tick(&app, 10);
    assert!(
        fill.computed_opacity() > 0.99,
        "date selection must stay settled immediately after re-enable"
    );
    tick(&app, 200);
    assert!(fill.computed_opacity() > 0.99);
}

#[test]
fn detail_body_fade_retargets_tabs_and_snaps_when_page_motion_is_disabled() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::detail_body_fade_retargets_tabs_and_snaps_when_page_motion_is_disabled"
    )) {
        return;
    }
    let app = setup();
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_selected_title("A series".into());
    app.set_season_names(Rc::new(VecModel::from(vec!["Season 1".into()])).into());
    app.set_episode_rows(
        Rc::new(VecModel::from(vec![nova::EpisodeRow {
            text: "Episode 1".into(),
            ..Default::default()
        }]))
        .into(),
    );
    app.set_episode_total(1);
    app.show().unwrap();
    tick(&app, 300);
    app.global::<nova::Anim>().set_transitions(true);
    app.set_detail_tab(3);
    assert_eq!(app.get_detail_tab(), 3);
    tick(&app, 60);
    let opacity = element(&app, "DetailPage::body_content").computed_opacity();
    assert!(
        opacity > 0.01 && opacity < 0.99,
        "new body has an intermediate entrance frame: {opacity}"
    );
    app.set_detail_tab(0);
    tick(&app, 40);
    assert!(element(&app, "DetailPage::body_content").computed_opacity() < 0.99);
    app.global::<nova::Anim>().set_transitions(false);
    tick(&app, 10);
    assert!(element(&app, "DetailPage::body_content").computed_opacity() > 0.99);
    app.global::<nova::Anim>().set_transitions(true);
    tick(&app, 10);
    assert!(
        element(&app, "DetailPage::body_content").computed_opacity() > 0.99,
        "re-enabling page motion must not revive the previous entrance"
    );
    tick(&app, 250);
    assert!(element(&app, "DetailPage::body_content").computed_opacity() > 0.99);
    app.set_detail_tab(3);
    tick(&app, 40);
    app.global::<nova::Anim>().set_enabled(false);
    tick(&app, 10);
    assert!(element(&app, "DetailPage::body_content").computed_opacity() > 0.99);
    app.global::<nova::Anim>().set_enabled(true);
    tick(&app, 10);
    assert!(element(&app, "DetailPage::body_content").computed_opacity() > 0.99);
}

//! Discover card reveals survive poster updates; browse filters pan horizontally.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn card(index: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: format!("result:{index}").into(),
        title: format!("Search movie {index}").into(),
        year: "2026".into(),
        ..Default::default()
    }
}

fn check_visible(app: &nova::AppWindow, failures: &RefCell<Vec<String>>, count: usize) {
    for index in 0..count {
        let label = format!("Search movie {index}");
        match ElementHandle::find_by_accessible_label(app, &label).next() {
            Some(element) if element.computed_opacity() > 0.99 => {}
            Some(element) => failures.borrow_mut().push(format!(
                "{label} remained invisible (opacity {})",
                element.computed_opacity()
            )),
            None => failures
                .borrow_mut()
                .push(format!("{label} was not rendered")),
        }
    }
}

// The headless backend doesn't render frames. Read animated bindings during
// the transition, as a renderer would, instead of only evaluating them at its end.
fn sample_reveal_frames(app: &nova::AppWindow) -> Rc<Cell<bool>> {
    let animated = Rc::new(Cell::new(false));
    for delay in (20..800).step_by(20) {
        let weak = app.as_weak();
        let animated = animated.clone();
        after(delay, move || {
            let app = weak.upgrade().unwrap();
            for index in 0..4 {
                if let Some(element) =
                    ElementHandle::find_by_accessible_label(&app, &format!("Search movie {index}"))
                        .next()
                {
                    let opacity = element.computed_opacity();
                    if opacity > 0.01 && opacity < 0.99 {
                        animated.set(true);
                    }
                }
            }
        });
    }
    animated
}

#[test]
fn poster_updates_cannot_strand_reveals_and_filters_are_draggable() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_discover_scroll_page(true);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    let names = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|value| SharedString::from(*value))
                .collect::<Vec<_>>(),
        ))
        .into()
    };
    app.set_addon_names(names(&["All addons", "Cinemeta"]));
    app.set_type_names(names(&["movie", "series"]));
    app.set_catalog_names(names(&["Popular", "Top rated"]));
    app.set_genre_names(names(&["All genres", "Action"]));
    app.set_catalog(Rc::new(VecModel::from((0..20).map(card).collect::<Vec<_>>())).into());
    let failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        let rail = ElementHandle::find_by_element_id(&app, "DiscoverHeader::filter_flick")
            .next()
            .expect("Discover filter Flickable");
        let filter_row = ElementHandle::find_by_element_id(&app, "DiscoverHeader::filter_row")
            .next()
            .expect("filter row");
        let before_x = filter_row.absolute_position().x;
        let pos = rail.absolute_position();
        let start = LogicalPosition::new(pos.x + 160.0, pos.y + rail.size().height / 2.0);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: start,
                button: slint::platform::PointerEventButton::Left,
            });
        for step in 1..=6 {
            let weak = app.as_weak();
            after(step * 25, move || {
                weak.upgrade().unwrap().window().dispatch_event(
                    slint::platform::WindowEvent::PointerMoved {
                        position: LogicalPosition::new(start.x - step as f32 * 20.0, start.y),
                    },
                );
            });
        }
        let weak = app.as_weak();
        after(180, move || {
            weak.upgrade().unwrap().window().dispatch_event(
                slint::platform::WindowEvent::PointerReleased {
                    position: LogicalPosition::new(start.x - 120.0, start.y),
                    button: slint::platform::PointerEventButton::Left,
                },
            );
        });
        let app2 = app.as_weak();
        after(500, move || {
            let app = app2.upgrade().unwrap();
            if filter_row.absolute_position().x >= before_x - 30.0 {
                failures1.borrow_mut().push(format!("dragging the filter rail did not pan its dropdowns: x {before_x} -> {}, rail width {}, row width {}", filter_row.absolute_position().x, rail.size().width, filter_row.size().width));
            }
            if app.get_discover_scroll_y().abs() > 1.0 {
                failures1
                    .borrow_mut()
                    .push("horizontal filter drag moved the page vertically".into());
            }
            app.set_discover_search_open(true);
            let results = Rc::new(VecModel::from((0..8).map(card).collect::<Vec<_>>()));
            app.set_search_results(results.clone().into());
            let animated = sample_reveal_frames(&app);
            // Deliver poster rows both before and during the stagger. This
            // cancelled the previous shared reveal timer, leaving black cards.
            for delay in [10, 25, 80, 160] {
                let results = results.clone();
                after(delay, move || {
                    let mut row = results.row_data(0).unwrap();
                    row.is_loaded = !row.is_loaded;
                    results.set_row_data(0, row);
                });
            }
            let app3 = app.as_weak();
            after(850, move || {
                let app = app3.upgrade().unwrap();
                check_visible(&app, &failures1, 4);
                if !animated.get() {
                    failures1
                        .borrow_mut()
                        .push("search cards never faded through intermediate opacity".into());
                }
                // Same-length replacements (new query) must also reveal.
                app.set_search_results(
                    Rc::new(VecModel::from((0..8).map(card).collect::<Vec<_>>())).into(),
                );
                let animated = sample_reveal_frames(&app);
                let app4 = app.as_weak();
                after(850, move || {
                    let app = app4.upgrade().unwrap();
                    check_visible(&app, &failures1, 4);
                    if !animated.get() {
                        failures1
                            .borrow_mut()
                            .push("replacement search cards did not animate".into());
                    }
                    app.set_modal_visible(true);
                    let app5 = app.as_weak();
                    after(80, move || {
                        let app = app5.upgrade().unwrap();
                        app.set_modal_visible(false);
                        let app6 = app.as_weak();
                        after(20, move || {
                            let app = app6.upgrade().unwrap();
                            // Return from detail is immediately visible, before
                            // a fresh 40ms entrance timer could even fire.
                            check_visible(&app, &failures1, 4);
                            app.global::<nova::Anim>().set_enabled(false);
                            app.set_search_results(
                                Rc::new(VecModel::from((0..10).map(card).collect::<Vec<_>>()))
                                    .into(),
                            );
                            let app7 = app.as_weak();
                            after(80, move || {
                                let app = app7.upgrade().unwrap();
                                check_visible(&app, &failures1, 4);
                                slint::quit_event_loop().unwrap();
                            });
                        });
                    });
                });
            });
        });
    });
    slint::run_event_loop().unwrap();
    assert!(
        failures.borrow().is_empty(),
        "{}",
        failures.borrow().join("\n")
    );
}

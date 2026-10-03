//! Stream list pagination keeps real page slices and absolute stream indexes.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(value: &str) -> SharedString {
    SharedString::from(value)
}

fn row(index: usize) -> nova::StreamRow {
    nova::StreamRow {
        id: s(&format!("stream-{index}")),
        text: s(&format!("Stream {index}")),
        details: SharedString::default(),
        lines: 1,
        is_download: false,
        download_progress: 0.0,
        download_action: 0,
    }
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn click_then(
    app: &nova::AppWindow,
    element: ElementHandle,
    body: impl FnOnce(nova::AppWindow) + 'static,
) {
    let weak = app.as_weak();
    slint::spawn_local(async move {
        element
            .single_click(slint::platform::PointerEventButton::Left)
            .await;
        after(300, move || body(weak.upgrade().unwrap()));
    })
    .unwrap();
}

#[test]
fn second_page_is_populated_and_picks_absolute_index() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    // Include the movie hero’s library control while keeping both pagers
    // in view; the test clicks the bottom one rather than scrolling to it.
    app.window().set_size(slint::PhysicalSize::new(900, 800));
    app.window().show().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_modal_visible(true);
    app.set_selected_title(s("Movie"));
    app.set_detail_tab(0);
    app.set_modal_episodes(false);
    app.set_stream_total(27);
    app.set_stream_page_count(2);
    app.set_stream_page(0);
    app.set_stream_page_start(0);
    // A short representative page keeps both controls on-screen for this
    // layout check; `stream_total` still advertises multiple pages.
    app.set_streams(Rc::new(VecModel::from((0..2).map(row).collect::<Vec<_>>())).into());

    let pages = Rc::new(RefCell::new(Vec::new()));
    let picks = Rc::new(RefCell::new(Vec::new()));
    {
        let pages = pages.clone();
        app.on_stream_page_picked(move |page| pages.borrow_mut().push(page));
    }
    {
        let picks = picks.clone();
        app.on_stream_picked(move |index| picks.borrow_mut().push(index));
    }

    let failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let weak = app.as_weak();
    let pages1 = pages.clone();
    let picks1 = picks.clone();
    let failures1 = failures.clone();
    after(450, move || {
        let app = weak.upgrade().unwrap();
        let pager_count = ElementHandle::find_by_element_type_name(&app, "EpisodePager").count();
        if pager_count != 2 {
            failures1.borrow_mut().push(format!(
                "expected top and bottom pagers, found {pager_count}"
            ));
        }
        let next_count = ElementHandle::find_by_accessible_label(&app, "Next page").count();
        if next_count != 2 {
            failures1
                .borrow_mut()
                .push(format!("expected two Next controls, found {next_count}"));
        }
        let Some(next) = ElementHandle::find_by_accessible_label(&app, "Next page").nth(1) else {
            failures1
                .borrow_mut()
                .push("missing bottom Next control".into());
            slint::quit_event_loop().unwrap();
            return;
        };
        let pages2 = pages1.clone();
        let picks2 = picks1.clone();
        let failures2 = failures1.clone();
        click_then(&app, next, move |app| {
            if pages2.borrow().as_slice() != [1] {
                failures2
                    .borrow_mut()
                    .push(format!("Next page callback: {:?}", pages2.borrow()));
            }
            // Mirror the Rust bridge's page response: the UI model now holds
            // only the second-page slice, but page_start remains global.
            app.set_stream_page(1);
            app.set_stream_page_start(25);
            app.set_streams(Rc::new(VecModel::from(vec![row(25), row(26)])).into());
            let weak = app.as_weak();
            let failures3 = failures2.clone();
            let picks3 = picks2.clone();
            after(250, move || {
                let app = weak.upgrade().unwrap();
                let has_last = ElementHandle::find_by_accessible_label(&app, "Stream 26")
                    .next()
                    .is_some();
                if !has_last {
                    failures3
                        .borrow_mut()
                        .push("second-page stream rows were not rendered".into());
                    slint::quit_event_loop().unwrap();
                    return;
                }
                let Some(last) = ElementHandle::find_by_accessible_label(&app, "Stream 26").next()
                else {
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let failures4 = failures3.clone();
                click_then(&app, last, move |_| {
                    if picks3.borrow().as_slice() != [26] {
                        failures4
                            .borrow_mut()
                            .push(format!("absolute pick index: {:?}", picks3.borrow()));
                    }
                    slint::quit_event_loop().unwrap();
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

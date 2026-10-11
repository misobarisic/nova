//! Empty failed pages expose an explicit, user-triggered retry on both layouts.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

async fn settle() {
    let done = Rc::new(Cell::new(false));
    let mut scheduled = false;
    std::future::poll_fn(move |cx| {
        if done.get() {
            return std::task::Poll::Ready(());
        }
        if !scheduled {
            scheduled = true;
            let signal = done.clone();
            let waker = cx.waker().clone();
            slint::Timer::single_shot(Duration::from_millis(120), move || {
                signal.set(true);
                waker.wake();
            });
        }
        std::task::Poll::Pending
    })
    .await;
}

#[test]
fn empty_failed_pages_retry_only_after_user_action() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    let calls = Rc::new(Cell::new(0));
    let recorded = calls.clone();
    app.on_load_more(move || recorded.set(recorded.get() + 1));
    app.window().show().unwrap();
    let weak = app.as_weak();
    let completed = Rc::new(Cell::new(false));
    let finished = completed.clone();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        for size in [(320, 800), (1280, 800)] {
            app.window()
                .set_size(slint::PhysicalSize::new(size.0, size.1));
            for search in [false, true] {
                app.set_discover_search_open(search);
                app.set_can_load_more(!search);
                app.set_search_can_load_more(search);
                app.set_search_loading(false);
                app.set_loading_more(false);
                app.set_search_loading_more(false);
                let before = calls.get();
                settle().await;
                assert_eq!(calls.get(), before, "failure must wait for user retry");
                let button = ElementHandle::find_by_accessible_label(&app, "Retry")
                    .next()
                    .expect("an empty failed page must expose Retry");
                button
                    .single_click(slint::platform::PointerEventButton::Left)
                    .await;
                assert_eq!(calls.get(), before + 1);
                // A successful empty response removes Retry.
                app.set_can_load_more(false);
                app.set_search_can_load_more(false);
                settle().await;
                assert!(
                    ElementHandle::find_by_accessible_label(&app, "Retry")
                        .next()
                        .is_none()
                );
            }
        }
        finished.set(true);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::Timer::single_shot(Duration::from_secs(10), || {
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop().unwrap();
    assert!(completed.get());
}

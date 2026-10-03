//! Scroll a Settings control into the viewport before exercising its pointer action.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub async fn click(app: &nova::AppWindow, element: &ElementHandle) {
    if app.get_show_settings()
        && app.get_settings_detail_open()
        && !element
            .accessible_id()
            .is_some_and(|s| s.starts_with("settings:"))
    {
        let y = element.absolute_position().y;
        let h = element.size().height;
        let viewport_h = app.window().size().height as f32;
        if y < 40.0 || y + h > viewport_h - 40.0 {
            app.set_settings_scroll_y(app.get_settings_scroll_y() + viewport_h / 2.0 - y - h / 2.0);
            settle().await;
        }
    }
    element
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
}

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let timer_ready = ready.clone();
    let timer_waker = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(60), move || {
        timer_ready.set(true);
        if let Some(waker) = timer_waker.borrow_mut().take() {
            waker.wake();
        }
    });
    std::future::poll_fn(|cx| {
        if ready.get() {
            std::task::Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
    .await;
}

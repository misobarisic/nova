//! Season review keeps confirmation explicit and cancellation reachable on
//! phones, short landscape and desktop, including native Android Back.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..10 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
        slint::platform::update_timers_and_animations();
    }
}

fn button(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, &format!("DetailStreamsModule::{id}"))
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing review button {id}"))
}

#[test]
fn season_review_keeps_confirm_cancel_visible_and_back_cancels() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::season_review_keeps_confirm_cancel_visible_and_back_cancels"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_touch_menus(true);
    app.set_is_android(true);
    app.set_modal_visible(true);
    app.set_detail_tab(3);
    app.set_selected_title("A show with a long season title".into());
    let confirms = Rc::new(Cell::new(0));
    let count = confirms.clone();
    app.on_season_download_confirm(move || count.set(count.get() + 1));
    let cancels = Rc::new(Cell::new(0));
    let count = cancels.clone();
    app.on_season_download_cancel(move || count.set(count.get() + 1));
    let count = cancels.clone();
    app.on_stream_selector_back(move || count.set(count.get() + 1));
    app.show().unwrap();

    for (width, height) in [(360, 800), (900, 360), (1366, 768)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        for language in ["en", "hr"] {
            slint::select_bundled_translation(language).unwrap();
            app.set_stream_selector_open(true);
            app.set_season_download_active(true);
            app.set_season_download_review(true);
            app.set_season_download_status(
                nova::backend_text::season_download_summary(
                    12_000_000_000,
                    3,
                    Some(40_000_000_000),
                )
                .into(),
            );
            app.set_season_download_rows(
                Rc::new(VecModel::from(
                    (1..=100)
                        .map(|episode| {
                            format!(
                                "S1 E{episode} · A.long.release.name.1080p.WEB-DL.HEVC-GROUP · 1.2 GB"
                            )
                            .into()
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            settle(&app);
            let bridge = app.global::<nova::DetailBridge>();
            bridge.set_safe_top(20.0);
            bridge.set_safe_bottom(16.0);
            bridge.set_safe_left(8.0);
            bridge.set_safe_right(8.0);
            settle(&app);
            let confirm = button(&app, "review_confirm");
            let cancel = button(&app, "review_cancel");
            let scroll =
                ElementHandle::find_by_element_id(&app, "DetailStreamsModule::review_scroll")
                    .next()
                    .expect("review scroll viewport");
            assert!(
                scroll.size().height >= 100.0,
                "review remains readable in short landscape"
            );
            for action in [&confirm, &cancel] {
                let p = action.absolute_position();
                let size = action.size();
                assert!(size.width >= 44.0 && size.height >= 44.0);
                assert!(p.x >= 8.0 && p.y >= 20.0);
                assert!(
                    p.x + size.width <= width as f32 - 7.0
                        && p.y + size.height <= height as f32 - 15.0,
                    "review action fits at {width}x{height} in {language}: {p:?} {size:?}"
                );
            }
            let before_confirm = confirms.get();
            let before_cancel = cancels.get();
            // No pointer interaction or explicit focus repair before Back.
            // The same capture lane used on Android must already be alive.
            for event in [
                slint::platform::WindowEvent::KeyPressed {
                    text: slint::platform::Key::Back.into(),
                },
                slint::platform::WindowEvent::KeyPressRepeated {
                    text: slint::platform::Key::Back.into(),
                },
                slint::platform::WindowEvent::KeyReleased {
                    text: slint::platform::Key::Back.into(),
                },
            ] {
                assert!(matches!(
                    app.window().dispatch_event_with_result(event),
                    Ok(slint::platform::WindowEventDispatchResult::Accepted)
                ));
            }
            settle(&app);
            assert_eq!(cancels.get(), before_cancel + 1);
            assert_eq!(confirms.get(), before_confirm);
            confirm.mock_single_click(slint::platform::PointerEventButton::Left);
            assert_eq!(confirms.get(), before_confirm + 1);
            assert_eq!(cancels.get(), before_cancel + 1);
            cancel.mock_single_click(slint::platform::PointerEventButton::Left);
            assert_eq!(cancels.get(), before_cancel + 2);
            assert_eq!(confirms.get(), before_confirm + 1);
            for key in [
                slint::platform::Key::RightArrow,
                slint::platform::Key::Return,
                slint::platform::Key::LeftArrow,
                slint::platform::Key::Return,
            ] {
                let text: slint::SharedString = key.into();
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                        text: text.clone(),
                    });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
            }
            assert_eq!(confirms.get(), before_confirm + 2);
            assert_eq!(cancels.get(), before_cancel + 3);
        }
    }
    slint::select_bundled_translation("en").unwrap();
}

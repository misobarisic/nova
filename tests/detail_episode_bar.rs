//! Detail → picked-episode bar (headless).
//!
//! On the Episodes tab, picking an episode puts its streams under a back bar
//! carrying the episode label (the detail top bar is icon-only, so this bar is
//! the way back to the episode list). Its chevron must sit on the bar's centre
//! line: the icon lives in a plain `Rectangle` child of a `HorizontalLayout`,
//! which Slint top-aligns (`Rectangle` has no `vertical-alignment`), so the
//! icon used to ride high over the label.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(value: &str) -> SharedString {
    SharedString::from(value)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn episode_bar_chevron_is_vertically_centered() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    // Tab 3 = Episodes: the picked-episode bar and its streams live there.
    app.set_detail_tab(3);
    app.set_modal_episodes(false);
    app.set_selected_title(s("Show"));
    app.set_episode_context(s("S1 E1 · Pilot"));
    app.set_streams(
        Rc::new(VecModel::from(vec![nova::StreamRow {
            id: s("stream-1"),
            text: s("Addon B\nShow 1080p"),
            details: s(""),
            lines: 2,
            is_download: false,
            download_progress: 0.0,
            download_action: 0,
        }]))
        .into(),
    );

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let failures1 = failures.clone();
    let app1 = app.as_weak();
    after(300, move || {
        let app = app1.upgrade().unwrap();
        let fail = |msg: String| failures1.borrow_mut().push(msg);
        let Some(bar) = ElementHandle::find_by_element_id(&app, "DetailPage::ep_ctx_touch").next()
        else {
            fail("DIAG: the picked-episode bar did not render".to_string());
            slint::quit_event_loop().unwrap();
            return;
        };
        let bar_pos = bar.absolute_position();
        let bar_centre = bar_pos.y + bar.size().height / 2.0;
        // The bar's chevron: the only `IcChevronLeft` inside the bar's band
        // (the detail top bar's back arrow sits far above it).
        let icons: Vec<_> =
            ElementHandle::find_by_element_type_name(&app, "IcChevronLeft").collect();
        let in_bar: Vec<f32> = icons
            .iter()
            .filter(|icon| {
                let position = icon.absolute_position();
                position.x < bar_pos.x + 60.0
                    && position.y > bar_pos.y - 40.0
                    && position.y + icon.size().height < bar_pos.y + 60.0
            })
            .map(|icon| icon.absolute_position().y + icon.size().height / 2.0)
            .collect();
        if in_bar.is_empty() {
            fail("DIAG: no chevron found in the picked-episode bar".to_string());
        } else if !in_bar.iter().any(|centre| (centre - bar_centre).abs() <= 1.5) {
            fail(format!(
                "the bar chevron must be vertically centered: centre {in_bar:?}, bar centre {bar_centre}",
            ));
        }
        slint::quit_event_loop().unwrap();
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "episode bar failures:\n  {}",
        failures.join("\n  ")
    );
}

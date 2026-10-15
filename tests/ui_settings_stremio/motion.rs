//! Account disclosures animate their geometry without stretching the content.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::ComponentHandle;
use std::{rc::Rc, time::Duration};

fn advance(app: &nova::AppWindow, milliseconds: u64) {
    for _ in 0..milliseconds.div_ceil(10) {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    }
}

fn header(app: &nova::AppWindow, service: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, service)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .unwrap_or_else(|| panic!("missing {service} header"))
}

fn card(app: &nova::AppWindow, index: usize) -> ElementHandle {
    if index == 2 {
        ElementHandle::find_by_element_type_name(app, "StremioSettings")
            .next()
            .unwrap()
    } else {
        ElementHandle::find_by_element_type_name(app, "AccountEditor")
            .nth(index)
            .unwrap()
    }
}

#[test]
fn account_cards_animate_reverse_and_respect_motion_switches() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::account_cards_animate_reverse_and_respect_motion_switches"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for (enabled, transitions) in [(true, true), (false, true), (true, false)] {
        let app = nova::AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(360, 1600));
        app.global::<nova::Anim>().set_enabled(enabled);
        app.global::<nova::Anim>().set_transitions(transitions);
        app.set_tracking_accounts(
            Rc::new(slint::VecModel::from(vec![
                nova::TrackingAccountRow {
                    service: 0,
                    ..Default::default()
                },
                nova::TrackingAccountRow {
                    service: 1,
                    ..Default::default()
                },
            ]))
            .into(),
        );
        app.set_show_home(false);
        app.set_show_settings(true);
        app.set_settings_selected_id(12);
        app.set_settings_detail_open(true);
        app.show().unwrap();
        advance(&app, 400);
        for (index, service) in ["MyAnimeList", "AniList", "Stremio"]
            .into_iter()
            .enumerate()
        {
            let closed = card(&app, index).size().height;
            assert!((95.0..98.0).contains(&closed));
            header(&app, service).mock_single_click(slint::platform::PointerEventButton::Left);
            advance(&app, 60);
            let opening = card(&app, index).size().height;
            advance(&app, 300);
            let open = card(&app, index).size().height;
            assert!(open > closed + 60.0, "{service} must reveal its controls");
            if enabled && transitions {
                assert!(
                    opening > closed && opening < open - 1.0,
                    "{service} opening must have an intermediate height: {closed}/{opening}/{open}"
                );
            } else {
                assert!(
                    (opening - open).abs() < 0.5,
                    "disabled motion must snap open"
                );
            }
            header(&app, service).mock_single_click(slint::platform::PointerEventButton::Left);
            advance(&app, 60);
            let closing = card(&app, index).size().height;
            if enabled && transitions {
                assert!(
                    closing > closed && closing < open,
                    "{service} closing must retain its painted content: {closed}/{closing}/{open}"
                );
                // Reopening mid-close continues from its current geometry.
                header(&app, service).mock_single_click(slint::platform::PointerEventButton::Left);
                // The simulated click holds for 50ms before release; closing
                // continues during that press, before the new target is set.
                let reversal_start = card(&app, index).size().height;
                advance(&app, 60);
                let reversed = card(&app, index).size().height;
                assert!(
                    reversal_start > closed && reversed > reversal_start && reversed < open,
                    "{service} must reverse in place: closed={closed}, start={reversal_start}, reversed={reversed}, open={open}"
                );
                advance(&app, 300);
                header(&app, service).mock_single_click(slint::platform::PointerEventButton::Left);
            } else {
                assert!(
                    (closing - closed).abs() < 0.5,
                    "disabled motion must snap closed"
                );
            }
            advance(&app, 300);
            assert!((card(&app, index).size().height - closed).abs() < 0.5);
        }
        app.hide().unwrap();
    }
}

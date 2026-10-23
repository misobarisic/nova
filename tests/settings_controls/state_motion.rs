//! Actual compiled switches animate independently of hover and page changes.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

fn control(app: &nova::AppWindow, history: bool) -> ElementHandle {
    let label = if history {
        "Include watched episodes"
    } else {
        "Relative episode dates"
    };
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Checkbox))
        .expect("compiled checkbox control")
}

fn knob_x(app: &nova::AppWindow, history: bool) -> f32 {
    let id = if history {
        "HistoryToggle::history_knob"
    } else {
        "ToggleSwitch::knob"
    };
    let selected = control(app, history);
    let position = selected.absolute_position();
    let size = selected.size();
    ElementHandle::find_by_element_id(app, id)
        .find(|thumb| {
            let p = thumb.absolute_position();
            p.x >= position.x
                && p.x <= position.x + size.width
                && p.y >= position.y
                && p.y <= position.y + size.height
        })
        .expect("retained thumb inside the chosen compiled toggle")
        .absolute_position()
        .x
}

fn advance(app: &nova::AppWindow, history: bool, millis: u64) {
    // The headless backend needs these reads to evaluate lazy animation bindings.
    for _ in 0..millis {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1));
        let _ = knob_x(app, history);
    }
}

fn click_control(app: &nova::AppWindow, history: bool) {
    let control = control(app, history);
    let p = control.absolute_position();
    let size = control.size();
    let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    // Keep press/release timing separate from the explicit animation frames.
    for event in [
        slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
        slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn fixture(history: bool) -> (nova::AppWindow, Rc<Cell<usize>>) {
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_hover(false);
    app.global::<nova::Anim>().set_transitions(false);
    app.window().set_size(slint::PhysicalSize::new(1280, 1600));
    app.set_show_home(false);
    if history {
        app.set_modal_visible(true);
        app.set_tracking_open(true);
        app.set_tracking_setup_active(true);
        app.set_tracking_setup_revision("motion-review".into());
        app.set_tracking_setup(
            Rc::new(VecModel::from(vec![nova::TrackingSetupRow {
                title: "Motion review release".into(),
                ..Default::default()
            }]))
            .into(),
        );
    } else {
        app.set_show_settings(true);
        app.set_settings_selected_id(3);
        app.set_settings_detail_open(true);
        app.set_cache_date_relative(false);
    }
    let changes = Rc::new(Cell::new(0));
    let recorded = changes.clone();
    app.on_settings_edited(move |_| recorded.set(recorded.get() + 1));
    app.show().unwrap();
    for _ in 0..12 {
        for element in ElementQuery::from_root(&app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
    (app, changes)
}

fn external_off(app: &nova::AppWindow, history: bool, revision: &str) {
    if history {
        // A new coordinator revision resets the retained history choice.
        app.set_tracking_setup_revision(revision.into());
    } else {
        app.set_cache_date_relative(false);
    }
}

#[test]
fn switches_animate_on_and_off_with_hover_and_page_transitions_disabled() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::switches_animate_on_and_off_with_hover_and_page_transitions_disabled"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for history in [false, true] {
        let (app, changes) = fixture(history);
        let anim = app.global::<nova::Anim>();
        let off = knob_x(&app, history);
        click_control(&app, history);
        assert_eq!(
            control(&app, history).accessible_checked(),
            Some(true),
            "logical state changes immediately"
        );
        let start = knob_x(&app, history);
        advance(&app, history, 45);
        let middle = knob_x(&app, history);
        advance(&app, history, 160);
        let on = knob_x(&app, history);
        assert!(
            start < middle && middle < on,
            "thumb animates on with hover/pages off: {start} -> {middle} -> {on}"
        );
        let callbacks = changes.get();
        if !history {
            assert_eq!(callbacks, 1);
        }

        external_off(&app, history, "motion-off");
        let start = knob_x(&app, history);
        advance(&app, history, 45);
        let middle = knob_x(&app, history);
        advance(&app, history, 160);
        let end = knob_x(&app, history);
        assert!(
            start > middle && middle > end,
            "external update animates off: {start} -> {middle} -> {end}"
        );
        assert!((end - off).abs() < 0.1);
        assert_eq!(
            changes.get(),
            callbacks,
            "external updates cannot invoke toggle callbacks"
        );

        if history {
            app.set_tracking_busy(true);
            click_control(&app, history);
            assert_eq!(
                control(&app, history).accessible_checked(),
                Some(false),
                "disabled history control rejects input"
            );
            app.set_tracking_busy(false);
        }
        anim.set_enabled(false);
        click_control(&app, history);
        advance(&app, history, 1);
        assert!(
            (knob_x(&app, history) - on).abs() < 0.1,
            "master-off snaps on"
        );
        external_off(&app, history, "motion-disabled-off");
        advance(&app, history, 1);
        assert!(
            (knob_x(&app, history) - off).abs() < 0.1,
            "master-off snaps off"
        );

        // Disabling the master must also settle an already running animation.
        anim.set_enabled(true);
        advance(&app, history, 200);
        click_control(&app, history);
        let _ = knob_x(&app, history);
        advance(&app, history, 35);
        let middle = knob_x(&app, history);
        assert!(middle > off && middle < on);
        anim.set_enabled(false);
        advance(&app, history, 16);
        assert!(
            (knob_x(&app, history) - on).abs() < 0.1,
            "master-off settles an in-flight thumb by the next frame"
        );
        anim.set_enabled(true);
        advance(&app, history, 1);
        assert!(
            (knob_x(&app, history) - on).abs() < 0.1,
            "quick re-enable cannot revive stale intermediate state"
        );
    }
}

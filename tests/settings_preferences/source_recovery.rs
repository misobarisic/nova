//! Recovery is opt-in; uncertain source timelines reuse the accessible resume UI.
use crate::preference_support::{key, reveal, settle, toggle};
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, platform::Key};
use std::{cell::RefCell, rc::Rc};

#[test]
fn recovery_setting_routes_pointer_and_keyboard_and_timeline_prompt_keeps_both_choices() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::recovery_setting_routes_pointer_and_keyboard_and_timeline_prompt_keeps_both_choices"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(5);
    app.set_settings_detail_open(true);
    let fields = Rc::new(RefCell::new(Vec::new()));
    let recorded = fields.clone();
    app.on_settings_edited(move |field| recorded.borrow_mut().push(field.to_string()));
    app.show().unwrap();
    for (width, height, android) in [(390, 844, true), (1280, 800, false), (900, 360, true)] {
        app.set_is_android(android);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.set_auto_retry_streams(false);
        let control = reveal(
            &app,
            || toggle(&app, "Automatically try another source"),
            "Automatically try another source",
        );
        assert_eq!(control.accessible_checked(), Some(false));
        control.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(app.get_auto_retry_streams());
        assert_eq!(
            fields.borrow().last().map(String::as_str),
            Some("auto_retry_streams")
        );
        key(&app, Key::LeftArrow);
        assert!(!app.get_auto_retry_streams());
        key(&app, Key::RightArrow);
        assert!(app.get_auto_retry_streams());
    }
    app.set_show_settings(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(true);
    app.set_resume_source_changed(true);
    app.set_resume_prompt_visible(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Start over")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Resume")
            .next()
            .is_some()
    );
    let choices = Rc::new(RefCell::new(Vec::new()));
    let recorded = choices.clone();
    app.on_resume_choice(move |resume| recorded.borrow_mut().push(resume));
    ElementHandle::find_by_accessible_label(&app, "Start over")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    ElementHandle::find_by_accessible_label(&app, "Resume")
        .next()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(&*choices.borrow(), &[false, true]);
    app.set_resume_source_can_resume(false);
    settle(&app);
    let resume = ElementHandle::find_by_accessible_label(&app, "Resume")
        .next()
        .unwrap();
    assert_eq!(resume.accessible_enabled(), Some(false));
    resume.mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        &*choices.borrow(),
        &[false, true],
        "an out-of-range resume cannot dispatch"
    );
}

fn prompt_frames(app: &nova::AppWindow, millis: u64) {
    for _ in 0..millis {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
        for id in ["ResumePrompt::panel", "ResumePrompt::scrim"] {
            for item in ElementHandle::find_by_element_id(app, id) {
                let _ = (
                    item.size(),
                    item.absolute_position(),
                    item.computed_opacity(),
                );
            }
        }
    }
}

fn prompt_key(app: &nova::AppWindow, key: Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
}

fn prompt_panel(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_id(app, "ResumePrompt::panel").next()
}

#[test]
fn recovery_prompt_retains_animation_snapshot_and_blocks_input_until_exit_finishes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::recovery_prompt_retains_animation_snapshot_and_blocks_input_until_exit_finishes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_is_paused(true);
    app.set_player_title("The current source".into());
    let choices = Rc::new(RefCell::new(Vec::new()));
    let recorded = choices.clone();
    let weak = app.as_weak();
    app.on_resume_choice(move |resume| {
        recorded.borrow_mut().push(resume);
        let app = weak.upgrade().unwrap();
        app.set_resume_prompt_visible(false);
        // Production clears the recovery facts in the same callback. Exit
        // pixels must keep the exact question that received the decision.
        app.set_resume_source_changed(false);
    });
    let cancels = Rc::new(std::cell::Cell::new(0));
    let recorded = cancels.clone();
    let weak = app.as_weak();
    app.on_resume_cancel(move || {
        recorded.set(recorded.get() + 1);
        weak.upgrade().unwrap().set_resume_prompt_visible(false);
    });
    app.show().unwrap();
    for (width, height, tv) in [(390, 844, false), (900, 360, false), (1920, 1080, true)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.global::<nova::InterfaceStyle>().set_tv(tv);
        let anim = app.global::<nova::Anim>();
        anim.set_enabled(true);
        anim.set_transitions(true);
        app.set_resume_source_changed(true);
        app.set_resume_source_position(90.);
        app.set_resume_prompt_visible(true);
        prompt_frames(&app, 65);
        let panel = prompt_panel(&app).unwrap();
        let entering_y = panel.absolute_position().y;
        let entering_alpha = panel.computed_opacity();
        assert!(entering_alpha > 0. && entering_alpha < 1.);
        prompt_frames(&app, 260);
        let shown_y = panel.absolute_position().y;
        assert!(entering_y > shown_y + 1., "the card rises while entering");
        assert!(panel.computed_opacity() > 0.99);
        app.set_player_title(format!("Updated display label at {width}px").into());
        prompt_frames(&app, 1);
        assert!(
            app.get_resume_prompt_visible(),
            "display enrichment preserves request ownership"
        );
        let resume = ElementHandle::find_by_accessible_label(&app, "Resume")
            .next()
            .unwrap();
        let previous_choices = choices.borrow().len();
        resume.mock_single_click(slint::platform::PointerEventButton::Left);
        prompt_frames(&app, 60);
        assert!(!app.get_resume_prompt_visible());
        assert_eq!(choices.borrow().len(), previous_choices + 1);
        assert_eq!(resume.accessible_enabled(), Some(false));
        assert!(panel.absolute_position().y > shown_y + 1.);
        assert!(panel.computed_opacity() > 0. && panel.computed_opacity() < 1.);
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Check resume position")
                .next()
                .is_some(),
            "the outgoing snapshot keeps the source question after its facts clear"
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Resume playback?")
                .next()
                .is_none()
        );
        let before_cancels = cancels.get();
        resume.mock_single_click(slint::platform::PointerEventButton::Left);
        prompt_key(&app, Key::Back);
        prompt_key(&app, Key::Return);
        app.set_system_back_request(app.get_system_back_request() + 1);
        prompt_frames(&app, 1);
        assert_eq!(choices.borrow().len(), previous_choices + 1);
        assert_eq!(cancels.get(), before_cancels);
        assert!(
            app.get_player_open(),
            "Back cannot reach the covered player"
        );
        let reversing_y = panel.absolute_position().y;
        app.set_resume_source_changed(true);
        app.set_resume_prompt_visible(true);
        assert!((panel.absolute_position().y - reversing_y).abs() < 0.5);
        prompt_frames(&app, 300);
        assert!(app.get_resume_prompt_visible());
        assert!(panel.computed_opacity() > 0.99);
        app.set_resume_prompt_visible(false);
        prompt_frames(&app, 300);
        assert!(prompt_panel(&app).is_none());
        let focused = i_slint_core::window::WindowInner::from_pub(app.window())
            .focus_item
            .borrow()
            .upgrade();
        assert!(
            focused.is_some_and(|item| item.is_visible()),
            "exit restores visible native focus"
        );

        for master_off in [false, true] {
            app.set_resume_prompt_visible(true);
            prompt_frames(&app, 45);
            if master_off {
                anim.set_enabled(false);
            } else {
                anim.set_transitions(false);
            }
            prompt_frames(&app, 1);
            let settled = prompt_panel(&app).unwrap();
            assert!((settled.absolute_position().y - shown_y).abs() < 0.5);
            assert!(settled.computed_opacity() > 0.99);
            anim.set_enabled(true);
            anim.set_transitions(true);
            prompt_frames(&app, 300);
            app.set_resume_prompt_visible(false);
            prompt_frames(&app, 45);
            if master_off {
                anim.set_enabled(false);
            } else {
                anim.set_transitions(false);
            }
            prompt_frames(&app, 1);
            assert!(
                prompt_panel(&app).is_none(),
                "disabled motion ends retention"
            );
            anim.set_enabled(true);
            anim.set_transitions(true);
            prompt_frames(&app, 300);
            assert!(
                prompt_panel(&app).is_none(),
                "old motion cannot revive the prompt"
            );
        }
        app.set_resume_prompt_visible(true);
        prompt_frames(&app, 65);
        app.set_profiles_open(true);
        prompt_frames(&app, 1);
        assert!(!app.get_resume_prompt_visible());
        assert!(
            prompt_panel(&app).is_none(),
            "profile changes discard stale paint"
        );
        app.set_profiles_open(false);
        prompt_frames(&app, 300);
        assert!(prompt_panel(&app).is_none());
        app.set_resume_prompt_visible(true);
        prompt_frames(&app, 65);
        app.set_resume_prompt_visible(false);
        prompt_frames(&app, 40);
        app.set_player_title(format!("Replacement source at {width}px").into());
        prompt_frames(&app, 1);
        assert!(
            prompt_panel(&app).is_none(),
            "new owners discard outgoing paint"
        );
    }
}

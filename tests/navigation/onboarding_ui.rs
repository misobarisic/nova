//! First-run navigation owns input and keeps its footer reachable on phones.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn native_focus(app: &nova::AppWindow) -> String {
    let focused = i_slint_core::window::WindowInner::from_pub(app.window())
        .focus_item
        .borrow()
        .upgrade();
    focused.map_or_else(
        || "none".to_string(),
        |item| {
            format!(
                "{:?} visible={} geometry={:?}",
                item.element_type_names_and_ids(0),
                item.is_visible(),
                item.geometry()
            )
        },
    )
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..20 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn assert_language_checkmark(app: &nova::AppWindow) {
    let label = if app.get_language_index() == 0 {
        "English"
    } else {
        "Hrvatski"
    };
    let button = (0..10)
        .find_map(|_| {
            let button = ElementHandle::find_by_accessible_label(app, label)
                .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
                .last();
            if button.is_none() {
                // The short landscape body scrolls below the heading and intro.
                let size = app.window().size();
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(
                            size.width as f32 / 2.0,
                            size.height as f32 / 2.0,
                        ),
                        delta_x: 0.0,
                        delta_y: -80.0,
                    });
                settle(app);
            }
            button
        })
        .expect("selected language control must be reachable by scrolling");
    let check = ElementHandle::find_by_element_type_name(app, "IcCheck")
        .next()
        .unwrap();
    let choice = button.absolute_position();
    let mark = check.absolute_position();
    assert!(
        (mark.y + check.size().height / 2.0 - choice.y - button.size().height / 2.0).abs() < 0.5,
        "checkmark must be vertically centered with the language control"
    );
    assert!(
        mark.x >= choice.x + button.size().width + 8.0,
        "checkmark must be separated from the language control at the right"
    );
    assert!(
        mark.x + check.size().width <= app.window().size().width as f32,
        "checkmark must stay within the viewport"
    );
}
fn click(app: &nova::AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(app, label)
        .last()
        .unwrap()
        .mock_single_click(slint::platform::PointerEventButton::Left);
    settle(app);
}
fn scroll_body(app: &nova::AppWindow, delta_y: f32) {
    let size = app.window().size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(
                size.width as f32 / 2.0,
                size.height as f32 / 2.0,
            ),
            delta_x: 0.0,
            delta_y,
        });
    settle(app);
}
fn reveal_label(app: &nova::AppWindow, label: &str) -> ElementHandle {
    scroll_body(app, 2000.0);
    (0..40)
        .find_map(|_| {
            let element = ElementHandle::find_by_accessible_label(app, label).find(|element| {
                let pos = element.absolute_position();
                pos.y >= 24.0
                    && pos.y + element.size().height < app.window().size().height as f32 - 100.0
            });
            if element.is_none() {
                scroll_body(app, -40.0);
            }
            element
        })
        .unwrap_or_else(|| panic!("reachable onboarding control: {label}"))
}
fn back(app: &nova::AppWindow) {
    let text: slint::SharedString = slint::platform::Key::Back.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}
fn assert_wrapped_header(app: &nova::AppWindow, heading: &str, introduction: &str) {
    let text = |label: &str| {
        ElementHandle::find_by_element_type_name(app, "Text")
            .find(|element| element.accessible_label().as_deref() == Some(label))
            .unwrap_or_else(|| panic!("visible onboarding text: {label}"))
    };
    let heading_element = text(heading);
    let introduction_element = text(introduction);
    let heading_position = heading_element.absolute_position();
    let introduction_position = introduction_element.absolute_position();
    assert!(heading_element.size().height > 0.0);
    assert!(introduction_element.size().height > 0.0);
    assert!(
        heading_position.y + heading_element.size().height <= introduction_position.y + 0.5,
        "heading and introduction bounds overlap: {heading}"
    );
    for element in [heading_element, introduction_element] {
        let position = element.absolute_position();
        assert!(
            position.x >= 0.0 && position.x + element.size().width <= 320.5,
            "wrapped text must stay inside the phone width"
        );
    }
}

#[test]
fn skip_language_back_and_finish_preserve_selection_and_footer() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "skip_language_back_and_finish_preserve_selection_and_footer"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_touch_menus(true);
    app.set_onboarding_visible(true);
    app.window().set_size(slint::PhysicalSize::new(320, 568));
    app.window().show().unwrap();
    let installs = Rc::new(Cell::new(0));
    let recorded = installs.clone();
    app.on_onboarding_addon_added(move |_| recorded.set(recorded.get() + 1));
    let finishes = Rc::new(Cell::new(0));
    let recorded = finishes.clone();
    app.on_onboarding_finished(move || recorded.set(recorded.get() + 1));
    let backgrounds = Rc::new(Cell::new(0));
    let recorded = backgrounds.clone();
    app.on_exit_to_background(move || recorded.set(recorded.get() + 1));
    let navigation = Rc::new(Cell::new(0));
    let recorded = navigation.clone();
    app.on_library_picked(move || recorded.set(recorded.get() + 1));

    // A factory product must own native keyboard navigation before a pointer
    // press can rescue its focus. Keep this in the existing backend instance.
    let initial_language = app.get_language_index();
    app.set_onboarding_step(1);
    settle(&app);
    let checkmarks: Vec<_> = ElementHandle::find_by_element_type_name(&app, "IcCheck").collect();
    assert_eq!(
        checkmarks.len(),
        1,
        "the selected language uses a vector checkmark"
    );
    assert_eq!(checkmarks[0].size(), slint::LogicalSize::new(20.0, 20.0));
    assert!(
        ElementHandle::find_by_accessible_label(&app, "✓")
            .next()
            .is_none()
    );
    for (width, height) in [(320, 568), (900, 360), (1280, 800)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        settle(&app);
        assert_language_checkmark(&app);
    }
    app.window().set_size(slint::PhysicalSize::new(320, 568));
    settle(&app);
    let focus_before_arrow = native_focus(&app);
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::RightArrow.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::RightArrow.into(),
        },
    ] {
        let pressed = matches!(&event, slint::platform::WindowEvent::KeyPressed { .. });
        let result = app.window().dispatch_event_with_result(event).unwrap();
        // The wizard handles the press; the release has no action.
        if pressed {
            assert!(matches!(
                result,
                slint::platform::WindowEventDispatchResult::Accepted
            ));
        }
    }
    assert_eq!(
        app.get_language_index(),
        1 - initial_language,
        "first wizard arrow: focus before={focus_before_arrow}; after={}",
        native_focus(&app)
    );
    settle(&app);
    assert_language_checkmark(&app);
    let request_before = app.get_system_back_request();
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
            app.window().dispatch_event_with_result(event).unwrap(),
            slint::platform::WindowEventDispatchResult::Accepted
        ));
    }
    settle(&app);
    assert_eq!(
        app.get_onboarding_step(),
        0,
        "held Back pops one wizard step"
    );
    assert_eq!(app.get_system_back_request(), request_before + 1);
    assert_eq!(
        backgrounds.get(),
        0,
        "held Back must not reach Home at step zero"
    );
    assert_eq!(navigation.get(), 0);
    assert_eq!(installs.get(), 0);
    assert_eq!(finishes.get(), 0);
    app.set_language_index(initial_language);
    app.set_kb_active(false);
    app.global::<nova::SettingsStyle>().set_keyboard_mode(false);
    settle(&app);

    // Clicking the real underlying navigation must hit the overlay instead.
    let library = ElementHandle::find_by_accessible_label(&app, "Library")
        .next()
        .expect("the underlying Home navigation is mounted");
    library.mock_single_click(slint::platform::PointerEventButton::Left);
    settle(&app);
    assert_eq!(navigation.get(), 0);
    assert_eq!(app.get_onboarding_step(), 0);
    click(&app, "Skip for now");
    assert_eq!(installs.get(), 0);
    assert_eq!(app.get_onboarding_step(), 1);
    click(&app, "Hrvatski");
    assert_eq!(app.get_language_index(), 1);
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert_eq!(
        backgrounds.get(),
        0,
        "Home must not consume onboarding's Back broadcast"
    );
    assert_eq!(app.get_onboarding_step(), 0);
    assert!(!app.get_kb_active());
    click(&app, "Skip for now");
    assert_eq!(app.get_language_index(), 1);
    click(&app, "Continue");
    assert_eq!(app.get_onboarding_step(), 2);
    back(&app);
    assert_eq!(app.get_onboarding_step(), 1);
    click(&app, "Continue");
    for (width, height) in [(320, 568), (568, 320), (1280, 800)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        settle(&app);
        let footer = ElementHandle::find_by_accessible_label(&app, "Start using Nova")
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .last()
            .unwrap();
        let position = footer.absolute_position();
        let size = footer.size();
        assert!(position.x >= 0.0 && position.x + size.width <= width as f32);
        assert!(position.y >= 0.0 && position.y + size.height <= height as f32);
        assert!(size.height >= 44.0);
        // Preference lists may exceed one viewport. Each picker must remain
        // reachable independently while the footer stays fixed.
        for id in [
            "Onboarding::audio_language_picker",
            "Onboarding::subtitle_language_picker",
        ] {
            scroll_body(&app, 2000.0);
            let picker = (0..30)
                .find_map(|_| {
                    let picker = ElementHandle::find_by_element_id(&app, id).next();
                    if picker.is_none() {
                        scroll_body(&app, -60.0);
                    }
                    picker
                })
                .unwrap_or_else(|| panic!("{id} reachable at {width}x{height}"));
            let position = picker.absolute_position();
            assert!(position.x >= 0.0 && position.x + picker.size().width <= width as f32);
        }
    }
    click(&app, "Start using Nova");
    assert_eq!(finishes.get(), 1);
    assert_eq!(installs.get(), 0);
    assert_eq!(app.get_language_index(), 1);
    app.window().set_size(slint::PhysicalSize::new(320, 568));
    // Query only the visible heading/intro after returning the body to its
    // beginning. Lower controls may legitimately require vertical scrolling.
    for (locale, headings, introductions) in [
        (
            "en",
            ["Connect your first add-on", "Make playback yours"],
            [
                "Add an add-on to bring catalogs and streams into Nova.",
                "Choose defaults. You can fine-tune them later.",
            ],
        ),
        (
            "hr",
            ["Povežite svoj prvi dodatak", "Prilagodite reprodukciju"],
            [
                "Dodajte dodatak za kataloge i streamove u Novi.",
                "Odaberite zadane postavke. Možete ih promijeniti kasnije.",
            ],
        ),
    ] {
        let catalogs_available = slint::select_bundled_translation(locale).is_ok();
        if locale == "hr" && !catalogs_available {
            continue;
        }
        for (index, step) in [0, 2].into_iter().enumerate() {
            app.set_onboarding_step(step);
            settle(&app);
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(160.0, 220.0),
                    delta_x: 0.0,
                    delta_y: 2000.0,
                });
            settle(&app);
            assert_wrapped_header(&app, headings[index], introductions[index]);
        }
    }
    let _ = slint::select_bundled_translation("en");
}

#[test]
fn ordered_playback_languages_share_settings_edits_and_exclusive_modes() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "ordered_playback_languages_share_settings_edits_and_exclusive_modes"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_touch_menus(true);
    app.set_onboarding_visible(true);
    app.set_onboarding_step(2);
    app.window().set_size(slint::PhysicalSize::new(320, 568));
    app.window().show().unwrap();
    let strings = |values: &[&str]| {
        Rc::new(VecModel::from(
            values
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<slint::SharedString>>(),
        ))
        .into()
    };
    app.set_audio_language_codes(strings(&["", "en", "ja"]));
    app.set_audio_language_names(strings(&["Automatic", "English (en)", "Japanese (ja)"]));
    app.set_subtitle_language_codes(strings(&["", "off", "hr", "en"]));
    app.set_subtitle_language_names(strings(&[
        "Automatic",
        "Off",
        "Croatian (hr)",
        "English (en)",
    ]));
    app.set_audio_language_label("Add audio".into());
    app.set_subtitle_language_label("Add subtitles".into());
    app.set_audio_language_selected_codes(strings(&["en", "ja"]));
    app.set_audio_language_selected_names(strings(&["English (en)", "Japanese (ja)"]));
    app.set_subtitle_language_selected_codes(strings(&["hr", "en"]));
    app.set_subtitle_language_selected_names(strings(&["Croatian (hr)", "English (en)"]));
    let edits = Rc::new(RefCell::new(Vec::new()));
    let recorded = edits.clone();
    app.on_playback_language_edited(move |code, action, subtitle| {
        recorded
            .borrow_mut()
            .push((code.to_string(), action, subtitle))
    });
    let picked = Rc::new(RefCell::new(Vec::new()));
    let recorded = picked.clone();
    app.on_playback_language_picked(move |code, subtitle| {
        recorded.borrow_mut().push((code.to_string(), subtitle))
    });
    let saved = Rc::new(RefCell::new(Vec::new()));
    let recorded = saved.clone();
    app.on_settings_edited(move |key| recorded.borrow_mut().push(key.to_string()));
    settle(&app);
    for (label, expected) in [
        ("Move down: English (en)", ("en", 3, false)),
        ("Move up: Japanese (ja)", ("ja", 2, false)),
        ("Remove: Japanese (ja)", ("ja", 1, false)),
        ("Move down: Croatian (hr)", ("hr", 3, true)),
        ("Remove: Croatian (hr)", ("hr", 1, true)),
    ] {
        reveal_label(&app, label).mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(
            edits.borrow().last(),
            Some(&(expected.0.to_string(), expected.1, expected.2))
        );
        assert_eq!(
            saved.borrow().last().map(String::as_str),
            Some(if expected.2 {
                "default_subtitle_language"
            } else {
                "default_audio_language"
            })
        );
    }
    for (picker_label, choice, expected, subtitle) in [
        ("Add audio", "Japanese (ja)", "ja", false),
        ("Add audio", "Automatic", "", false),
        ("Add subtitles", "English (en)", "en", true),
        ("Add subtitles", "Off", "off", true),
        ("Add subtitles", "Automatic", "", true),
    ] {
        reveal_label(&app, picker_label)
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        click(&app, choice);
        if expected.is_empty() || expected == "off" {
            assert_eq!(
                picked.borrow().last(),
                Some(&(expected.to_string(), subtitle))
            );
        } else {
            assert_eq!(
                edits.borrow().last(),
                Some(&(expected.to_string(), 0, subtitle))
            );
        }
        // Backend publication restores the add label after each selection.
        app.set_audio_language_choice_index(-1);
        app.set_subtitle_language_choice_index(-1);
        settle(&app);
    }
    let footer = ElementHandle::find_by_accessible_label(&app, "Start using Nova")
        .last()
        .unwrap();
    assert!(footer.size().height >= 44.0);
}

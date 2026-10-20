//! The universal rules panel keeps phone controls reachable and keyboard edits
//! use the same typed bridge callbacks as touch choices.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::{ComponentHandle, platform::WindowEvent};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn settle(app: &nova::AppWindow) {
    for _ in 0..8 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        slint::platform::update_timers_and_animations();
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn find_button(app: &nova::AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
}
fn button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    find_button(app, label).unwrap_or_else(|| panic!("missing filter control {label}"))
}
fn form(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "StreamFilterPanel::body_scroll")
        .next()
        .expect("mounted filter form")
}
fn scroll_by(app: &nova::AppWindow, delta_y: f32) {
    let scroll = form(app);
    let position = scroll.absolute_position();
    let size = scroll.size();
    app.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(
            position.x + size.width / 2.0,
            position.y + size.height / 2.0,
        ),
        delta_x: 0.0,
        delta_y,
    });
}
fn in_form(app: &nova::AppWindow, control: &ElementHandle) -> bool {
    let scroll = form(app);
    let viewport = scroll.absolute_position();
    let bounds = scroll.size();
    let pos = control.absolute_position();
    let size = control.size();
    pos.x >= viewport.x - 0.5
        && pos.x + size.width <= viewport.x + bounds.width + 0.5
        && pos.y >= viewport.y - 0.5
        && pos.y + size.height <= viewport.y + bounds.height + 0.5
}
fn reveal_button(app: &nova::AppWindow, label: &str) -> ElementHandle {
    // Accessibility queries omit fully clipped delegates. Begin at the top
    // and scan the bounded form rather than requiring the target to exist
    // before its viewport has reached it.
    scroll_by(app, 3000.0);
    settle(app);
    for _ in 0..40 {
        if let Some(control) = find_button(app, label)
            && in_form(app, &control)
        {
            return control;
        }
        scroll_by(app, -50.0);
        settle(app);
    }
    panic!("filter control must be reachable by scrolling: {label}");
}
fn language_input(app: &nova::AppWindow) -> ElementHandle {
    ElementHandle::find_by_element_id(app, "StreamFilterPanel::language")
        .next()
        .expect("mounted audio language editor")
        .query_descendants()
        .match_predicate(|item| item.accessible_role() == Some(AccessibleRole::TextInput))
        .find_first()
        .expect("audio language text input")
}
fn type_text(app: &nova::AppWindow, text: &str) {
    for character in text.chars() {
        let text: slint::SharedString = character.to_string().into();
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    }
    settle(app);
}

fn fixture(app: &nova::AppWindow) {
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_selected_title("The Quiet Sea".into());
    app.set_selected_year("2026".into());
    app.set_stream_total(1);
    app.set_streams(
        Rc::new(slint::VecModel::from(vec![nova::StreamRow {
            id: "release".into(),
            title: "The.Quiet.Sea.1080p.WEB-DL.HEVC".into(),
            metadata: "Aster · 1080p".into(),
            description: "Audio: Japanese / English".into(),
            size: "4.5 GiB".into(),
            ..Default::default()
        }]))
        .into(),
    );
    let sync = app.global::<nova_ui_shared::SettingsSync>();
    sync.set_enabled(true);
    sync.set_rows(
        Rc::new(slint::VecModel::from(vec![
            nova_ui_shared::SettingSyncInfo {
                key: "stream_filters".into(),
                status: 1,
                can_override: true,
                has_shared: true,
                shared_supported: true,
                local_value: "1080p · 8 GiB · HEVC · ja, en · retain unknown".into(),
                shared_value: "1080p · 8 GiB · HEVC · ja, en · retain unknown".into(),
            },
        ]))
        .into(),
    );
    sync.on_lookup(|_| 0);
    let filters = app.global::<nova::StreamFiltersBridge>();
    filters.set_max_resolution_index(3);
    filters.set_max_resolution_label("1080p".into());
    filters.set_max_size_index(4);
    filters.set_max_size_label("8 GiB".into());
    filters.set_allow_hevc(true);
    filters.set_audio_languages("ja, en".into());
    filters.set_retain_unknown(true);
}

#[test]
fn universal_filters_fit_320_and_390_phones_and_desktop_with_keyboard_and_touch() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::universal_filters_fit_320_and_390_phones_and_desktop_with_keyboard_and_touch"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    let edits = Rc::new(RefCell::new(Vec::<(i32, String)>::new()));
    let recorded = edits.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_edited(move |field, value| {
            recorded.borrow_mut().push((field, value.to_string()));
            let app = weak.upgrade().unwrap();
            let filters = app.global::<nova::StreamFiltersBridge>();
            match field {
                0 => filters.set_enabled(value == "true"),
                7 => filters.set_audio_languages(value),
                8 => filters.set_require_audio(value == "true"),
                9 => filters.set_retain_unknown(value == "true"),
                _ => {}
            }
        });
    let resets = Rc::new(Cell::new(0));
    let recorded = resets.clone();
    app.global::<nova::StreamFiltersBridge>()
        .on_reset(move || recorded.set(recorded.get() + 1));
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_closed(move || {
            weak.upgrade()
                .unwrap()
                .global::<nova::StreamFiltersBridge>()
                .set_open(false)
        });
    app.show().unwrap();
    settle(&app);
    for (width, height) in [(320, 740), (390, 844), (1280, 900)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });
        app.set_is_android(width < 700);
        app.global::<nova::StreamFiltersBridge>()
            .set_audio_languages("ja, en".into());
        app.global::<nova::StreamFiltersBridge>()
            .set_require_audio(false);
        app.global::<nova::StreamFiltersBridge>()
            .set_retain_unknown(true);
        app.global::<nova::StreamFiltersBridge>().set_open(true);
        settle(&app);
        let card = ElementHandle::find_by_element_id(&app, "StreamFilterPanel::card")
            .next()
            .unwrap();
        let pos = card.absolute_position();
        let size = card.size();
        assert!(
            pos.x >= -0.5
                && pos.y >= -0.5
                && pos.x + size.width <= width as f32 + 0.5
                && pos.y + size.height <= height as f32 + 0.5,
            "{width}×{height}: panel overflows: {pos:?} {size:?}"
        );
        let toggle = ElementHandle::find_by_accessible_label(&app, "Use stream rules")
            .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
            .unwrap();
        toggle.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap().0, 0);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(1, "4".into()));
        // Keyboard navigation reveals the lower form at genuine compact
        // widths; typing and Apply exercise the actual editor and callbacks.
        for _ in 0..7 {
            key(&app, slint::platform::Key::DownArrow);
        }
        key(&app, slint::platform::Key::Return);
        let input = language_input(&app);
        assert!(
            in_form(&app, &input),
            "audio editor must be fully revealed at {width}px"
        );
        type_text(&app, ", hr");
        assert_eq!(input.accessible_value().as_deref(), Some("ja, en, hr"));
        let apply = reveal_button(&app, "Apply languages");
        assert!(apply.size().height >= 44.0);
        apply.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap(), &(7, "ja, en, hr".into()));
        // Enter from the field returns keyboard ownership to the panel.
        language_input(&app).mock_single_click(slint::platform::PointerEventButton::Left);
        key(&app, slint::platform::Key::Return);
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(8, "true".into()));
        assert!(
            app.global::<nova::StreamFiltersBridge>()
                .get_require_audio()
        );
        key(&app, slint::platform::Key::DownArrow);
        key(&app, slint::platform::Key::Return);
        assert_eq!(edits.borrow().last().unwrap(), &(9, "false".into()));
        assert!(
            !app.global::<nova::StreamFiltersBridge>()
                .get_retain_unknown()
        );
        // Both bottom policies are also reachable by a touch scroll/tap.
        reveal_button(&app, "Keep streams with unknown details")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap(), &(9, "true".into()));
        reveal_button(&app, "Require one of these audio languages")
            .mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(edits.borrow().last().unwrap(), &(8, "false".into()));
        // Reset and Done stay outside the scrolling form at every viewport.
        for label in ["Reset rules", "Done"] {
            let control = ElementHandle::find_by_accessible_label(&app, label)
                .find(|item| item.accessible_role() == Some(AccessibleRole::Button))
                .unwrap();
            let pos = control.absolute_position();
            let size = control.size();
            assert!(pos.x >= 0.0 && pos.x + size.width <= width as f32 + 0.5);
            assert!(pos.y >= 0.0 && pos.y + size.height <= height as f32 + 0.5);
        }
        let previous_resets = resets.get();
        button(&app, "Reset rules").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(resets.get(), previous_resets + 1);
        button(&app, "Done").mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert!(!app.global::<nova::StreamFiltersBridge>().get_open());
        app.global::<nova::StreamFiltersBridge>().set_open(true);
        settle(&app);
        key(&app, slint::platform::Key::Escape);
        assert!(!app.global::<nova::StreamFiltersBridge>().get_open());
    }
}

#[test]
fn short_selector_shares_filter_action_with_heading_and_keeps_sources_usable() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::short_selector_shares_filter_action_with_heading_and_keeps_sources_usable"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    app.set_remote_has_ready_devices(true);
    app.set_stream_addons(
        Rc::new(slint::VecModel::from(vec![slint::SharedString::from(
            "Addon",
        )]))
        .into(),
    );
    app.set_streams_hint("This stream cannot be played here.".into());
    app.set_stream_page_count(2);
    let opened = Rc::new(Cell::new(0));
    let recorded = opened.clone();
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_opened(move || {
            recorded.set(recorded.get() + 1);
            weak.upgrade()
                .unwrap()
                .global::<nova::StreamFiltersBridge>()
                .set_open(true);
        });
    app.show().unwrap();
    settle(&app);
    for (width, height) in [(320, 360), (390, 360), (900, 360)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });
        settle(&app);
        let results = ElementHandle::find_by_element_id(&app, "StreamSelector::result_scroll")
            .next()
            .unwrap();
        assert!(
            results.size().height >= 80.0,
            "{width}x{height}: sources need a usable viewport: {:?}",
            results.size()
        );
        let heading = ElementHandle::find_by_element_id(&app, "StreamSelector::stream_heading")
            .next()
            .unwrap();
        let action = button(&app, "Universal filters");
        assert_eq!(action.size(), slint::LogicalSize::new(44.0, 44.0));
        assert!(action.absolute_position().y >= heading.absolute_position().y - 0.5);
        assert!(
            action.absolute_position().y + action.size().height
                <= heading.absolute_position().y + heading.size().height + 0.5
        );
        let output = ElementHandle::find_by_element_id(&app, "RemoteTargetMenu::output_button")
            .next()
            .expect("paired output launcher shares the compact heading");
        let heading_origin = heading.absolute_position();
        let heading_size = heading.size();
        assert!(
            heading_origin.x >= 0.0 && heading_origin.x + heading_size.width <= width as f32 + 0.5
        );
        assert!(
            heading_origin.y >= 0.0
                && heading_origin.y + heading_size.height <= height as f32 + 0.5
        );
        for control in [&action, &output] {
            let pos = control.absolute_position();
            let size = control.size();
            assert!(
                pos.x >= heading_origin.x - 0.5
                    && pos.x + size.width <= heading_origin.x + heading_size.width + 0.5,
                "both filter and Play on controls stay inside the {width}px heading"
            );
            assert!(
                pos.y >= heading_origin.y - 0.5
                    && pos.y + size.height <= heading_origin.y + heading_size.height + 0.5
            );
        }
        assert!(
            output.absolute_position().x
                >= action.absolute_position().x + action.size().width + 7.5,
            "filter and Play on controls retain their 8px separation"
        );
        assert!(
            output.size().height <= 32.0,
            "Play on retains its compact button"
        );
        let title = ElementHandle::find_by_element_id(&app, "StreamSelector::heading_title")
            .next()
            .unwrap();
        assert!(title.absolute_position().x >= heading_origin.x - 0.5);
        assert!(
            title.absolute_position().x + title.size().width <= action.absolute_position().x - 0.5,
            "heading text elides within the available lane"
        );
        let before = opened.get();
        action.mock_single_click(slint::platform::PointerEventButton::Left);
        settle(&app);
        assert_eq!(opened.get(), before + 1);
        assert!(app.global::<nova::StreamFiltersBridge>().get_open());
        app.global::<nova::StreamFiltersBridge>().set_open(false);
        settle(&app);
    }
}

#[cfg(feature = "desktop")]
fn resize_native_preview(app: &nova::AppWindow, width: u32, height: u32) {
    use slint::winit_030::{WinitWindowAccessor, winit};
    app.set_is_android(width < 700);
    // A desktop WM otherwise clamps the app's normal 360px minimum. Only
    // this preview window drops that constraint, matching a 320px device.
    app.window()
        .with_winit_window(|window| {
            window.set_min_inner_size(None::<winit::dpi::PhysicalSize<u32>>);
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
        })
        .expect("native preview uses the winit backend");
    app.window().dispatch_event(WindowEvent::Resized {
        size: slint::LogicalSize::new(
            width as f32 / app.window().scale_factor(),
            height as f32 / app.window().scale_factor(),
        ),
    });
    app.window().request_redraw();
}

#[test]
#[cfg(feature = "desktop")]
#[ignore = "requires a display; optional native universal-filter design preview"]
fn render_universal_filter_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_universal_filter_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    fixture(&app);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    let index = Rc::new(Cell::new(0));
    let step = index.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(650),
        move || {
            let app = weak.upgrade().unwrap();
            let sizes = [
                (320, 740, "filters-phone-320", false),
                (320, 740, "filters-phone-320-bottom", true),
                (390, 844, "filters-phone-390", false),
                (390, 844, "filters-phone-390-bottom", true),
                (1280, 900, "filters-desktop", false),
            ];
            if step.get() == 0 {
                resize_native_preview(&app, sizes[0].0, sizes[0].1);
                app.global::<nova::StreamFiltersBridge>().set_open(true);
                step.set(1);
                return;
            }
            let current = step.get() - 1;
            let pixels = app.window().take_snapshot().unwrap();
            assert_eq!(
                (pixels.width(), pixels.height()),
                (sizes[current].0, sizes[current].1),
                "preview must use its named physical viewport"
            );
            if sizes[current].3 {
                assert!(
                    in_form(&app, &button(&app, "Keep streams with unknown details")),
                    "scrolled phone preview must reveal the bottom policy"
                );
            }
            if let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    std::path::PathBuf::from(dir).join(format!("{}.png", sizes[current].2)),
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            if current + 1 == sizes.len() {
                slint::quit_event_loop().unwrap();
            } else {
                let next = sizes[current + 1];
                resize_native_preview(&app, next.0, next.1);
                scroll_by(&app, if next.3 { -3000.0 } else { 3000.0 });
                step.set(step.get() + 1);
            }
        },
    );
    app.run().unwrap();
}

#[test]
fn initially_requested_filter_form_keeps_its_exit_tree_and_reinitializes_on_reopen() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::initially_requested_filter_form_keeps_its_exit_tree_and_reinitializes_on_reopen"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    fixture(&app);
    app.global::<nova::Anim>().set_enabled(true);
    app.global::<nova::Anim>().set_transitions(true);
    // Request the form before the lazy selector tree has ever been evaluated.
    app.global::<nova::StreamFiltersBridge>().set_open(true);
    let weak = app.as_weak();
    app.global::<nova::StreamFiltersBridge>()
        .on_closed(move || {
            weak.upgrade()
                .unwrap()
                .global::<nova::StreamFiltersBridge>()
                .set_open(false);
        });
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    settle(&app);
    let card = ElementHandle::find_by_element_id(&app, "StreamFilterPanel::card")
        .next()
        .expect("first requested form mounted");
    assert!(card.computed_opacity() > 0.99);
    app.global::<nova::StreamFiltersBridge>().set_open(false);
    // Evaluate the conditional tree now, before its retained exit elapses.
    let _ = ElementQuery::from_root(&app)
        .match_predicate(|_| true)
        .find_all();
    assert!(
        card.is_valid(),
        "closing must retain the form's exit animation"
    );
    settle(&app);
    settle(&app);
    assert!(
        card.is_valid(),
        "the first requested form stays mounted after exit"
    );
    app.global::<nova::StreamFiltersBridge>()
        .set_audio_languages("hr".into());
    app.global::<nova::StreamFiltersBridge>().set_open(true);
    settle(&app);
    settle(&app);
    assert!(card.is_valid(), "reopening reuses the existing form");
    assert_eq!(
        language_input(&app).accessible_value().as_deref(),
        Some("hr")
    );
    key(&app, slint::platform::Key::Escape);
    assert!(!app.global::<nova::StreamFiltersBridge>().get_open());
}

/// Measure actual compiled selector construction/layout/first rendering on a
/// native backend. Keep timing informational: shared CI machines cannot enforce
/// a frame budget reliably. Run the same fixture before/after UI changes.
#[test]
#[cfg(feature = "desktop")]
#[ignore = "requires a display; native stream selector opening timings"]
fn measure_native_stream_selector_opening() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::measure_native_stream_selector_opening"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    fixture(&app);
    app.set_stream_selector_open(false);
    app.set_streams(
        Rc::new(slint::VecModel::from(
            (0..25)
                .map(|index| nova::StreamRow {
                    id: format!("timing-{index}").into(),
                    title: format!("The.Quiet.Sea.2026.1080p.WEB-DL.HEVC.Release.{index}").into(),
                    metadata: "Example addon · 1080p · HEVC".into(),
                    description: "Audio: Japanese / English\nSubtitles: English / Croatian".into(),
                    size: "4.5 GiB".into(),
                    lines: 3,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_stream_total(25);
    app.set_stream_addons(
        Rc::new(slint::VecModel::from(vec![
            "Example addon".into(),
            "Second addon".into(),
        ]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    let mut phase = 0;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let app = weak.upgrade().unwrap();
            let round = phase / 2;
            if phase % 2 == 0 {
                let start = std::time::Instant::now();
                app.set_stream_selector_open(true);
                let image = app.window().take_snapshot().unwrap();
                let elapsed = start.elapsed();
                let tree = ElementQuery::from_root(&app)
                    .match_predicate(|_| true)
                    .find_all();
                // Accessibility traversal excludes invisible components, so
                // this count is diagnostic rather than proof of lazy mounting.
                println!(
                    "STREAM_SELECTOR_OPEN size={}x{} round={} elapsed_us={} visible_elements={}",
                    image.width(),
                    image.height(),
                    round % 8,
                    elapsed.as_micros(),
                    tree.len(),
                );
            } else {
                app.set_stream_selector_open(false);
                let _ = app.window().take_snapshot().unwrap();
                if round == 7 {
                    app.window().set_size(slint::PhysicalSize::new(1280, 800));
                } else if round == 15 {
                    slint::quit_event_loop().unwrap();
                }
            }
            phase += 1;
        },
    );
    slint::run_event_loop().unwrap();
}

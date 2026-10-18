//! Standalone About → Licenses (headless).
//!
//! The About page keeps the generated license catalog behind its own nested
//! page, and source-link buttons stay vertically centered in their rows on
//! narrow and wide layouts. One test function: the testing backend initializes
//! once per process.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn back(app: &nova::AppWindow) {
    for event in [
        slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        },
        slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        },
    ] {
        app.window().dispatch_event(event);
    }
}

fn settle(app: &nova::AppWindow) {
    // The mock backend has no paint pass to resolve virtual ListView height
    // estimates. Measure its exposed delegates before advancing queued work.
    for _ in 0..16 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
    }
}

#[test]
fn licenses_animate_open_close_reverse_and_respect_disabled_motion() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::licenses_animate_open_close_reverse_and_respect_disabled_motion"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_show_home(false);
    app.set_show_about(true);
    app.set_license_text_chunks(
        Rc::new(VecModel::from(vec![s(&"License notice.\n".repeat(60))])).into(),
    );
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    let pane = || {
        ElementHandle::find_by_element_id(&app, "AboutPage::about_scroll")
            .next()
            .unwrap()
    };
    let info = || {
        ElementHandle::find_by_element_id(&app, "AboutPage::about_info_scroll")
            .next()
            .unwrap()
    };
    let info_x = info().absolute_position().x;
    let button = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .unwrap();
    click(&app, &button);
    let start = pane().absolute_position().x;
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));
    let opening = pane().absolute_position().x;
    assert!(
        opening < start - 1.0 && opening > info_x + 1.0,
        "opening must visibly slide"
    );
    settle(&app);
    assert!((pane().absolute_position().x - info_x).abs() < 1.0);
    back(&app);
    let closing_start = pane().absolute_position().x;
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));
    let closing = pane().absolute_position().x;
    assert!(
        closing > closing_start + 1.0 && closing < start - 1.0,
        "closing must visibly slide"
    );
    // A quick reopen reverses the in-flight animation from its current value.
    app.set_about_licenses_open(true);
    let reverse_start = pane().absolute_position().x;
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));
    assert!(pane().absolute_position().x < reverse_start - 1.0);
    settle(&app);
    back(&app);
    settle(&app);
    assert!((info().absolute_position().x - info_x).abs() < 1.0);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
    for master_disabled in [false, true] {
        app.global::<nova::Anim>().set_enabled(!master_disabled);
        app.global::<nova::Anim>().set_transitions(master_disabled);
        app.set_about_licenses_open(true);
        assert!(
            (pane().absolute_position().x - info_x).abs() < 1.0,
            "disabled motion snaps open"
        );
        back(&app);
        assert!(
            (info().absolute_position().x - info_x).abs() < 1.0,
            "disabled motion snaps closed"
        );
    }
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let size = element.size();
    let position = LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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

fn drag_horizontal(app: &nova::AppWindow, from: LogicalPosition, to: LogicalPosition) {
    let _ = app
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
            position: from,
            button: slint::platform::PointerEventButton::Left,
        });
    for step in 1..=4 {
        let t = step as f32 / 4.0;
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
                    position: LogicalPosition::new(
                        from.x + (to.x - from.x) * t,
                        from.y + (to.y - from.y) * t,
                    ),
                });
    }
    let _ =
        app.window()
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerReleased {
                position: to,
                button: slint::platform::PointerEventButton::Left,
            });
}

fn check_alignment(app: &nova::AppWindow, label: &str, failures: &Rc<RefCell<Vec<String>>>) {
    let rows: Vec<_> = ElementHandle::find_by_element_id(app, "AboutPage::source_card").collect();
    let buttons: Vec<_> =
        ElementHandle::find_by_element_id(app, "AboutPage::source_view_button").collect();
    let short_viewport = app.window().size().height < 500;
    if (!short_viewport && (rows.len() != 2 || buttons.len() != 2))
        || (short_viewport && (rows.is_empty() || buttons.is_empty()))
    {
        failures.borrow_mut().push(format!(
            "{label}: expected two source rows/buttons, found {} rows and {} buttons",
            rows.len(),
            buttons.len()
        ));
        return;
    }

    let viewport_width = app.window().size().width as f32;
    for (index, row) in rows.iter().enumerate() {
        // A short scroll viewport can clip a neighboring source action.
        // Measure only a row whose action is actually exposed, and locate it
        // within that row rather than pairing two independently clipped lists.
        let Some(button) = row
            .query_descendants()
            .match_id("AboutPage::source_view_button")
            .find_first()
        else {
            continue;
        };
        let row_y = row.absolute_position().y;
        let row_x = row.absolute_position().x;
        let row_w = row.size().width;
        let row_h = row.size().height;
        let button_x = button.absolute_position().x;
        let button_y = button.absolute_position().y;
        let button_w = button.size().width;
        let button_h = button.size().height;
        let row_center = row_y + row_h / 2.0;
        let button_center = button_y + button_h / 2.0;
        if row_w >= 440.0 && (row_center - button_center).abs() > 1.0 {
            failures.borrow_mut().push(format!(
                "{label}: source button {index} center y={button_center:.1} differs from row center y={row_center:.1}"
            ));
        }
        let text = row
            .query_descendants()
            .match_id("AboutPage::source_text")
            .find_first()
            .unwrap();
        if text.size().width < 180.0 {
            failures.borrow_mut().push(format!(
                "{label}: source text lane is cramped: {:?}",
                text.size()
            ));
        }
        if row_w < 440.0 && text.absolute_position().y + text.size().height + 9.5 > button_y {
            failures
                .borrow_mut()
                .push(format!("{label}: source action overlaps text"));
        }
        if row_x < -0.5 || row_x + row_w > viewport_width + 0.5 {
            failures.borrow_mut().push(format!(
                "{label}: source row {index} at x={row_x:.1} w={row_w:.1} exceeds viewport width {viewport_width:.1}"
            ));
        }
        if button_x < row_x - 0.5
            || button_x + button_w > row_x + row_w + 0.5
            || button_y < row_y - 0.5
            || button_y + button_h > row_y + row_h + 0.5
        {
            failures.borrow_mut().push(format!(
                "{label}: source button {index} at x={button_x:.1} y={button_y:.1} w={button_w:.1} h={button_h:.1} escapes row x={row_x:.1} y={row_y:.1} w={row_w:.1} h={row_h:.1}"
            ));
        }
    }
}

#[test]
fn about_opens_nested_licenses_and_centers_source_buttons() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "about_opens_nested_licenses_and_centers_source_buttons"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 1600));
    app.window().show().unwrap();
    app.set_show_home(false);
    app.set_show_about(true);
    let weak = app.as_weak();
    app.on_home_picked(move || {
        let app = weak.upgrade().unwrap();
        app.set_show_about(false);
        app.set_show_home(true);
    });
    let opened = Rc::new(RefCell::new(Vec::new()));
    let urls = opened.clone();
    app.on_license_source_requested(move |url| urls.borrow_mut().push(url.to_string()));
    // Preserve intrinsic-width and source-row geometry coverage with long
    // generated-style crate names and legal text, including unbroken tokens.
    let mut license_text = (0..64)
        .map(|_| "A long license clause with attributed contributors and redistribution terms.")
        .collect::<Vec<_>>()
        .join("\n");
    license_text.push('\n');
    license_text.push_str(&"X".repeat(128));
    let chars: Vec<_> = license_text.chars().collect();
    app.set_license_text_chunks(
        Rc::new(VecModel::from(
            chars
                .chunks(512)
                .map(|chunk| s(&chunk.iter().collect::<String>()))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_license_sources(
        Rc::new(VecModel::from(vec![
            nova::LicenseSource {
                label: s(&"crate-one".repeat(32)),
                license: s(&"License".repeat(24)),
                url: s("https://example.com/crate-one"),
            },
            nova::LicenseSource {
                label: s(&"crate-two".repeat(32)),
                license: s(&"License".repeat(24)),
                url: s("https://example.com/crate-two"),
            },
        ]))
        .into(),
    );
    let failures = Rc::new(RefCell::new(Vec::new()));
    settle(&app);
    assert!(
        ElementHandle::find_by_element_type_name(&app, "BottomNav")
            .next()
            .is_none()
    );
    let licenses = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .unwrap();
    click(&app, &licenses);
    settle(&app);
    assert!(app.get_about_licenses_open());
    check_alignment(&app, "narrow layout", &failures);
    let row = ElementHandle::find_by_element_id(&app, "AboutPage::source_card")
        .next()
        .unwrap();
    let before_x = row.absolute_position().x;
    let y = row.absolute_position().y + row.size().height / 2.0;
    drag_horizontal(
        &app,
        LogicalPosition::new(336.0, y),
        LogicalPosition::new(176.0, y),
    );
    let row = ElementHandle::find_by_element_id(&app, "AboutPage::source_card")
        .next()
        .unwrap();
    assert!(
        (row.absolute_position().x - before_x).abs() <= 1.0,
        "long legal text must not pan horizontally"
    );
    let source = ElementHandle::find_by_element_id(&app, "AboutPage::source_view_button")
        .next()
        .unwrap();
    click(&app, &source);
    assert_eq!(*opened.borrow(), ["https://example.com/crate-one"]);
    // Resize while inside Licenses; the rail appears without losing the page.
    app.window().set_size(slint::PhysicalSize::new(900, 750));
    settle(&app);
    assert!(app.get_about_licenses_open());
    assert_eq!(
        ElementHandle::find_by_element_type_name(&app, "SideItem").count(),
        6
    );
    check_alignment(&app, "wide layout", &failures);
    for language in ["en", "hr"] {
        slint::select_bundled_translation(language).unwrap();
        for (width, height) in [
            (320, 1600),
            (360, 1600),
            (390, 1600),
            (800, 360),
            (1280, 750),
        ] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            settle(&app);
            check_alignment(&app, &format!("{language} {width}x{height}"), &failures);
            if height < 500 {
                let mut seen = std::collections::HashSet::new();
                // Each source must become reachable in short landscape; the
                // accessibility query intentionally omits offscreen items.
                for _ in 0..24 {
                    for row in ElementHandle::find_by_element_id(&app, "AboutPage::source_card") {
                        if row
                            .query_descendants()
                            .match_id("AboutPage::source_view_button")
                            .find_first()
                            .is_some()
                        {
                            for text in row.query_descendants().match_type_name("Text").find_all() {
                                if let Some(label) = text.accessible_label() {
                                    if label.starts_with("crate-one")
                                        || label.starts_with("crate-two")
                                    {
                                        seen.insert(label.to_string());
                                    }
                                }
                            }
                        }
                    }
                    if seen.len() == 2 {
                        break;
                    }
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: LogicalPosition::new(
                                width as f32 / 2.0,
                                height as f32 - 30.0,
                            ),
                            delta_x: 0.0,
                            delta_y: -40.0,
                        });
                    settle(&app);
                    if ElementHandle::find_by_element_id(&app, "AboutPage::source_view_button")
                        .next()
                        .is_some()
                    {
                        check_alignment(
                            &app,
                            &format!("{language} {width}x{height} scrolled"),
                            &failures,
                        );
                    }
                }
                assert_eq!(
                    seen.len(),
                    2,
                    "both source actions must be reachable in {language} landscape"
                );
                // Restore the first row before the next viewport/locale.
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(width as f32 / 2.0, height as f32 - 30.0),
                        delta_x: 0.0,
                        delta_y: 10000.0,
                    });
                settle(&app);
            }
        }
    }
    slint::select_bundled_translation("en").unwrap();
    back(&app);
    settle(&app);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
    assert!(
        ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
            .next()
            .is_some()
    );
    back(&app);
    settle(&app);
    assert!(app.get_show_home() && !app.get_show_about());
    assert!(
        failures.borrow().is_empty(),
        "About geometry failures: {:?}",
        failures.borrow()
    );
}

#[test]
fn opening_real_size_license_catalog_keeps_instantiated_content_bounded() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::opening_real_size_license_catalog_keeps_instantiated_content_bounded"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_show_about(true);
    app.set_license_sources(
        Rc::new(VecModel::from(
            (0..700)
                .map(|index| nova::LicenseSource {
                    label: format!("crate-{index:03} with contributors").into(),
                    license: "MIT OR Apache-2.0".into(),
                    url: format!("https://example.com/crate-{index:03}").into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    // Approximately the production 900KB legal catalog, with bounded rows.
    let mut chunks = vec![s(&"License conditions ž漢🙂\n".repeat(16)); 1900];
    chunks.push(s("FINAL LEGAL NOTICE"));
    app.set_license_text_chunks(Rc::new(VecModel::from(chunks)).into());
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    let licenses = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .unwrap();
    click(&app, &licenses);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let urls = opened.clone();
    app.on_license_source_requested(move |url| urls.borrow_mut().push(url.to_string()));
    // Reach the last source using the persistent page keyboard scope. Native
    // focus stays outside virtualized delegates that disappear on scroll.
    for _ in 0..699 {
        for event in [
            slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::DownArrow.into(),
            },
            slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::DownArrow.into(),
            },
        ] {
            app.window().dispatch_event(event);
        }
        settle(&app);
        for row in ElementHandle::find_by_element_id(&app, "AboutPage::source_card") {
            let _ = row.size();
        }
    }
    assert!(
        ElementHandle::find_by_accessible_label(&app, "crate-699 with contributors")
            .next()
            .is_some(),
        "keyboard must reveal the last virtualized source"
    );
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Return.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Return.into(),
        });
    assert_eq!(*opened.borrow(), ["https://example.com/crate-699"]);
    for (width, height) in [(390, 844), (800, 360), (1280, 750)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        settle(&app);
        // Walk the raw item tree: accessibility queries omit offscreen items
        // even when an eager repeater has instantiated all 700 source cards.
        let mut source_cards = 0;
        let mut legal_chunks = 0;
        let root = i_slint_core::item_tree::ItemRc::new_root(
            i_slint_core::window::WindowInner::from_pub(app.window()).component(),
        );
        root.visit_descendants(|item| {
            for index in 0..item.element_count().unwrap_or(0) {
                if let Some(info) = item.element_type_names_and_ids(index) {
                    source_cards +=
                        usize::from(info.iter().any(|(_, id)| id.ends_with("source_card")));
                    legal_chunks +=
                        usize::from(info.iter().any(|(_, id)| id.ends_with("license_chunk")));
                }
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
        assert!(
            source_cards < 40,
            "{width}x{height}: eagerly instantiated {source_cards} source cards"
        );
        assert!(
            legal_chunks < 20,
            "{width}x{height}: eagerly instantiated {legal_chunks} legal chunks"
        );
    }
    // Virtualization must retain the tail of the legal catalog and Back.
    for _ in 0..24 {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: LogicalPosition::new(700.0, 650.0),
                delta_x: 0.0,
                delta_y: -100_000_000.0,
            });
        // Native wheel physics runs for 180ms independently of Anim.enabled.
        // Finish that motion before letting variable-height rows re-estimate.
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(400));
        settle(&app);
        if ElementHandle::find_by_accessible_label(&app, "FINAL LEGAL NOTICE")
            .next()
            .is_some()
        {
            break;
        }
    }
    let mut flick_geometry = Vec::new();
    let tree = i_slint_core::item_tree::ItemRc::new_root(
        i_slint_core::window::WindowInner::from_pub(app.window()).component(),
    );
    tree.visit_descendants(|item| {
        if let Some(flick) = item.downcast::<i_slint_core::items::Flickable>() {
            let flick = flick.as_pin_ref();
            flick_geometry.push((
                item.geometry(),
                i_slint_core::items::Flickable::FIELD_OFFSETS
                    .content_y()
                    .apply_pin(flick)
                    .get(),
                i_slint_core::items::Flickable::FIELD_OFFSETS
                    .content_height()
                    .apply_pin(flick)
                    .get(),
            ));
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    assert!(
        ElementHandle::find_by_accessible_label(&app, "FINAL LEGAL NOTICE")
            .next()
            .is_some(),
        "tail missing: offset={}, keyboard={}, flicks={flick_geometry:?}, visible text={:?}",
        app.get_about_licenses_scroll_y(),
        app.get_kb_active(),
        ElementHandle::find_by_element_type_name(&app, "Text")
            .map(|text| (
                text.accessible_id(),
                text.accessible_label(),
                text.absolute_position(),
                text.size()
            ))
            .collect::<Vec<_>>()
    );
    let notice = ElementHandle::find_by_accessible_label(&app, "FINAL LEGAL NOTICE")
        .next()
        .unwrap();
    assert!(
        notice.absolute_position().y >= 0.0
            && notice.absolute_position().y + notice.size().height
                <= app.window().size().height as f32 - 17.0,
        "the legal tail must fit inside the physical window and bottom gutter"
    );
    let legal_offset = app.get_about_licenses_scroll_y();
    assert!(legal_offset < -1000.0);
    let about_offset = app.get_about_scroll_y();
    back(&app);
    settle(&app);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
    assert!(
        (app.get_about_scroll_y() - about_offset).abs() < 0.5,
        "Licenses scrolling must not change the About offset"
    );
    let licenses = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .expect("About action is reachable after closing the legal catalog");
    click(&app, &licenses);
    for _ in 0..4 {
        settle(&app);
    }
    assert!(
        (app.get_about_licenses_scroll_y() - legal_offset).abs() < 1.0,
        "same-width reopen must restore the legal position: old={legal_offset}, new={}",
        app.get_about_licenses_scroll_y()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "FINAL LEGAL NOTICE")
            .next()
            .is_some(),
        "the retained tail must remain visible after reopening Licenses"
    );
    back(&app);
    settle(&app);
    assert!(app.get_show_about() && !app.get_about_licenses_open());
}

#[test]
fn tv_arrows_page_the_license_document_after_source_actions() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::tv_arrows_page_the_license_document_after_source_actions"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.global::<nova::InterfaceStyle>().set_tv(true);
    app.set_show_home(false);
    app.set_show_about(true);
    app.set_license_sources(
        Rc::new(VecModel::from(vec![nova::LicenseSource {
            label: "Nova".into(),
            license: "GPL-3.0-or-later".into(),
            url: "https://example.com/nova".into(),
        }]))
        .into(),
    );
    app.set_license_text_chunks(
        Rc::new(VecModel::from(vec![
            s(
                &"License conditions and permissions.\n".repeat(20)
            );
            30
        ]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.show().unwrap();
    settle(&app);
    let button = ElementHandle::find_by_element_id(&app, "AboutPage::about_licenses_button")
        .next()
        .unwrap();
    click(&app, &button);
    settle(&app);
    let before = app.get_about_licenses_scroll_y();
    let key = |key: slint::platform::Key| {
        let text: SharedString = key.into();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        settle(&app);
    };
    key(slint::platform::Key::DownArrow);
    assert!(
        app.get_about_licenses_scroll_y() < before - 100.0,
        "Down must reach the legal text beyond the source action"
    );
    key(slint::platform::Key::UpArrow);
    assert!(
        (app.get_about_licenses_scroll_y() - before).abs() < 2.0,
        "Up restores the source action position"
    );
}

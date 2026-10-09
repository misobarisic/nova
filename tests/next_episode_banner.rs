//! Real player overlay: layout, hidden-OSD taps and nearest-layer Back.
use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(app);
}

fn click(app: &nova::AppWindow, element: &ElementHandle) {
    let p = element.absolute_position();
    let s = element.size();
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    settle(app);
}

fn banner(app: &nova::AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(app, "NextEpisodeBanner").next()
}

#[test]
fn banner_fits_and_routes_actions_without_interrupting_playback() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_show_home(false);
    app.set_is_android(true);
    app.set_player_open(true);
    app.set_playback_started(true);
    app.set_next_episode_token("session:episode".into());
    app.set_next_episode_context("S12 E100".into());
    app.set_next_episode_title(
        "A very long episode title that must wrap without covering the buttons".into(),
    );
    app.set_next_episode_visible(true);
    app.window().show().unwrap();
    let choices = Rc::new(RefCell::new(Vec::new()));
    let recorded = choices.clone();
    app.on_next_episode_streams(move |token| recorded.borrow_mut().push(token.to_string()));
    let dismissals = Rc::new(RefCell::new(Vec::new()));
    let recorded = dismissals.clone();
    let weak = app.as_weak();
    app.on_next_episode_dismiss(move |token| {
        recorded.borrow_mut().push(token.to_string());
        weak.upgrade().unwrap().set_next_episode_visible(false);
    });
    let pauses = Rc::new(RefCell::new(0));
    let recorded = pauses.clone();
    app.on_toggle_pause(move || *recorded.borrow_mut() += 1);
    let closes = Rc::new(RefCell::new(0));
    let recorded = closes.clone();
    app.on_close_player(move || *recorded.borrow_mut() += 1);

    for (width, height, has_thumb) in [
        (360, 800, false),
        (800, 360, false),
        (1280, 720, false),
        (320, 568, false),
        (360, 640, false),
        (360, 800, true),
        (800, 360, true),
        (1280, 720, true),
    ] {
        app.set_next_episode_has_thumb(has_thumb);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        settle(&app);
        let banner = banner(&app).expect("banner mounted");
        let p = banner.absolute_position();
        let s = banner.size();
        if width > height {
            assert!(
                s.width <= 421.0,
                "Android landscape offer must stay compact"
            );
            assert!(p.x > width as f32 / 2.0 - 100.0);
        }
        assert!(
            p.x >= 0.0
                && p.y >= 0.0
                && p.x + s.width <= width as f32 + 1.0
                && p.y + s.height <= height as f32 + 1.0,
            "banner overflow at {width}x{height}: {p:?} {s:?}"
        );
        let action = ElementHandle::find_by_accessible_label(&app, "Choose streams")
            .next()
            .expect("choose action");
        let a = action.absolute_position();
        let size = action.size();
        assert!(size.height >= 43.0 && size.width >= 43.0);
        let arrow = action
            .query_descendants()
            .match_inherits("IcChevronRight")
            .find_first()
            .expect("action arrow");
        let arrow_pos = arrow.absolute_position();
        assert!(
            (arrow_pos.y + arrow.size().height / 2.0 - a.y - size.height / 2.0).abs() <= 1.0,
            "action arrow is not vertically centered at {width}x{height}"
        );
        assert!(
            a.x >= p.x
                && a.y >= p.y
                && a.x + size.width <= p.x + s.width + 1.0
                && a.y + size.height <= p.y + s.height + 1.0,
            "action clipped at {width}x{height}"
        );
        let transport_top = height as f32 / 2.0 - 36.0;
        assert!(
            p.y + s.height <= transport_top || p.y >= transport_top + 72.0,
            "banner overlaps center transport at {width}x{height}"
        );
    }
    app.window().set_size(slint::PhysicalSize::new(800, 360));
    settle(&app);
    // Mounting the banner must not steal focus from Pause.
    key(&app, slint::platform::Key::Return);
    assert_eq!(*pauses.borrow(), 1);
    app.set_osd_visible(false);
    let action = ElementHandle::find_by_accessible_label(&app, "Choose streams")
        .next()
        .unwrap();
    click(&app, &action);
    assert_eq!(&*choices.borrow(), &["session:episode"]);
    assert_eq!(*pauses.borrow(), 1, "banner tap leaked to video");

    let dismiss = ElementHandle::find_by_accessible_label(&app, "Dismiss next episode")
        .next()
        .unwrap();
    click(&app, &dismiss);
    assert!(banner(&app).is_none());
    assert_eq!(*closes.borrow(), 0);
    app.set_next_episode_visible(true);
    settle(&app);
    // Arrow navigation reaches both new actions, skipping Android's absent fullscreen.
    for _ in 0..12 {
        key(&app, slint::platform::Key::RightArrow);
    }
    key(&app, slint::platform::Key::LeftArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(choices.borrow().len(), 2);
    key(&app, slint::platform::Key::LeftArrow);
    key(&app, slint::platform::Key::Return); // Settings gear.
    assert!(banner(&app).is_none(), "banner must yield to player menus");
    key(&app, slint::platform::Key::Escape);
    assert!(banner(&app).is_some());
    key(&app, slint::platform::Key::Back);
    assert_eq!(dismissals.borrow().len(), 2);
    assert_eq!(*closes.borrow(), 0, "first Back dismisses the offer only");
    key(&app, slint::platform::Key::Back);
    assert_eq!(*closes.borrow(), 1);

    slint::select_bundled_translation("hr").unwrap();
    app.set_next_episode_visible(true);
    app.set_next_episode_has_thumb(true);
    settle(&app);
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Odaberite streamove")
            .next()
            .is_some()
    );
    slint::select_bundled_translation("en").unwrap();
}

//! Responsive selector geometry, movie actions and keyboard page transitions.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn idle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
}

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    idle();
}

fn click(app: &nova::AppWindow, element: ElementHandle) {
    let p = element.absolute_position();
    let size = element.size();
    let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
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
    idle();
}

fn row(index: i32) -> nova::StreamRow {
    nova::StreamRow {
        id: format!("stream-{index}").into(),
        text: format!("Torrentio · 1080p\nArrival.2016.1080p.BluRay.HEVC.a_long_release_filename_with_metadata_{index}").into(),
        lines: 1, // The actual wrapped measurement must override this floor.
        ..Default::default()
    }
}

#[test]
fn responsive_selector_keeps_state_and_dispatches_movie_and_stream_actions() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_selected_title("Arrival".into());
    app.set_selected_year("2016".into());
    app.set_detail_watch_label("Start".into());
    app.window().set_size(slint::PhysicalSize::new(900, 800));
    app.window().show().unwrap();
    idle();

    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Episodes").count(),
        0
    );
    let watch = element(&app, "DetailPage::watch_action_button");
    let library = element(&app, "DetailPage::library_state_button");
    assert!((watch.absolute_position().y - library.absolute_position().y).abs() < 1.0);
    assert!(library.absolute_position().x >= watch.absolute_position().x + watch.size().width);
    let weak = app.as_weak();
    app.on_watch_now(move || weak.upgrade().unwrap().set_stream_selector_open(true));
    click(&app, watch);
    assert!(app.get_stream_selector_open());
    let artwork =
        slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(16, 9));
    app.set_selected_backdrop(artwork.clone());
    app.set_selected_poster(artwork);

    app.set_stream_total(27);
    app.set_stream_page_count(2);
    app.set_stream_page(1);
    app.set_stream_page_start(25);
    app.set_detail_kb_s(25);
    app.set_stream_filter(2);
    app.set_stream_addons(
        Rc::new(VecModel::from(vec![
            SharedString::from("Torrentio"),
            SharedString::from("MediaFusion"),
        ]))
        .into(),
    );
    app.set_streams(Rc::new(VecModel::from(vec![row(25), row(26)])).into());
    app.set_selected_title(
        "Arrival with a very long translated title that wraps across several lines".into(),
    );
    let context = "Season 1 · Episode 12 · A very long episode title that needs to wrap on phones";
    let hint = "This stream cannot be played here.";
    app.set_episode_context(context.into());
    app.set_streams_hint(hint.into());

    // Include both sides of each breakpoint and short phone/desktop landscape.
    let sizes = [
        (320, 800),
        (390, 844),
        (699, 800),
        (700, 800),
        (1199, 800),
        (1200, 800),
        (1440, 900),
        (1920, 1080),
        (320, 360),
        (390, 360),
        (699, 360),
        (700, 360),
        (900, 360),
        (1440, 480),
    ];
    for (true_black, width, height) in [false, true].into_iter().flat_map(|true_black| {
        sizes
            .into_iter()
            .map(move |(width, height)| (true_black, width, height))
    }) {
        nova_ui::apply_theme(&app, true_black);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        idle();
        let selector = ElementHandle::find_by_element_type_name(&app, "StreamSelector")
            .next()
            .unwrap();
        assert_eq!(
            selector.absolute_position(),
            slint::LogicalPosition::new(0.0, 0.0)
        );
        assert!((selector.size().width - width as f32).abs() < 1.0);
        assert!((selector.size().height - height as f32).abs() < 1.0);
        let backdrop = element(&app, "StreamSelector::artwork");
        assert!(
            backdrop.absolute_position().y.abs() < 0.5,
            "{width}x{height}: backdrop must begin at the top, not float behind results"
        );
        let viewport = element(&app, "StreamSelector::result_scroll");
        let p = viewport.absolute_position();
        let size = viewport.size();
        assert!(p.x >= 0.0 && p.x + size.width <= width as f32 + 1.0);
        assert!(p.y >= 0.0 && p.y + size.height <= height as f32 + 1.0);
        assert!(
            size.height >= 70.0,
            "{width}x{height}: results collapsed to {size:?}"
        );
        if width >= 1200 && height >= 600 {
            assert!(
                p.x > width as f32 * 0.3,
                "wide layout needs its artwork column"
            );
        } else {
            assert!(
                p.x < 40.0,
                "compact/phone rows should use the full canvas width"
            );
        }
        let hint_text = ElementHandle::find_by_accessible_label(&app, hint)
            .next()
            .expect("stream feedback remains visible in every layout");
        assert!(hint_text.size().height > 0.0);
        assert!(hint_text.absolute_position().y + hint_text.size().height <= p.y + 1.0);
        if height < 600 {
            let context_text = ElementHandle::find_by_accessible_label(&app, context)
                .next()
                .unwrap();
            let header_bottom = element(&app, "StreamSelector::selector_body")
                .absolute_position()
                .y;
            assert!(
                context_text.absolute_position().y + context_text.size().height <= header_bottom,
                "{width}x{height}: episode context must not overlap the stream heading"
            );
        }
        assert_eq!(app.get_stream_page(), 1);
        assert_eq!(app.get_stream_filter(), 2);
        assert_eq!(app.get_detail_kb_s(), 25);
        assert_eq!(app.get_streams().row_count(), 2);
    }
    app.set_episode_context("".into());
    app.set_streams_hint("".into());
    nova_ui::apply_theme(&app, false);

    // Real page responses keep absolute indexes, including keyboard movement
    // through the page boundary (24 -> 25) and the final Enter activation.
    let pages = Rc::new(RefCell::new(Vec::new()));
    let pages2 = pages.clone();
    let weak = app.as_weak();
    app.on_stream_page_picked(move |page| {
        pages2.borrow_mut().push(page);
        let app = weak.upgrade().unwrap();
        app.set_stream_page(page);
        app.set_stream_page_start(page * 25);
        let range = if page == 0 { 0..25 } else { 25..27 };
        app.set_streams(Rc::new(VecModel::from(range.map(row).collect::<Vec<_>>())).into());
    });
    let picks = Rc::new(RefCell::new(Vec::new()));
    let picks2 = picks.clone();
    app.on_stream_picked(move |index| picks2.borrow_mut().push(index));
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    key(&app, slint::platform::Key::PageUp);
    assert_eq!(pages.borrow().as_slice(), &[0]);
    for _ in 0..25 {
        key(&app, slint::platform::Key::DownArrow);
    }
    assert_eq!(pages.borrow().as_slice(), &[0, 1]);
    assert_eq!(app.get_detail_kb_s(), 25);
    key(&app, slint::platform::Key::Return);
    assert_eq!(picks.borrow().as_slice(), &[25]);

    // The separate actions target must not activate row playback.
    let action_ids = Rc::new(RefCell::new(Vec::new()));
    let action_ids2 = action_ids.clone();
    let weak = app.as_weak();
    app.set_touch_menus(true);
    app.on_stream_action_requested(move |id| {
        action_ids2.borrow_mut().push(id);
        let app = weak.upgrade().unwrap();
        app.set_stream_action_items(
            Rc::new(VecModel::from(vec![nova::SheetItem {
                label: "Download".into(),
                enabled: true,
            }]))
            .into(),
        );
        app.set_stream_action_open(true);
    });
    click(&app, element(&app, "StreamList::stream_actions"));
    assert_eq!(
        action_ids.borrow().as_slice(),
        &[SharedString::from("stream-25")]
    );
    assert_eq!(picks.borrow().as_slice(), &[25]);
    assert!(app.get_stream_action_open());

    let backs = Rc::new(RefCell::new(0));
    let backs2 = backs.clone();
    let weak = app.as_weak();
    app.on_stream_selector_back(move || {
        *backs2.borrow_mut() += 1;
        weak.upgrade().unwrap().set_stream_selector_open(false);
    });
    // Back dismisses the action sheet before it leaves the selector.
    key(&app, slint::platform::Key::Back);
    assert!(app.get_stream_selector_open());
    assert!(!app.get_stream_action_open());
    assert_eq!(*backs.borrow(), 0);
    key(&app, slint::platform::Key::Back);
    assert_eq!(*backs.borrow(), 1);
    assert!(!app.get_stream_selector_open());
    assert!(app.get_modal_visible());

    // Return scrolling uses the card's measured position after layout, rather
    // than an estimated stride or the next-unwatched episode's index.
    app.set_detail_is_movie(false);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_season_names(Rc::new(VecModel::from(vec![SharedString::from("Season 2")])).into());
    app.set_season_combo_idx(0);
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (0..20)
                .map(|index| nova::EpisodeRow {
                    text: format!("Episode {}", index + 1).into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    idle();
    app.set_detail_kb_ep(9);
    app.set_detail_episode_reveal_request(1);
    idle();
    // The changed handler arms the stopped timer in this frame; expire it
    // in the next frame after the updated episode layout has settled.
    idle();
    let cards = ElementHandle::find_by_element_id(&app, "DetailPage::ep_card").collect::<Vec<_>>();
    // The testing backend enumerates cards intersecting the visible viewport.
    let title = ElementHandle::find_by_accessible_label(&app, "Episode 10")
        .next()
        .expect("last watched episode must be visible");
    let position = title.absolute_position();
    let card = cards
        .iter()
        .find(|card| {
            let p = card.absolute_position();
            let size = card.size();
            position.x >= p.x
                && position.x < p.x + size.width
                && position.y >= p.y
                && position.y < p.y + size.height
        })
        .expect("episode title belongs to the revealed card");
    let viewport = element(&app, "DetailPage::det_scroll");
    assert!(card.absolute_position().y >= viewport.absolute_position().y - 1.0);
    assert!(
        card.absolute_position().y + card.size().height
            <= viewport.absolute_position().y + viewport.size().height + 1.0
    );

    // Pointer Back uses the same routing as the keyboard; there is no Close X.
    app.set_stream_selector_open(true);
    idle();
    click(&app, element(&app, "StreamSelector::back_button"));
    assert_eq!(*backs.borrow(), 2);
    assert!(!app.get_stream_selector_open());
    app.set_stream_selector_open(true);
    idle();
    assert_eq!(
        ElementHandle::find_by_element_id(&app, "StreamSelector::close_button").count(),
        0
    );
    key(&app, slint::platform::Key::Escape);
    assert!(!app.get_stream_selector_open());
    assert!(app.get_modal_visible());
    app.set_stream_selector_open(false);
    app.set_modal_visible(true);
    idle();

    // Movie tabs remain restricted even when Tracking is available.
    app.set_modal_visible(false);
    idle();
    app.set_detail_tab(0);
    app.set_detail_is_movie(true);
    app.set_modal_visible(true);
    app.set_in_library(true);
    idle();
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Episodes").count(),
        0
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Tracking")
            .next()
            .is_some()
    );
    app.set_detail_is_movie(false);
    idle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Episodes")
            .next()
            .is_some()
    );

    // Translated loading and empty states also fit a short phone viewport.
    slint::select_bundled_translation("hr").unwrap();
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_streams(Default::default());
    app.set_stream_total(0);
    app.set_stream_page_count(1);
    app.set_streams_hint("".into());
    app.set_streams_searching(true);
    app.window().set_size(slint::PhysicalSize::new(320, 480));
    idle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Odaberite izvor")
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Učitavanje zapisa…")
            .next()
            .is_some()
    );
    app.set_streams_searching(false);
    idle();
    assert!(
        ElementHandle::find_by_accessible_label(&app, "Nema pronađenih zapisa.")
            .next()
            .is_some()
    );
    let viewport = element(&app, "StreamSelector::result_scroll");
    assert!(viewport.size().height >= 70.0);
    slint::select_bundled_translation("en").unwrap();

    // Returning from playback recreates Detail with its selector already open.
    // System Back must work before the hidden detail navigation gains focus.
    app.set_modal_visible(false);
    idle();
    app.set_stream_selector_open(true);
    app.set_modal_visible(true);
    idle();
    let before = *backs.borrow();
    key(&app, slint::platform::Key::Back);
    assert_eq!(*backs.borrow(), before + 1);
    assert!(!app.get_stream_selector_open());
    assert!(app.get_modal_visible());
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressRepeated {
            text: slint::platform::Key::Back.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        });
    idle();
    assert_eq!(
        *backs.borrow(),
        before + 1,
        "held Back must only leave once"
    );
    assert!(app.get_modal_visible());
}

#[test]
#[ignore = "requires a desktop display; SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn render_stream_selector_previews() {
    let out = std::path::PathBuf::from(
        std::env::var_os("NOVA_STREAM_SELECTOR_PREVIEW_DIR")
            .unwrap_or_else(|| "output/stream-selector".into()),
    );
    std::fs::create_dir_all(&out).unwrap();
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(
        &app,
        std::env::var_os("NOVA_STREAM_SELECTOR_PREVIEW_TRUE_BLACK").is_some(),
    );
    app.set_animations(false);
    app.set_modal_visible(true);
    app.set_detail_is_movie(true);
    app.set_stream_selector_open(true);
    app.set_selected_title("Arrival".into());
    app.set_selected_year("2016".into());
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(128, 128);
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let y = (i / 128) as u8;
        *pixel = slint::Rgba8Pixel::new(12 + y / 4, 22 + y / 3, 38 + y / 3, 255);
    }
    let backdrop = slint::Image::from_rgba8(pixels);
    app.set_selected_backdrop(backdrop.clone());
    app.set_selected_poster(backdrop);
    app.set_stream_addons(
        Rc::new(VecModel::from(vec![
            "Torrentio".into(),
            "MediaFusion".into(),
            "Local".into(),
        ]))
        .into(),
    );
    app.set_stream_total(30);
    app.set_stream_page_count(2);
    let mut rows: Vec<_> = (0..25).map(row).collect();
    rows[0].text = "Arrival · 1080p".into();
    rows[0].details = "Downloaded · 2.1 GB\n👤 10 · 💾 · ⚙️ NyaaSi\n🇬🇧 / 🇩🇪 / 🇫🇷 / 🇪🇸".into();
    rows[0].is_download = true;
    rows[0].download_action = 4;
    app.set_streams(Rc::new(VecModel::from(rows)).into());
    app.set_detail_kb_s(1);
    app.set_kb_active(true);
    let sizes = [
        ("phone", 390, 844),
        ("compact", 1000, 800),
        ("artwork", 1440, 900),
    ];
    app.window()
        .set_size(slint::PhysicalSize::new(sizes[0].1, sizes[0].2));
    app.window().show().unwrap();
    let index = Rc::new(std::cell::Cell::new(0));
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let app = weak.upgrade().unwrap();
            let i = index.get();
            let snapshot = app.window().take_snapshot().unwrap();
            let bytes: Vec<u8> = snapshot
                .as_slice()
                .iter()
                .flat_map(|p| [p.r, p.g, p.b, p.a])
                .collect();
            image::save_buffer(
                out.join(format!("{}.png", sizes[i].0)),
                &bytes,
                snapshot.width(),
                snapshot.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
            index.set(i + 1);
            if i + 1 == sizes.len() {
                slint::quit_event_loop().unwrap();
            } else {
                app.window()
                    .set_size(slint::PhysicalSize::new(sizes[i + 1].1, sizes[i + 1].2));
            }
        },
    );
    slint::run_event_loop().unwrap();
}

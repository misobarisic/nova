#![cfg(not(feature = "live-preview"))]
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::ComponentHandle;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn capture(app: &nova::AppWindow, dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let snapshot = app.window().take_snapshot().unwrap();
    let bytes: Vec<_> = snapshot
        .as_slice()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    image::save_buffer(
        dir.join(name),
        &bytes,
        snapshot.width(),
        snapshot.height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
}

async fn settle() {
    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let timer_ready = ready.clone();
    let timer_waker = waker.clone();
    slint::Timer::single_shot(Duration::from_millis(350), move || {
        timer_ready.set(true);
        if let Some(waker) = timer_waker.borrow_mut().take() {
            waker.wake();
        }
    });
    std::future::poll_fn(|cx| {
        if ready.get() {
            std::task::Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
    .await;
}
async fn button(app: &nova::AppWindow, name: &str) -> ElementHandle {
    for attempt in 0..14 {
        if let Some(target) = ElementHandle::find_by_accessible_label(app, name)
            .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
        {
            let viewport = ElementHandle::find_by_element_id(app, "SettingsPage::set_scroll")
                .next()
                .unwrap();
            let p = viewport.absolute_position();
            let size = viewport.size();
            let target_p = target.absolute_position();
            let target_size = target.size();
            if target_p.y < p.y + 72.0 || target_p.y + target_size.height > p.y + size.height - 16.0
            {
                app.set_settings_scroll_y(
                    app.get_settings_scroll_y() + p.y + size.height / 2.0
                        - target_p.y
                        - target_size.height / 2.0,
                );
                settle().await;
            }
            let target = ElementHandle::find_by_accessible_label(app, name)
                .find(|e| e.accessible_role() == Some(AccessibleRole::Button))
                .unwrap();
            let p = target.absolute_position();
            assert!(
                p.x >= 0.0 && p.x + target.size().width <= 360.5,
                "{name} overflows phone width"
            );
            return target;
        }
        // Real compiled trees omit completely clipped controls from queries.
        // Search the Tracking scroll instead of assuming all services fit.
        app.set_settings_scroll_y(if attempt == 0 {
            0.0
        } else {
            app.get_settings_scroll_y() - 280.0
        });
        settle().await;
    }
    if let Some(dir) = std::env::var_os("NOVA_TRACKING_CARD_PREVIEW_DIR") {
        capture(
            app,
            std::path::Path::new(&dir),
            &format!("missing-{name}.png"),
        );
    }
    panic!(
        "missing action {name}; scroll={}, pending bytes={}",
        app.get_settings_scroll_y(),
        app.get_stremio_pending().len()
    )
}
async fn click(app: &nova::AppWindow, name: &str) {
    let target = button(app, name).await;
    // Hover first so the Flickable can settle/clamp a shrinking body before
    // pointer-down; a moving target between press/release is a drag.
    let p = target.absolute_position();
    let size = target.size();
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0),
        });
    settle().await;
    let target = button(app, name).await;
    target
        .single_click(slint::platform::PointerEventButton::Left)
        .await;
    settle().await;
}

#[test]
fn initial_merge_requires_preview_and_survives_recreation() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::initial_merge_requires_preview_and_survives_recreation"
    )) {
        return;
    }
    let previews = std::env::var_os("NOVA_TRACKING_CARD_PREVIEW_DIR").map(std::path::PathBuf::from);
    if previews.is_none() {
        i_slint_backend_testing::init_integration_test_with_system_time();
    }
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_tracking_accounts(
        Rc::new(slint::VecModel::from(vec![
            nova::TrackingAccountRow {
                service: 0,
                state: "Not connected".into(),
                ..Default::default()
            },
            nova::TrackingAccountRow {
                service: 1,
                name: "demo".into(),
                state: "Connected".into(),
                connected: true,
                ..Default::default()
            },
        ]))
        .into(),
    );
    app.set_show_home(false);
    app.set_show_settings(true);
    app.set_settings_selected_id(12);
    app.set_settings_detail_open(true);
    let connects = Rc::new(Cell::new(0));
    let count = connects.clone();
    app.on_stremio_connect(move || count.set(count.get() + 1));
    let starts = Rc::new(Cell::new(0));
    let count = starts.clone();
    app.on_stremio_start(move || count.set(count.get() + 1));
    let syncs = Rc::new(Cell::new(0));
    let count = syncs.clone();
    app.on_stremio_sync(move || count.set(count.get() + 1));
    let opened = Rc::new(Cell::new(0));
    let count = opened.clone();
    app.on_stremio_open_link(move || count.set(count.get() + 1));
    let cancelled = Rc::new(Cell::new(0));
    let count = cancelled.clone();
    app.on_stremio_cancel(move || count.set(count.get() + 1));
    let disconnected = Rc::new(Cell::new(0));
    let count = disconnected.clone();
    app.on_stremio_disconnect(move || count.set(count.get() + 1));
    let weak = app.as_weak();
    slint::spawn_local(async move {
        let app = weak.upgrade().unwrap();
        settle().await;
        for service in ["MyAnimeList", "AniList", "Stremio"] {
            let header = button(&app, service).await;
            assert!((90.0..115.0).contains(&header.size().height), "compact {service} header");
        }
        if let Some(dir) = &previews { capture(&app, dir, "tracking-cards-collapsed.png"); }
        if let Some(dir) = &previews {
            // Native screenshot mode renders the final view directly. The
            // interaction and long-list regression continues below headlessly.
            click(&app, "Stremio").await;
            app.set_stremio_connected(true);
            app.set_stremio_account("demo@example.org".into());
            app.set_stremio_status("Stremio sync complete".into());
            app.set_stremio_pending("Gods' Game We Play\nEpisode metadata unavailable\nseries · native:123\n\nSaihate no Paladin\nEpisode metadata unavailable\nseries · kitsu:44432\n\nPsycho-Pass\nEpisode metadata unavailable\nseries · kitsu:7000".into());
            app.window().set_size(slint::PhysicalSize::new(360, 1000));
            settle().await;
            app.set_settings_scroll_y(-240.0);
            settle().await;
            capture(&app, dir, "tracking-card-expanded.png");
            slint::quit_event_loop().unwrap();
            return;
        }
        let card = button(&app, "Stremio").await;
        assert!((90.0..115.0).contains(&card.size().height), "collapsed account card stays compact");
        assert!(ElementHandle::find_by_accessible_label(&app, "Connect Stremio").next().is_none(), "collapsed cards hide account controls");
        click(&app, "Stremio").await;
        click(&app, "Connect Stremio").await;
        assert_eq!(connects.get(), 1);
        app.set_stremio_link_code("ABCD".into());
        app.set_stremio_link_url("https://link.stremio.com".into());
        settle().await;
        assert!(
            button(&app, "Open browser")
                .await
                .accessible_enabled()
                .unwrap()
        );
        assert!(
            button(&app, "Cancel sign-in")
                .await
                .accessible_enabled()
                .unwrap()
        );
        click(&app, "Open browser").await;
        assert_eq!(opened.get(), 1);
        click(&app, "Cancel sign-in").await;
        assert_eq!(cancelled.get(), 1);
        app.set_stremio_link_code("".into());
        app.set_stremio_connected(true);
        app.set_stremio_account("viewer@example.org".into());
        app.set_stremio_preview("Preparing library merge…".into());
        settle().await;
        assert_eq!(
            button(&app, "Start sync").await.accessible_enabled(),
            Some(false)
        );
        assert!(
            ElementHandle::find_by_accessible_label(&app, "Sync now")
                .next()
                .is_none()
        );
        app.set_stremio_merge_ready(true);
        app.set_stremio_busy(true);
        settle().await;
        assert_eq!(
            button(&app, "Start sync").await.accessible_enabled(),
            Some(false)
        );
        app.set_stremio_busy(false);
        app.set_show_settings(false);
        settle().await;
        app.set_show_settings(true);
        settle().await;
        click(&app, "Stremio").await;
        assert_eq!(app.get_stremio_account(), "viewer@example.org");
        assert_eq!(
            button(&app, "Start sync").await.accessible_enabled(),
            Some(true)
        );
        click(&app, "Start sync").await;
        assert_eq!(starts.get(), 1);
        app.set_stremio_preview("".into());
        settle().await;
        click(&app, "Sync now").await;
        assert_eq!(syncs.get(), 1);
        app.set_stremio_pending(
            "A title with a very long metadata mapping name that wraps on phones\nAnother title"
                .into(),
        );
        settle().await;
        let disconnect = button(&app, "Disconnect").await;
        let pending = ElementHandle::find_by_element_id(&app, "StremioSettings::pending_titles")
            .next()
            .expect("pending titles remain visible above Disconnect");
        let pending_height = pending.size().height;
        assert!(
            (30.0..140.0).contains(&pending_height),
            "wrapped pending text should use its content height: {pending_height}"
        );
        let gap = disconnect.absolute_position().y - pending.absolute_position().y - pending_height;
        assert!(
            (0.0..30.0).contains(&gap),
            "pending titles should have ordinary spacing before Disconnect: {gap}"
        );
        // Spare viewport height must not turn labels into padded text blocks.
        app.window().set_size(slint::PhysicalSize::new(360, 1600));
        settle().await;
        button(&app, "Disconnect").await;
        let pending = ElementHandle::find_by_element_id(&app, "StremioSettings::pending_titles")
            .next()
            .unwrap();
        assert!(
            (pending.size().height - pending_height).abs() < 1.0,
            "pending text stretched in a taller viewport"
        );
        // A sync can resolve most mappings while this scroll is already at
        // the bottom of a much longer list. Its previous extent must shrink.
        app.set_stremio_pending(
            "Long pending title\nEpisode metadata unavailable\nseries · native:123\n\n"
                .repeat(59)
                .into(),
        );
        settle().await;
        app.set_settings_scroll_y(-100_000.0);
        settle().await;
        button(&app, "Disconnect").await;
        app.set_stremio_pending(
            "Gods' Game We Play\nEpisode metadata unavailable\nseries · ap56f71f0a-03b3-1edc-a14a-83812d8073b0\n\nSaihate no Paladin\nEpisode metadata unavailable\nseries · kitsu:44432\n\nPsycho-Pass\nEpisode metadata unavailable\nseries · kitsu:7000".into(),
        );
        let mut pending_sizes = None;
        for height in [800, 1600] {
            app.window().set_size(slint::PhysicalSize::new(360, height));
            settle().await;
            button(&app, "Disconnect").await;
            let card = ElementHandle::find_by_element_type_name(&app, "StremioSettings")
                .next().expect("expanded Stremio card");
            let disconnect = button(&app, "Disconnect").await;
            let bottom_gap = card.absolute_position().y + card.size().height
                - disconnect.absolute_position().y - disconnect.size().height;
            assert!((0.0..34.0).contains(&bottom_gap),
                "expanded card has excessive bottom padding at {height}px: {bottom_gap}; card {:?} {:?}; disconnect {:?} {:?}",
                card.absolute_position(), card.size(), disconnect.absolute_position(), disconnect.size());
            let labels: Vec<_> = ["pending_heading", "pending_explanation", "pending_titles"]
                .map(|id| {
                    ElementHandle::find_by_element_id(&app, &format!("StremioSettings::{id}"))
                        .next()
                        .unwrap_or_else(|| panic!("missing {id} at viewport height {height}"))
                })
                .into();
            let heights: Vec<_> = labels.iter().map(|label| label.size().height).collect();
            assert!((12.0..28.0).contains(&heights[0]), "pending heading stretched: {heights:?}");
            assert!((24.0..64.0).contains(&heights[1]), "pending explanation stretched: {heights:?}");
            assert!((100.0..300.0).contains(&heights[2]), "pending titles stretched: {heights:?}");
            if let Some(previous) = &pending_sizes {
                assert_eq!(&heights, previous, "pending labels grew with their host");
            }
            pending_sizes = Some(heights);
            for pair in labels.windows(2) {
                let gap = pair[1].absolute_position().y
                    - pair[0].absolute_position().y
                    - pair[0].size().height;
                assert!(
                    (12.0..16.0).contains(&gap),
                    "pending labels have an excessive gap at viewport height {height}: {gap}"
                );
            }
        }
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        settle().await;
        button(&app, "Disconnect").await;
        if let Some(dir) = &previews { capture(&app, dir, "tracking-card-expanded.png"); }
        click(&app, "Disconnect").await;
        assert_eq!(disconnected.get(), 1);
        app.set_stremio_connected(false);
        app.set_stremio_status("Stremio needs reconnection".into());
        settle().await;
        assert!(
            button(&app, "Connect Stremio")
                .await
                .accessible_enabled()
                .unwrap()
        );
        click(&app, "Disconnect").await;
        assert_eq!(
            disconnected.get(),
            2,
            "saved offline credentials remain removable"
        );
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Back.into(),
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Back.into(),
            });
        settle().await;
        assert!(!app.get_settings_detail_open());
        assert_eq!(app.get_stremio_account(), "viewer@example.org");
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}

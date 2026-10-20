//! Cross-show queue actions remain stable through reorders and responsive pages.
use i_slint_backend_testing::{AccessibleRole, ElementQuery};
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    for _ in 0..12 {
        for item in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (item.size(), item.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    }
}
fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    settle(app);
}
fn fixture() -> Vec<nova::WatchQueueRow> {
    vec![
        nova::WatchQueueRow {
            id: "episode-a".into(),
            title: "The Quiet Sea".into(),
            context: "S2 E4 · A signal in the dark".into(),
            ..Default::default()
        },
        nova::WatchQueueRow {
            id: "movie-b".into(),
            title: "A Long Way Home".into(),
            context: "Movie".into(),
            ..Default::default()
        },
        nova::WatchQueueRow {
            id: "episode-c".into(),
            title: "The Observatory".into(),
            context: "S1 E8 · The other side of midnight".into(),
            ..Default::default()
        },
    ]
}

#[test]
fn queue_keyboard_reorder_keeps_the_selected_identity_and_back_returns_to_more() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::queue_keyboard_reorder_keeps_the_selected_identity_and_back_returns_to_more"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let rows = Rc::new(VecModel::from(fixture()));
    let queue = app.global::<nova::QueueBridge>();
    queue.set_rows(rows.clone().into());
    queue.set_open(true);
    let moves = Rc::new(RefCell::new(Vec::new()));
    let observed = moves.clone();
    let model = rows.clone();
    queue.on_move(move |id, direction| {
        observed.borrow_mut().push((id.to_string(), direction));
        let mut entries = model.iter().collect::<Vec<_>>();
        let index = entries.iter().position(|entry| entry.id == id).unwrap();
        let target = (index as i32 + direction).clamp(0, entries.len() as i32 - 1) as usize;
        entries.swap(index, target);
        model.set_vec(entries);
    });
    let removed = Rc::new(RefCell::new(Vec::new()));
    let observed = removed.clone();
    queue.on_remove(move |id| observed.borrow_mut().push(id.to_string()));
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.show().unwrap();
    settle(&app);
    key(&app, slint::platform::Key::DownArrow);
    key(&app, slint::platform::Key::RightArrow);
    key(&app, slint::platform::Key::Return);
    assert_eq!(rows.row_data(0).unwrap().id, "movie-b");
    key(&app, slint::platform::Key::Return);
    assert_eq!(
        moves.borrow().as_slice(),
        &[("movie-b".to_string(), -1), ("movie-b".to_string(), -1)]
    );
    key(&app, slint::platform::Key::Delete);
    assert_eq!(removed.borrow().as_slice(), &["movie-b"]);
    app.set_system_back_request(app.get_system_back_request() + 1);
    settle(&app);
    assert!(!queue.get_open());
    assert!(app.get_more_open());
    assert!(!app.get_show_home());
}

#[test]
fn queue_actions_fit_small_phone_and_landscape_with_long_titles() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::queue_actions_fit_small_phone_and_landscape_with_long_titles"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let queue = app.global::<nova::QueueBridge>();
    let mut entries = fixture();
    entries[0].title = "A very long title that must wrap without widening the evening queue and hiding its controls".into();
    queue.set_rows(Rc::new(VecModel::from(entries)).into());
    queue.set_open(true);
    app.show().unwrap();
    for (width, height) in [(320, 800), (390, 844), (720, 320), (1280, 800)] {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        settle(&app);
        assert!(queue.get_open(), "queue stays open across window resizes");
        let find_actions = || ElementQuery::from_root(&app)
            .match_predicate(|item| {
                item.accessible_role() == Some(AccessibleRole::Button)
                    && item.accessible_label().is_some_and(|label| {
                        ["Play ·", "Move up ·", "Move down ·", "Remove from queue ·"]
                            .iter().any(|prefix| label.starts_with(prefix))
                    })
            }).find_all();
        let mut actions = find_actions();
        if actions.is_empty() {
            // Short landscape starts with the heading/hero above the rows.
            // Query real visible controls after scrolling into the plan.
            app.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 / 2.0, height as f32 * 0.75),
                delta_x: 0.0, delta_y: -360.0,
            });
            settle(&app);
            actions = find_actions();
        }
        assert!(!actions.is_empty(), "queue actions are reachable at {width}×{height}");
        for action in actions {
            let pos = action.absolute_position();
            let size = action.size();
            assert!(size.width >= 44.0 && size.height >= 44.0);
            assert!(
                pos.x >= -0.5 && pos.x + size.width <= width as f32 + 0.5,
                "{width}×{height}: queue action overflow at {pos:?} {size:?}"
            );
            assert_eq!(action.accessible_role(), Some(AccessibleRole::Button));
        }
    }
}

#[test]
#[ignore = "requires a display; optional native design preview"]
fn render_journal_and_queue_previews() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::render_journal_and_queue_previews"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    nova_ui::apply_theme(&app, false);
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_show_home(false);
    app.set_more_open(true);
    let queue = app.global::<nova::QueueBridge>();
    queue.set_rows(Rc::new(VecModel::from(fixture())).into());
    queue.set_summary("1 movie · 2 episodes".into());
    queue.set_open(true);
    let journal = app.global::<nova::JournalBridge>();
    journal.set_current_year(2026);
    journal.set_oldest_year(2024);
    journal.set_year(2026);
    journal.set_watched("124:36:20".into());
    journal.set_title_count(32);
    journal.set_completion_count(84);
    journal.set_rewatch_count(12);
    journal.set_session_count(108);
    journal.set_active_days(64);
    journal.set_months(
        Rc::new(VecModel::from(
            (0..12)
                .map(|i| nova::JournalMonthRow {
                    label: format!("{:02}", i + 1).into(),
                    fraction: [
                        0.25, 0.45, 0.33, 0.7, 0.38, 0.85, 0.55, 1.0, 0.72, 0.5, 0.0, 0.0,
                    ][i],
                    watched: "12:04:20".into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    journal.set_titles(
        Rc::new(VecModel::from(vec![nova::JournalTitleRow {
            title: "The Quiet Sea".into(),
            first_date: "2026-04-02".into(),
            finished_date: "2026-07-18".into(),
            watched: "14:20:00".into(),
            sessions: 18,
            rewatches: 2,
            ..Default::default()
        }]))
        .into(),
    );
    journal.set_sessions(
        Rc::new(VecModel::from(vec![nova::JournalSessionRow {
            id: "1".into(),
            title: "The Quiet Sea".into(),
            episode: "S2 E4 · A signal in the dark".into(),
            date: "2026-10-18 20:14 UTC".into(),
            watched: "0:43:20".into(),
            completed: true,
            rewatch: false,
        }]))
        .into(),
    );
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let phase = Rc::new(std::cell::Cell::new(0));
    let step = phase.clone();
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(700),
        move || {
            let app = weak.upgrade().unwrap();
            let names = [
                "queue-desktop",
                "journal-desktop",
                "queue-phone",
                "journal-phone",
            ];
            let pixels = app.window().take_snapshot().unwrap();
            if let Some(dir) = std::env::var_os("NOVA_FEATURE_PREVIEW_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(
                    std::path::PathBuf::from(dir).join(format!("{}.png", names[step.get()])),
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            step.set(step.get() + 1);
            match step.get() {
                1 | 3 => {
                    app.global::<nova::QueueBridge>().set_open(false);
                    app.global::<nova::JournalBridge>().set_open(true);
                }
                2 => {
                    app.window().set_size(slint::PhysicalSize::new(390, 844));
                    app.global::<nova::JournalBridge>().set_open(false);
                    app.global::<nova::QueueBridge>().set_open(true);
                }
                _ => {
                    slint::quit_event_loop().unwrap();
                }
            }
        },
    );
    app.run().unwrap();
}

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, Model, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(value: &str) -> SharedString {
    SharedString::from(value)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

fn max_right_edge(app: &nova::AppWindow) -> f32 {
    ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .map(|element| {
            let position = element.absolute_position();
            let size = element.size();
            position.x + size.width
        })
        .fold(0.0, f32::max)
}

fn stream_row(id: &str, text: &str, details: &str, is_download: bool) -> nova::StreamRow {
    nova::StreamRow {
        id: s(id),
        text: s(text),
        details: s(details),
        lines: if details.is_empty() { 2 } else { 3 },
        is_download,
        download_progress: if is_download { 0.42 } else { 0.0 },
        download_action: if is_download { 1 } else { 0 },
    }
}

#[test]
fn pinned_download_and_stream_actions_fit_and_dispatch() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    app.set_modal_visible(true);
    app.set_detail_tab(0);
    app.set_modal_episodes(false);
    app.set_selected_title(s("Movie"));
    app.set_selected_description(s(""));
    app.set_touch_menus(true);
    app.set_streams(
        Rc::new(VecModel::from(vec![
            stream_row(
                "download:job-1",
                "Torrentio\nMovie 2160p\n[Tracker] 1080p",
                "Downloading · 420 MiB / 1.0 GiB · 2.0 MiB/s",
                true,
            ),
            stream_row("stream-1", "Addon B\nMovie 1080p", "", false),
        ]))
        .into(),
    );
    app.set_stream_action_title(s("Movie 2160p · Torrentio"));
    app.set_stream_action_items(
        Rc::new(VecModel::from(vec![
            nova::SheetItem {
                label: s("Pause download"),
                enabled: true,
            },
            nova::SheetItem {
                label: s("Remove download"),
                enabled: true,
            },
        ]))
        .into(),
    );

    let requested: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected: Rc<RefCell<Vec<(String, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    let app_weak = app.as_weak();
    let requested2 = requested.clone();
    app.on_stream_action_requested(move |id| {
        requested2.borrow_mut().push(id.to_string());
        if let Some(app) = app_weak.upgrade() {
            app.set_stream_action_open(true);
        }
    });
    let selected2 = selected.clone();
    app.on_stream_action_selected(move |id, action| {
        selected2.borrow_mut().push((id.to_string(), action));
    });

    let app1 = app.as_weak();
    let requested1 = requested.clone();
    let selected1 = selected.clone();
    after(250, move || {
        let app = app1.upgrade().unwrap();
        assert!(app.get_streams().row_data(0).unwrap().is_download);
        assert_eq!(
            app.get_streams().row_data(0).unwrap().id,
            s("download:job-1")
        );
        assert!(max_right_edge(&app) <= 361.0);
        assert_eq!(app.get_stream_action_items().row_count(), 2);

        let row = ElementHandle::find_by_element_type_name(&app, "TouchArea")
            .into_iter()
            .find(|element| {
                let position = element.absolute_position();
                let size = element.size();
                size.width > 300.0 && size.height >= 48.0 && position.x < 40.0 && position.y > 100.0
            })
            .expect("stream row hit area");
        let row_position = row.absolute_position();
        let row_size = row.size();
        let position = LogicalPosition::new(
            row_position.x + row_size.width / 2.0,
            row_position.y + row_size.height / 2.0,
        );
        let _ =
            app.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });

        let app2 = app.as_weak();
        let requested2 = requested1.clone();
        let selected2 = selected1.clone();
        after(700, move || {
            let app = app2.upgrade().unwrap();
            let _ = app.window().dispatch_event_with_result(
                slint::platform::WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
            );
            assert_eq!(
                requested2.borrow().as_slice(),
                &[s("download:job-1").to_string()]
            );
            assert!(app.get_stream_action_open());
            app.invoke_stream_action_selected(s("download:job-1").into(), 1);
            assert_eq!(
                selected2.borrow().as_slice(),
                &[(s("download:job-1").to_string(), 1)]
            );
            slint::quit_event_loop().unwrap();
        });
    });

    slint::run_event_loop().unwrap();
    assert!(requested.borrow().len() == 1);
}

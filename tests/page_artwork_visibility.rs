//! Page delegates report the retained viewport without an arbitrary row limit.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, VecModel};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

fn settle(app: &nova::AppWindow) {
    // Force layout on mock-renderer tests, then process changed bindings and
    // the retained delegates' retry tick. No native rasterizer is required.
    for _ in 0..2 {
        for component in ["ReleaseCard", "DownloadSeriesCard"] {
            for element in ElementHandle::find_by_element_type_name(app, component) {
                let _ = element.absolute_position();
                let _ = element.size();
            }
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1600));
    }
}

#[test]
fn calendar_and_downloads_report_rows_after_eighty_and_drop_offscreen_retention() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    for downloads in [false, true] {
        for width in [390, 1280] {
            let app = nova::AppWindow::new().unwrap();
            app.global::<nova::Anim>().set_enabled(false);
            app.global::<nova::Anim>().set_transitions(false);
            app.set_show_home(false);
            let retained = Rc::new(RefCell::new(HashMap::<String, bool>::new()));
            let recorded = retained.clone();
            let inactive = Rc::new(RefCell::new(Vec::new()));
            let recorded_active = inactive.clone();
            if downloads {
                app.on_downloads_page_artwork_visible(move |key, keep| {
                    recorded.borrow_mut().insert(key.to_string(), keep);
                });
                app.on_downloads_page_artwork_active_changed(move |active| {
                    recorded_active.borrow_mut().push(active);
                });
                app.set_downloads_page_counts(Rc::new(VecModel::from(vec![150, 150, 0, 0])).into());
                app.set_downloads_page_groups(
                    Rc::new(VecModel::from(
                        (0..150)
                            .map(|index| nova::DownloadGroup {
                                id: format!("row-{index}").into(),
                                title: format!("Series {index}").into(),
                                artwork_identity: format!("url-recipe-{index}").into(),
                                collapsed: true,
                                ..Default::default()
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                );
                app.set_show_downloads(true);
            } else {
                app.on_calendar_artwork_visible(move |key, keep| {
                    recorded.borrow_mut().insert(key.to_string(), keep);
                });
                app.on_calendar_artwork_active_changed(move |active| {
                    recorded_active.borrow_mut().push(active);
                });
                app.set_calendar_view(2);
                app.set_calendar_rows(
                    Rc::new(VecModel::from(
                        (0..150)
                            .map(|index| nova::CalendarRelease {
                                key: format!("row-{index}").into(),
                                title: format!("Release {index}").into(),
                                artwork_identity: format!("url-recipe-{index}").into(),
                                ..Default::default()
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                );
                app.set_show_calendar(true);
            }
            app.window().set_size(slint::PhysicalSize::new(width, 900));
            app.window().show().unwrap();
            settle(&app);
            let first = if downloads { "row-0" } else { "rrow-0" };
            let last = if downloads { "row-149" } else { "rrow-149" };
            assert_eq!(
                retained.borrow().get(first),
                Some(&true),
                "initial viewport, downloads={downloads}, width={width}"
            );
            assert_ne!(retained.borrow().get(last), Some(&true));
            app.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(width as f32 / 2.0, 700.0),
                    delta_x: 0.0,
                    delta_y: -25000.0,
                });
            settle(&app);
            assert_eq!(
                retained.borrow().get(last),
                Some(&true),
                "scroll must retain later rows, downloads={downloads}, width={width}"
            );
            assert_eq!(
                retained.borrow().get(first),
                Some(&false),
                "scrolled-away artwork must be released"
            );
            assert!(
                retained.borrow().values().filter(|keep| **keep).count() < 40,
                "retention stays bounded by viewport geometry"
            );
            // Detail/player/More remove the page without clearing its
            // destination flag. The retained model must become eligible again
            // when that component is recreated on overlay exit.
            for layer in 0..3 {
                match layer {
                    0 => app.set_modal_visible(true),
                    1 => app.set_player_open(true),
                    _ => app.set_more_open(true),
                }
                settle(&app);
                assert_eq!(inactive.borrow().last(), Some(&false));
                match layer {
                    0 => app.set_modal_visible(false),
                    1 => app.set_player_open(false),
                    _ => app.set_more_open(false),
                }
                settle(&app);
                assert_eq!(inactive.borrow().last(), Some(&true));
                assert!(retained.borrow().values().any(|keep| *keep));
            }
            if downloads {
                app.set_show_downloads(false);
            } else {
                app.set_show_calendar(false);
            }
            settle(&app);
            assert_eq!(
                inactive.borrow().last(),
                Some(&false),
                "removing the page must explicitly invalidate its artwork"
            );
        }
    }
}

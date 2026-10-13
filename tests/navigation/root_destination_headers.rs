//! Root menu headings share Discover's unpadded title lane across the rail breakpoint.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use std::time::Duration;

fn idle() {
    for _ in 0..2 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    }
}
fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id).next().expect(id)
}
#[test]
fn root_headings_align_with_discover_without_nested_header_padding() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "root_headings_align_with_discover_without_nested_header_padding"
    )) {
        return;
    }
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_downloads_page_summary("20 downloads · 10 series".into());
    app.window().show().unwrap();
    for width in [699, 700, 1280] {
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.invoke_navigate_section(1);
        idle();
        let reference = element(&app, "DiscoverHeader::hero_title");
        let title_y = reference.absolute_position().y;
        let page_left = reference.absolute_position().x;
        assert_eq!(reference.size().height, 44.0);
        for section in [3, 4, 5] {
            app.invoke_navigate_section(section);
            idle();
            let title = element(&app, "PageHeader::title_text");
            let p = title.absolute_position();
            assert!(
                (p.y - title_y).abs() < 1.0,
                "section {section} at {width}: title top matches Discover"
            );
            assert!(
                (p.x - page_left - if width < 700 { 56.0 } else { 0.0 }).abs() < 1.0,
                "section {section} at {width}: only narrow Back offsets the title"
            );
            assert_eq!(title.size().height, 44.0);
            assert_eq!(
                ElementHandle::find_by_accessible_label(&app, "Back").count(),
                usize::from(width < 700)
            );
            if section != 3 {
                let caption = element(&app, "PageHeader::description");
                assert!((caption.absolute_position().y - p.y - 52.0).abs() < 1.0);
            }
        }
    }
}

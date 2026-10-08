//! Hero logos preserve aspect ratio and text titles remain usable as fallback.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::time::Duration;

fn element(app: &nova::AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_element_id(app, id)
        .next()
        .unwrap_or_else(|| panic!("missing visible element: {id}"))
}

#[test]
fn home_and_detail_title_logos_fit_and_fall_back_to_text() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.global::<nova::Anim>().set_enabled(false);
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(600, 120);
    for pixel in pixels.make_mut_bytes().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[255, 255, 255, 128]);
    }
    let logo = Image::from_rgba8(pixels);
    app.set_show_home(true);
    app.set_home_featured_title("A title that remains available".into());
    app.set_home_featured_count(1);
    app.set_home_featured_logo(logo.clone());
    app.set_home_featured_revision(1);
    app.window().show().unwrap();
    for width in [1400, 390] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        let image = element(&app, "TitleArtwork::title_logo");
        let size = image.size();
        assert!(size.width > 0.0 && size.height > 0.0);
        assert!((size.width / size.height - 5.0).abs() < 0.1);
        assert!(size.height <= 113.0);
        app.set_home_featured_logo(Image::default());
        app.set_home_featured_revision(app.get_home_featured_revision() + 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert!(
            ElementHandle::find_by_element_id(&app, "TitleArtwork::title_logo")
                .next()
                .is_none()
        );
        assert!(element(&app, "TitleArtwork::title_text").size().height > 0.0);
        app.set_home_featured_logo(logo.clone());
        app.set_home_featured_revision(app.get_home_featured_revision() + 1);
    }
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_selected_title("Detail title".into());
    for width in [1400, 390] {
        app.window().set_size(slint::PhysicalSize::new(width, 1000));
        app.set_selected_logo(logo.clone());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        let title = element(&app, "DetailPage::detail_title");
        let image = title
            .query_descendants()
            .match_id("TitleArtwork::title_logo")
            .find_first()
            .unwrap();
        assert!((image.size().width / image.size().height - 5.0).abs() < 0.1);
        app.set_selected_logo(Image::default());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        assert!(
            title
                .query_descendants()
                .match_id("TitleArtwork::title_logo")
                .find_first()
                .is_none()
        );
        assert!(title.size().height > 0.0);
    }
}

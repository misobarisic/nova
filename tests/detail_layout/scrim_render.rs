//! Requires a display and the actual desktop GPU renderer: the headless
//! testing backend cannot catch colorized-texture sampling artifacts.
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

#[test]
#[ignore = "requires a display; run with SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn scrolled_episode_backdrop_stays_hidden_across_rotation() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "scrolled_episode_backdrop_stays_hidden_across_rotation"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    app.set_animations(false);
    app.set_anim_transitions(false);
    app.set_is_android(true);
    app.set_touch_menus(true);
    app.set_show_home(false);
    app.set_modal_visible(true);
    app.set_modal_episodes(true);
    app.set_detail_tab(3);
    app.set_selected_title("Repeated rotation".into());
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(32, 32);
    pixels
        .make_mut_slice()
        .fill(Rgba8Pixel::new(255, 255, 255, 255));
    app.set_selected_backdrop(Image::from_rgba8(pixels));
    app.set_episode_rows(
        Rc::new(slint::VecModel::from(
            (0..24)
                .map(|i| nova::EpisodeRow {
                    id: format!("episode-{i}").into(),
                    text: format!("Episode {}", i + 1).into(),
                    ep_no: format!("S1 E{}", i + 1).into(),
                    details: "The episode list must remain above a uniform canvas after rotation."
                        .into(),
                    watched: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_total(24);
    nova_ui::apply_theme(&app, false);
    app.window().set_size(slint::PhysicalSize::new(390, 844));
    app.window().show().unwrap();
    let directory =
        std::env::var_os("NOVA_DETAIL_ROTATION_PREVIEW_DIR").map(std::path::PathBuf::from);
    if let Some(directory) = &directory {
        std::fs::create_dir_all(directory).unwrap();
    }
    let timer = slint::Timer::default();
    let mut stage = 0;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        if stage == 0 {
            (*app).global::<nova_ui_shared::DetailContent>().invoke_restore_scroll_y(-1800.0);
            stage += 1;
            return;
        }
        let snapshot = app.window().take_snapshot().unwrap();
        if let Some(directory) = &directory {
            image::save_buffer(directory.join(format!("rotation-{stage}.png")), snapshot.as_bytes(), snapshot.width(), snapshot.height(), image::ColorType::Rgba8).unwrap();
        }
        let offset = (*app).global::<nova_ui_shared::DetailContent>().invoke_scroll_y();
        assert!(offset < -600.0, "rotation lost the scrolled page: stage {stage}, offset {offset}");
        let expected: [u8; 3] = if stage >= 7 { [0, 0, 0] } else { [8, 15, 25] };
        let scale = app.window().scale_factor();
        let x = (5.0 * scale) as usize;
        for y in [180.0, 240.0, 300.0] {
            let y = (y * scale) as usize;
            let pixel = snapshot.as_slice()[y * snapshot.width() as usize + x];
            assert!(pixel.r.abs_diff(expected[0]) <= 2 && pixel.g.abs_diff(expected[1]) <= 2 && pixel.b.abs_diff(expected[2]) <= 2,
                "backdrop leaked after rotation: stage {stage}, offset {offset}, pixel {pixel:?}, expected {expected:?}");
        }
        if stage == 12 { slint::quit_event_loop().unwrap(); return; }
        if stage == 6 { nova_ui::apply_theme(&app, true); }
        let portrait = stage % 2 == 0;
        let (width, height) = if portrait { (390, 844) } else { (844, 390) };
        app.window().set_size(slint::PhysicalSize::new(width, height));
        i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
            if portrait { i_slint_core::lengths::LogicalEdges::new(24.0, 0.0, 20.0, 0.0) }
            else { i_slint_core::lengths::LogicalEdges::new(0.0, 24.0, 0.0, 28.0) },
        );
        stage += 1;
    });
    slint::run_event_loop().unwrap();
}

#[test]
#[ignore = "requires a display; run with SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn hero_fades_stay_continuous_across_resize_and_return_from_detail() {
    if !crate::isolation::run(concat!(
        module_path!(),
        "::",
        "hero_fades_stay_continuous_across_resize_and_return_from_detail"
    )) {
        return;
    }
    let app = nova::AppWindow::new().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_modal_visible(true);
    app.set_selected_title("Sampling fixture".into());
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(32, 32);
    pixels
        .make_mut_slice()
        .fill(Rgba8Pixel::new(255, 255, 255, 255));
    let backdrop = Image::from_rgba8(pixels);
    app.set_selected_backdrop(backdrop.clone());
    app.set_show_home(true);
    app.set_home_featured_title("Sampling fixture".into());
    app.set_home_featured_count(1);
    app.set_home_featured_backdrop(backdrop);
    app.set_home_featured_revision(1);
    app.window().show().unwrap();

    // Include adjacent odd/even widths and common fractional scaling sizes.
    let widths = [
        390, 700, 701, 900, 1023, 1024, 1025, 1280, 1365, 1535, 1536, 1537, 1919, 1920, 1921, 2560,
    ];
    let index = Rc::new(Cell::new(0));
    let phase = Rc::new(Cell::new(0));
    let pending_frames = Rc::new(Cell::new(0));
    app.window()
        .set_size(slint::PhysicalSize::new(widths[0], 1000));
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(180),
        move || {
            let app = weak.upgrade().unwrap();
            let snapshot = app.window().take_snapshot().unwrap();
            let width = snapshot.width() as usize;
            let height = snapshot.height() as usize;
            let pixels = snapshot.as_slice();
            // The upper-right hero contains only the solid white fixture and
            // fades. The side mask is transparent here, so rows must be uniform
            // and neighboring scanlines must change gradually, without stripes.
            let scale = app.window().scale_factor() as f64;
            let top = (90.0 * scale) as usize;
            let bottom = ((200.0 * scale) as usize).min(height - 1);
            // Native presentation and the deferred entrance timer can lag
            // a remount under build/GPU load. Wait for a painted frame, with
            // a bound so a permanently missing backdrop cannot pass.
            let minimum = if index.get() >= widths.len() { 40 } else { 100 };
            if pixels[top * width + width * 9 / 10].r <= minimum {
                pending_frames.set(pending_frames.get() + 1);
                assert!(pending_frames.get() < 12,
                    "fixture artwork never painted, width {width}, phase {}", phase.get());
                app.window().request_redraw();
                return;
            }
            pending_frames.set(0);
            for y in top..bottom {
                for x in width * 84 / 100..width * 95 / 100 {
                    let p = pixels[y * width + x];
                    for q in [pixels[y * width + x + 1], pixels[(y + 1) * width + x]] {
                        assert!(
                            p.r.abs_diff(q.r) <= 3
                                && p.g.abs_diff(q.g) <= 3
                                && p.b.abs_diff(q.b) <= 3,
                            "stripe at physical width {width}, phase {}, ({x}, {y}): {p:?} -> {q:?}", phase.get()
                        );
                    }
                }
            }
            // Remount both pages, including a live theme change. Checking
            // only a stationary Detail would miss artifacts on Home return.
            let next_phase = (phase.get() + 1) % 4;
            phase.set(next_phase);
            nova_ui::apply_theme(&app, next_phase >= 2);
            if next_phase == 0 {
                let next = index.get() + 1;
                index.set(next);
                if next == widths.len() * 2 {
                    slint::quit_event_loop().unwrap();
                } else {
                    // Repeat with normal entrance fades enabled, sampling
                    // during compositing as well as stationary rendering.
                    app.global::<nova::Anim>().set_enabled(next >= widths.len());
                    app.window()
                        .set_size(slint::PhysicalSize::new(widths[next % widths.len()], 1000));
                }
            }
            app.set_modal_visible(next_phase & 1 == 0);
        },
    );
    slint::run_event_loop().unwrap();
}

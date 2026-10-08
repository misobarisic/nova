//! Requires a display and the actual desktop GPU renderer: the headless
//! testing backend cannot catch colorized-texture sampling artifacts.
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

#[test]
#[ignore = "requires a display; run with SLINT_BACKEND=winit SLINT_RENDERER=femtovg"]
fn hero_fades_stay_continuous_across_resize_and_return_from_detail() {
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

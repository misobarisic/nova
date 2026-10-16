//! Opt-in GPU regression: run under Xvfb with SLINT_BACKEND=winit and
//! SLINT_RENDERER=femtovg. Shaping tests alone cannot catch blank color glyphs.
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    export component EmojiWindow inherits Window {
        width: 480px;
        height: 160px;
        background: #000;
        in-out property <length> emoji-size: 32px;
        in-out property <angle> tilt: 0deg;
        for emoji[index] in ["🇭🇷", "🇬🇧", "🏳️‍🌈", "👩🏽‍💻", "🥰", "🦋"]: Rectangle {
            x: index * 80px;
            width: 80px;
            height: 160px;
            transform-rotation: root.tilt;
            Text {
                text: emoji;
                font-family: "Noto Color Emoji";
                font-size: root.emoji-size;
                color: #fff;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
    }
}

#[test]
#[ignore = "requires a real desktop renderer (Xvfb/Mesa is sufficient)"]
fn colrv1_emoji_paint_at_multiple_sizes_and_transforms() {
    let app = EmojiWindow::new().unwrap();
    nova_ui::initialize_fonts(app.window());
    app.show().unwrap();
    let step = Rc::new(Cell::new(0usize));
    let completed = step.clone();
    let pending = Rc::new(Cell::new(0));
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(120),
        move || {
            let app = weak.upgrade().unwrap();
            let image = app.window().take_snapshot().unwrap();
            let width = image.width() as usize;
            let height = image.height() as usize;
            let mut colored = [0usize; 6];
            for y in 0..height {
                for x in 0..width {
                    let p = image.as_slice()[y * width + x];
                    if p.r.max(p.g).max(p.b).saturating_sub(p.r.min(p.g).min(p.b)) > 30 {
                        colored[(x * 6 / width).min(5)] += 1;
                    }
                }
            }
            // Wait for native presentation, but never pass a permanently blank glyph.
            if colored.iter().any(|&n| n < 8) {
                pending.set(pending.get() + 1);
                assert!(
                    pending.get() < 20,
                    "blank or monochrome emoji at step {}: {colored:?}",
                    step.get()
                );
                app.window().request_redraw();
                return;
            }
            pending.set(0);
            let next = step.get() + 1;
            step.set(next);
            match next {
                1 => app.set_emoji_size(16.0),
                2 => app.set_emoji_size(64.0),
                // Exercise the ephemeral atlas used with non-uniform transforms.
                3 => app.set_tilt(8.0),
                4 => {
                    app.set_tilt(0.0);
                    app.set_emoji_size(32.0);
                }
                _ => {
                    slint::quit_event_loop().unwrap();
                    return;
                }
            }
            app.window().request_redraw();
        },
    );
    slint::run_event_loop().unwrap();
    assert_eq!(completed.get(), 5);
}

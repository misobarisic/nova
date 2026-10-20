//! Opt-in GPU regression: run under Xvfb with SLINT_BACKEND=winit and
//! SLINT_RENDERER=femtovg. Shaping tests alone cannot catch blank color glyphs.
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    export component EmojiWindow inherits Window {
        width: 800px;
        height: 240px;
        background: #000;
        default-font-family: "Roboto";
        in-out property <length> emoji-size: 32px;
        in-out property <angle> tilt: 0deg;
        in-out property <int> emoji-weight: 400;
        for emoji[index] in ["🇭🇷", "🇬🇧", "🏳️‍🌈", "👩🏽‍💻", "🥰", "🦋",
                            "🎬", "🎧", "🔊", "🌍", "📁", "💾",
                            "©️", "®️", "‼️", "⁉️", "™️", "#️⃣", "*️⃣", "1️⃣"]: Rectangle {
            x: Math.mod(index, 10) * 80px;
            y: Math.floor(index / 10) * 120px;
            width: 80px;
            height: 120px;
            transform-rotation: root.tilt;
            Text {
                text: emoji;
                font-size: root.emoji-size;
                font-weight: root.emoji-weight;
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
            let mut colored = [0usize; 20];
            let mut visible = [0usize; 20];
            for y in 0..height {
                for x in 0..width {
                    let p = image.as_slice()[y * width + x];
                    let slot = (y * 2 / height).min(1) * 10 + (x * 10 / width).min(9);
                    if p.r.max(p.g).max(p.b) > 30 {
                        visible[slot] += 1;
                    }
                    if p.r.max(p.g).max(p.b).saturating_sub(p.r.min(p.g).min(p.b)) > 30 {
                        colored[slot] += 1;
                    }
                }
            }
            // Wait for native presentation, but never pass a permanently blank glyph.
            // Some Noto symbols are intentionally gray. All cells must paint;
            // the original six colorful fixtures must also retain their hue.
            if colored[..6].iter().any(|&n| n < 8) || visible.iter().any(|&n| n < 8) {
                pending.set(pending.get() + 1);
                assert!(
                    pending.get() < 20,
                    "blank or monochrome emoji at step {}: color={colored:?}, visible={visible:?}",
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
                    app.set_emoji_weight(700);
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

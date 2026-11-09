//! Picture geometry chosen from the player's Picture menu: scaling mode,
//! aspect-ratio override, zoom and rotation. mpv keeps these as global
//! options, but the player re-applies them before every load so a freshly
//! created mpv instance (new episode, decoder switch) shows the same picture.

use libmpv2::Mpv;

/// Aspect overrides offered by the menu, in its order. Index 0 follows the
/// video's own aspect.
const ASPECTS: [&str; 7] = ["-1", "16:9", "4:3", "21:9", "2.39:1", "1.85:1", "1:1"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Picture {
    /// 0 fit, 1 fill (crop the edges), 2 stretch, 3 original size.
    pub scale: u8,
    pub aspect: u8,
    /// Linear zoom factor, 1.0 = 100%.
    pub zoom: f32,
    /// Clockwise quarter turns.
    pub rotate: u8,
}

impl Default for Picture {
    fn default() -> Self {
        Self { scale: 0, aspect: 0, zoom: 1.0, rotate: 0 }
    }
}

impl Picture {
    pub(crate) fn new(scale: i32, aspect: i32, zoom: f32, rotate: i32) -> Self {
        Self {
            scale: scale.clamp(0, 3) as u8,
            aspect: aspect.clamp(0, ASPECTS.len() as i32 - 1) as u8,
            zoom: if zoom.is_finite() { zoom.clamp(0.5, 3.0) } else { 1.0 },
            rotate: rotate.rem_euclid(4) as u8,
        }
    }

    pub(crate) fn apply(&self, mpv: &Mpv) {
        // Fill crops to cover the window (panscan); Stretch drops the aspect
        // lock; Original size shows the video unscaled.
        let _ = mpv.set_property("keepaspect", self.scale != 2);
        let _ = mpv.set_property("panscan", if self.scale == 1 { 1.0_f64 } else { 0.0 });
        let _ = mpv.set_property("video-unscaled", if self.scale == 3 { "yes" } else { "no" });
        let _ = mpv.set_property("video-aspect-override", ASPECTS[self.aspect as usize]);
        // mpv's zoom is a power of two.
        let _ = mpv.set_property("video-zoom", (self.zoom as f64).log2());
        let _ = mpv.set_property("video-rotate", i64::from(self.rotate) * 90);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_values_are_clamped_into_supported_options() {
        let picture = Picture::new(9, -2, f32::NAN, -1);
        assert_eq!(picture, Picture { scale: 3, aspect: 0, zoom: 1.0, rotate: 3 });
        assert_eq!(Picture::new(1, 4, 5.0, 6).zoom, 3.0);
        assert_eq!(Picture::new(1, 4, 5.0, 6).rotate, 2);
    }
}

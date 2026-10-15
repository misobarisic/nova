//! Temporary OSD clearance, preserving the user's base position and ASS styles.
use libmpv2::Mpv;

#[derive(Default)]
pub(super) struct SubtitlePosition {
    clearance: f64,
    opacity: f64,
    base: Option<f64>,
    applied: Option<f64>,
}

fn unit(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn position(base: f64, clearance: f64, opacity: f64) -> f64 {
    let target = base.min(100.0 * (1.0 - unit(clearance)));
    base + (target - base) * unit(opacity)
}

fn canvas_clearance(clearance: f64, height: f64, top: f64, bottom: f64) -> f64 {
    let content = height - top - bottom;
    if !height.is_finite()
        || !top.is_finite()
        || !bottom.is_finite()
        || height <= 0.0
        || content <= 0.0
        || top < 0.0
        || bottom < 0.0
    {
        return unit(clearance);
    }
    unit(1.0 - (height * (1.0 - unit(clearance)) - top) / content)
}

fn effective_clearance(mpv: &Mpv, clearance: f64) -> f64 {
    let native_ass = mpv
        .get_property::<String>("current-tracks/sub/codec")
        .is_ok_and(|codec| matches!(codec.as_str(), "ass" | "ssa"));
    let override_mode = mpv
        .get_property::<String>("sub-ass-override")
        .unwrap_or_default();
    let uses_margins = if native_ass && !matches!(override_mode.as_str(), "force" | "strip") {
        mpv.get_property::<bool>("sub-ass-force-margins")
            .unwrap_or(false)
    } else {
        mpv.get_property::<bool>("sub-use-margins").unwrap_or(true)
    };
    if uses_margins {
        return clearance;
    }
    // Authored ASS normally uses the video canvas. In portrait the black
    // border may already clear the controls; don't raise dialogue needlessly.
    canvas_clearance(
        clearance,
        mpv.get_property::<f64>("osd-dimensions/h").unwrap_or(0.0),
        mpv.get_property::<f64>("osd-dimensions/mt").unwrap_or(0.0),
        mpv.get_property::<f64>("osd-dimensions/mb").unwrap_or(0.0),
    )
}

impl SubtitlePosition {
    /// An explicit baseline edit must replace the lifted position even when
    /// its numeric value happens to match the current animated position.
    pub(super) fn set_base(&mut self, mpv: &Mpv, base: f64) {
        self.base = Some(base);
        let _ = mpv.set_property("sub-pos", base);
        self.applied = Some(base);
    }

    pub(super) fn set_clearance(&mut self, clearance: f32, opacity: f32) {
        self.clearance = unit(clearance as f64);
        self.opacity = unit(opacity as f64);
    }

    /// Called before drawing video, including paused frames and resizing.
    /// Do not force ASS overrides: libass moves ordinary bottom dialogue but
    /// retains explicit positions, signs, scrolling and other authored effects.
    pub(super) fn apply(&mut self, mpv: &Mpv) -> libmpv2::Result<()> {
        let current = mpv.get_property::<f64>("sub-pos")?;
        if !current.is_finite() {
            return Ok(());
        }
        if self
            .applied
            .is_none_or(|applied| (current - applied).abs() > 0.01)
        {
            // A config/profile or another control may have changed the base.
            self.base = Some(current);
        }
        let clearance = if self.opacity > 0.0 {
            effective_clearance(mpv, self.clearance)
        } else {
            0.0
        };
        let desired = position(self.base.unwrap_or(current), clearance, self.opacity);
        if (desired - current).abs() > 0.001 {
            mpv.set_property("sub-pos", desired)?;
            // Retain the value accepted by mpv (older builds may round).
            self.applied = Some(mpv.get_property::<f64>("sub-pos")?);
        } else {
            self.applied = Some(current);
        }
        Ok(())
    }

    pub(super) fn restore(&mut self, mpv: &Mpv) {
        if let Some(base) = self.base {
            let _ = mpv.set_property("sub-pos", base);
        }
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearance_preserves_a_higher_base_and_follows_the_fade() {
        assert_eq!(position(100.0, 0.25, 1.0), 75.0);
        assert_eq!(position(100.0, 0.25, 0.5), 87.5);
        assert_eq!(position(100.0, 0.25, 0.0), 100.0);
        assert_eq!(position(70.0, 0.25, 1.0), 70.0);
        assert_eq!(position(110.0, 0.25, 0.0), 110.0);
        assert_eq!(position(110.0, 0.25, 0.5), 92.5);
        assert_eq!(position(100.0, f64::NAN, 1.0), 100.0);
        assert_eq!(position(100.0, 0.25, f64::INFINITY), 100.0);
        assert_eq!(canvas_clearance(0.25, 720.0, 0.0, 0.0), 0.25);
        assert_eq!(canvas_clearance(0.25, 720.0, 180.0, 180.0), 0.0);
        assert_eq!(canvas_clearance(0.1, 844.0, 300.0, 300.0), 0.0);
        assert_eq!(canvas_clearance(0.25, 0.0, 0.0, 0.0), 0.25);
    }

    #[test]
    fn native_position_restores_and_respects_later_profile_changes() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        mpv.set_property("sub-pos", 95.0_f64).unwrap();
        let style = mpv.get_property::<String>("sub-ass-override").unwrap();
        let mut lift = SubtitlePosition::default();
        lift.set_clearance(0.25, 1.0);
        lift.apply(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 75.0);
        lift.set_clearance(0.25, 0.0);
        lift.apply(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 95.0);
        mpv.set_property("sub-pos", 65.0_f64).unwrap();
        lift.set_clearance(0.25, 1.0);
        lift.apply(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 65.0);
        // Editing the baseline while lifted must not treat the lifted value
        // as the next baseline, even when both happen to equal 75.
        lift.set_base(&mpv, 95.0);
        lift.apply(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 75.0);
        lift.set_base(&mpv, 75.0);
        lift.set_clearance(0.25, 0.0);
        lift.apply(&mpv).unwrap();
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 75.0);
        lift.restore(&mpv);
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), 75.0);
        assert_eq!(
            mpv.get_property::<String>("sub-ass-override").unwrap(),
            style
        );
    }
}

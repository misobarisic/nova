//! Device listening preferences. Only our labeled filter is ever replaced.
use libmpv2::Mpv;
use nova_ui::backend_text::tr;

const LABEL: &str = "nova-listening";

/// No source analysis: this is speech-range EQ, real-time leveling and
/// compression. Neither voice isolation nor measured integrated-LUFS mastering.
pub(crate) fn graph(effects: [bool; 3]) -> Option<String> {
    if !effects.into_iter().any(|enabled| enabled) {
        return None;
    }
    // Leave headroom before the speech EQ. Leveling/compressor makeup can add
    // gain, so every enabled combination ends with a non-auto-leveling limiter.
    let mut filters = vec!["volume=0.5"];
    if effects[0] {
        filters.push("equalizer=f=2000:t=q:w=1:g=4");
    }
    if effects[1] {
        filters.push("dynaudnorm=f=150:g=9:p=0.8:m=3:r=0.18");
    }
    if effects[2] {
        filters.push("acompressor=threshold=0.125:ratio=4:attack=10:release=250:makeup=1.5");
    }
    filters.push("alimiter=limit=0.89:level=false");
    Some(format!("@{LABEL}:lavfi=[{}]", filters.join(",")))
}

#[derive(Default)]
pub(crate) struct AudioProcessing {
    desired: [bool; 3],
    attempted: bool,
    active: bool,
    audio_format: String,
    pub(crate) status: String,
}

impl AudioProcessing {
    pub(crate) fn configure(&mut self, settings: &nova_config::CacheSettings) {
        let desired = [
            settings.dialogue_boost,
            settings.loudness_normalization,
            settings.night_mode,
        ];
        if desired != self.desired {
            self.desired = desired;
            self.reset();
        }
    }

    pub(crate) fn reset(&mut self) {
        self.attempted = false;
        self.active = false;
        self.audio_format.clear();
        self.status = if graph(self.desired).is_some() {
            tr("Audio processing will apply when built-in playback has audio.")
        } else {
            tr("Audio processing is off.")
        }
        .to_owned();
    }

    fn owned_filter(mpv: &Mpv) -> Option<bool> {
        use libmpv2::{mpv_format, mpv_format::Node};
        use libmpv2_sys::{mpv_node, mpv_node__bindgen_ty_1};
        use std::ffi::CStr;
        let mut root = mpv_node {
            format: mpv_format::None,
            u: mpv_node__bindgen_ty_1 { int64: 0 },
        };
        // Unlike audio-params, af's STRING representation is mpv option syntax,
        // not JSON. Read the native node to inspect exact labels and enabled.
        // SAFETY: mpv initializes the node; all returned pointers remain owned
        // by it until mpv_free_node_contents below. Formats guard union reads.
        unsafe {
            if libmpv2_sys::mpv_get_property(
                mpv.ctx.as_ptr(),
                c"af".as_ptr(),
                Node,
                (&mut root as *mut mpv_node).cast(),
            ) < 0
            {
                return None;
            }
            let mut found = None;
            if root.format == mpv_format::Array && !root.u.list.is_null() {
                let list = &*root.u.list;
                for i in 0..list.num as usize {
                    let filter = &*list.values.add(i);
                    if filter.format != mpv_format::Map || filter.u.list.is_null() {
                        continue;
                    }
                    let fields = &*filter.u.list;
                    let mut owned = false;
                    let mut enabled = true;
                    for j in 0..fields.num as usize {
                        let key = CStr::from_ptr(*fields.keys.add(j)).to_bytes();
                        let value = &*fields.values.add(j);
                        if key == b"label" && value.format == mpv_format::String {
                            owned = CStr::from_ptr(value.u.string).to_bytes() == LABEL.as_bytes();
                        } else if key == b"enabled" && value.format == mpv_format::Flag {
                            enabled = value.u.flag != 0;
                        }
                    }
                    if owned {
                        found = Some(enabled);
                        break;
                    }
                }
            }
            libmpv2_sys::mpv_free_node_contents(&mut root);
            found
        }
    }

    fn remove(mpv: &Mpv) -> libmpv2::Result<()> {
        if Self::owned_filter(mpv).is_some() {
            mpv.command("af", &["remove", "@nova-listening"])?;
        }
        Ok(())
    }

    fn failed(&mut self, mpv: &Mpv, detail: impl std::fmt::Display) {
        self.active = false;
        let cleanup = Self::remove(mpv);
        self.status = tr(
            "Audio processing is unavailable with this player's audio filters. Effects are disabled.",
        ).to_owned();
        if let Err(error) = cleanup {
            self.status =
                tr("Audio processing could not be disabled. Stop playback and try again.")
                    .to_owned();
            eprintln!("nova-player: audio filter cleanup failed: {error}");
        }
        eprintln!("nova-player: audio processing failed: {detail}");
    }

    fn install(&mut self, mpv: &Mpv, filter: &str) {
        // mpv replaces a matching label, keeping unrelated user filters.
        if let Err(error) = mpv.command("af", &["add", filter]) {
            self.failed(mpv, error);
            return;
        }
        if Self::owned_filter(mpv) != Some(true) {
            self.failed(mpv, "filter was not enabled");
            return;
        }
        self.active = true;
        self.status = tr("Audio processing is active on this device.").to_owned();
    }

    /// Called immediately on edits and on the native-player tick. Wait for
    /// actual audio before testing the graph: idle mpv accepts filter syntax
    /// without necessarily constructing the FFmpeg filters in that build.
    pub(crate) fn poll(&mut self, mpv: &Mpv) {
        let Some(filter) = graph(self.desired) else {
            if !self.attempted {
                self.attempted = true;
                if let Err(error) = Self::remove(mpv) {
                    self.failed(mpv, error);
                    return;
                }
                self.status = tr("Audio processing is off.").to_owned();
            }
            return;
        };
        let format = mpv
            .get_property::<String>("audio-params")
            .unwrap_or_default();
        if format.is_empty() || format == "null" || format == "{}" {
            if self.active {
                self.reset();
            }
            return;
        }
        if format != self.audio_format {
            self.audio_format = format;
            self.attempted = false;
        }
        if !self.attempted {
            self.attempted = true;
            self.install(mpv, &filter);
        } else if self.active && Self::owned_filter(mpv) != Some(true) {
            // A format/track change may cause a delayed native failure.
            self.failed(mpv, "filter became unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpv() -> Mpv {
        Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap()
    }

    #[test]
    fn idle_player_defers_filter_validation_and_does_not_claim_success() {
        let mpv = mpv();
        let mut processing = AudioProcessing::default();
        processing.configure(&nova_config::CacheSettings {
            dialogue_boost: true,
            ..Default::default()
        });
        processing.poll(&mpv);
        assert!(!processing.attempted);
        assert_eq!(AudioProcessing::owned_filter(&mpv), None);
        assert_eq!(
            processing.status,
            tr("Audio processing will apply when built-in playback has audio.")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_changes_reset_and_native_failures_preserve_other_filters_and_source() {
        use std::time::{Duration, Instant};
        let mpv = mpv();
        // Native PCM fixture avoids network, audio hardware and video renderers.
        let path = std::env::temp_dir().join(format!("nova-listening-{}.wav", std::process::id()));
        let samples = 48_000_u32 * 2;
        let data_len = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&96_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(wav.len() + data_len as usize, 0);
        std::fs::write(&path, wav).unwrap();
        mpv.set_property("pause", true).unwrap();
        mpv.command("loadfile", &[path.to_str().unwrap()]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while mpv.get_property::<String>("audio-params").is_err() {
            assert!(Instant::now() < deadline, "PCM audio did not load");
            std::thread::sleep(Duration::from_millis(10));
        }
        let source = mpv.get_property::<String>("path").unwrap();
        mpv.command("af", &["add", "@other:lavfi=[volume=0.9]"])
            .unwrap();
        let mut processing = AudioProcessing::default();
        processing.configure(&nova_config::CacheSettings {
            dialogue_boost: true,
            loudness_normalization: true,
            night_mode: true,
            ..Default::default()
        });
        processing.poll(&mpv);
        assert_eq!(AudioProcessing::owned_filter(&mpv), Some(true));
        assert_eq!(
            processing.status,
            tr("Audio processing is active on this device.")
        );
        processing.reset(); // source/renderer recovery reapplies one labeled filter
        processing.poll(&mpv);
        let filters = mpv.get_property::<String>("af").unwrap();
        assert_eq!(filters.matches("@nova-listening:").count(), 1);
        assert!(filters.contains("@other:"));
        processing.configure(&Default::default());
        processing.poll(&mpv);
        assert_eq!(AudioProcessing::owned_filter(&mpv), None);
        assert!(mpv.get_property::<String>("af").unwrap().contains("other"));
        processing.configure(&nova_config::CacheSettings {
            night_mode: true,
            ..Default::default()
        });
        processing.poll(&mpv);
        assert_eq!(AudioProcessing::owned_filter(&mpv), Some(true));
        processing.install(&mpv, "@nova-listening:lavfi=[nova_missing_filter]");
        assert_eq!(AudioProcessing::owned_filter(&mpv), None);
        assert!(processing.status.contains("unavailable"));
        let filters = mpv.get_property::<String>("af").unwrap();
        assert!(filters.contains("other"));
        processing.configure(&Default::default());
        processing.poll(&mpv);
        assert_eq!(AudioProcessing::owned_filter(&mpv), None);
        assert!(mpv.get_property::<String>("af").unwrap().contains("other"));
        assert_eq!(mpv.get_property::<String>("path").unwrap(), source);
        assert!(mpv.get_property::<bool>("pause").unwrap());
        mpv.command("stop", &[]).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn all_combinations_have_headroom_and_a_final_non_boosting_limiter() {
        assert_eq!(graph([false; 3]), None);
        for bits in 1..8 {
            let effects = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
            let filter = graph(effects).unwrap();
            assert!(filter.starts_with("@nova-listening:lavfi=[volume=0.5,"));
            assert!(filter.ends_with(",alimiter=limit=0.89:level=false]"));
            assert_eq!(filter.contains("equalizer="), effects[0]);
            assert_eq!(filter.contains("dynaudnorm="), effects[1]);
            assert_eq!(filter.contains("acompressor="), effects[2]);
        }
    }
}

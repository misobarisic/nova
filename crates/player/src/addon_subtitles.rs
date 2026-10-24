//! Unloaded addon choices share the native subtitle menu, without downloading
//! every language or assigning persisted meaning to stream-local track IDs.
use super::*;

#[derive(Clone, Debug)]
pub struct OnlineSubtitle {
    pub url: String,
    pub language: String,
    pub title: String,
}

#[cfg(any(target_os = "android", test))]
struct LoadedSubtitle {
    path: String,
    title: String,
    language: String,
    id: i32,
    restore: bool,
    secondary: bool,
}

// Bound retained local paths even if a session repeatedly loads addon choices.
#[cfg(any(target_os = "android", test))]
const LOADED_SUBTITLE_LIMIT: usize = 64;

#[cfg(any(target_os = "android", test))]
#[derive(Default)]
pub(super) struct LoadedSubtitles {
    tracks: std::collections::VecDeque<LoadedSubtitle>,
}

#[cfg(any(target_os = "android", test))]
impl LoadedSubtitles {
    fn register(&mut self, path: &str, title: &str, language: &str, id: i32) {
        self.tracks.retain(|track| track.id != id);
        self.tracks.push_back(LoadedSubtitle {
            path: path.into(),
            title: title.into(),
            language: language.into(),
            id,
            restore: false,
            secondary: false,
        });
        while self.tracks.len() > LOADED_SUBTITLE_LIMIT {
            self.tracks.pop_front();
        }
    }

    #[cfg(test)]
    fn arm(&mut self, selected: i32) -> Option<i32> {
        self.arm_pair(selected, -1)[0]
    }

    fn arm_pair(&mut self, selected: i32, secondary: i32) -> [Option<i32>; 2] {
        // Another teardown can precede the first recovered frame. Its native
        // track list is incomplete, so keep the already captured file identity.
        if self.tracks.iter().any(|track| track.restore) {
            return [false, true].map(|secondary| {
                self.tracks
                    .iter()
                    .find(|track| track.restore && track.secondary == secondary)
                    .map(|track| track.id)
            });
        }
        // Only the selected addon returns after reload. Retiring every other
        // numeric ID prevents it from aliasing a recovered embedded track.
        self.tracks
            .retain(|track| track.id == selected || track.id == secondary);
        for track in &mut self.tracks {
            track.restore = true;
            track.secondary = track.id == secondary;
        }
        [false, true].map(|secondary| {
            self.tracks
                .iter()
                .find(|track| track.secondary == secondary)
                .map(|track| track.id)
        })
    }

    fn restore(
        &mut self,
        mpv: &Mpv,
        sync: &mut subtitle_sync::SubtitleSync,
        primary_delay: f64,
    ) -> Result<bool, String> {
        if !self.tracks.iter().any(|track| track.restore) {
            return Ok(false);
        }
        let primary = mpv
            .get_property::<String>("sid")
            .unwrap_or_else(|_| "no".into());
        let secondary = mpv
            .get_property::<String>("secondary-sid")
            .unwrap_or_else(|_| "no".into());
        let mut mapping = Vec::new();
        for track in self.tracks.iter().filter(|track| track.restore) {
            match add_local_subtitle(mpv, &track.path, &track.title, &track.language) {
                Ok(id) => mapping.push(subtitle_sync::TrackRemap {
                    slot: if track.secondary {
                        subtitle_sync::Slot::Secondary
                    } else {
                        subtitle_sync::Slot::Primary
                    },
                    old: track.id,
                    new: id,
                }),
                Err(error) => {
                    // A partial pair must not consume either old timing record.
                    // Remove only this attempt's new tracks, then retry the same
                    // retained file identities on a later recovered frame.
                    for remap in &mapping {
                        let _ = mpv.command("sub-remove", &[&remap.new.to_string()]);
                    }
                    let _ = mpv.set_property("sid", primary);
                    let _ = mpv.set_property("secondary-sid", secondary);
                    return Err(error);
                }
            }
        }
        let main = mapping
            .iter()
            .find(|remap| remap.slot == subtitle_sync::Slot::Primary);
        let second = mapping
            .iter()
            .find(|remap| remap.slot == subtitle_sync::Slot::Secondary);
        let _ = mpv.set_property("sid", main.map_or(primary, |remap| remap.new.to_string()));
        let _ = mpv.set_property(
            "secondary-sid",
            second.map_or(secondary, |remap| remap.new.to_string()),
        );
        if second.is_some() {
            let _ = mpv.set_property("secondary-sub-visibility", true);
        }
        let restored_primary = main.is_some();
        sync.remap_pair(&mapping);
        // Recovery can finish while paused, when the observation tick is
        // deliberately idle. Restore both independent delays immediately.
        sync.primary_selection_changed(mpv, primary_delay);
        sync.secondary_selection_changed(mpv);
        for track in self.tracks.iter_mut().filter(|track| track.restore) {
            if let Some(remap) = mapping.iter().find(|remap| remap.old == track.id) {
                track.id = remap.new;
                track.restore = false;
            }
        }
        Ok(restored_primary)
    }

    pub(super) fn clear(&mut self) {
        self.tracks.clear();
    }
}

fn add_local_subtitle(mpv: &Mpv, path: &str, title: &str, language: &str) -> Result<i32, String> {
    mpv.command("sub-add", &[path, "select", title, language])
        .map_err(|error| error.to_string())?;
    mpv.get_property::<String>("sid")
        .map_err(|error| error.to_string())?
        .parse::<i32>()
        .map_err(|error| error.to_string())
}

fn automatic_online_choice(
    preferences: &SessionPreferences,
    manual: bool,
    raw: &str,
    subtitles: &[OnlineSubtitle],
) -> Option<i32> {
    use nova_config::playback_preferences::{language_preferences, normalize_language};
    if manual {
        return None;
    }
    let wanted = match &preferences.title.subtitles {
        Some(TrackChoice::Off) => return None,
        Some(TrackChoice::Track(track)) => vec![track.language.clone()?],
        None => language_preferences(&preferences.subtitle_language),
    };
    let tracks = track_preferences::parse_tracks(raw);
    for language in wanted {
        if tracks
            .iter()
            .any(|track| !track.audio && track.descriptor.language.as_deref() == Some(&language))
        {
            return None;
        }
        if let Some(index) = subtitles.iter().position(|subtitle| {
            normalize_language(&subtitle.language).as_deref() == Some(&language)
        }) {
            return Some(-2 - index as i32);
        }
    }
    None
}

impl Player {
    #[cfg(target_os = "android")]
    pub(super) fn arm_online_subtitle_restore(&self) {
        let selected = self.selected_track_id(false);
        let secondary = self
            .state
            .mpv
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|mpv| mpv.get_property::<String>("secondary-sid").ok())
            .and_then(|id| id.parse::<i32>().ok())
            .unwrap_or(-1);
        let restore = self
            .state
            .loaded_online_subtitles
            .lock()
            .unwrap()
            .arm_pair(selected, secondary);
        if let Some(mpv) = self.state.mpv.lock().unwrap().as_ref() {
            self.state
                .subtitle_sync
                .lock()
                .unwrap()
                .suspend_pair(mpv, restore);
        }
    }

    #[cfg(target_os = "android")]
    pub(super) fn restore_online_subtitle(&self, mpv: &Mpv) {
        // The old path/track-list can remain visible during a same-URL reload.
        // Wait for the recovered file to produce frames before adding tracks.
        if !self.state.started.load(Ordering::SeqCst) {
            return;
        }
        let mut loaded = self.state.loaded_online_subtitles.lock().unwrap();
        let primary_delay = self
            .resolve_subtitle_style(
                self.state
                    .preferences
                    .lock()
                    .unwrap()
                    .preferences
                    .subtitle_style(),
            )
            .delay as f64;
        if loaded
            .restore(
                mpv,
                &mut self.state.subtitle_sync.lock().unwrap(),
                primary_delay,
            )
            .unwrap_or(false)
        {
            self.state.preferences.lock().unwrap().manual_subtitles = true;
        }
    }

    /// Keep a loaded track when its native or source-provided language already
    /// satisfies the preference. A viewer's explicit track/Off choice wins.
    pub fn automatic_online_subtitle(&self) -> Option<i32> {
        let preferences = self.state.preferences.lock().unwrap();
        let manual = preferences.manual_subtitles;
        let session = preferences.preferences.clone();
        drop(preferences);
        let mpv = self.state.mpv.lock().unwrap();
        let raw = self.state.track_inventory(mpv.as_ref()?).ok()?;
        automatic_online_choice(
            &session,
            manual,
            &raw,
            &self.state.online_subtitles.lock().unwrap(),
        )
    }

    pub fn set_online_subtitles(&self, subtitles: Vec<OnlineSubtitle>) {
        *self.state.online_subtitles.lock().unwrap() = subtitles;
        self.refresh_tracks();
    }

    pub fn online_subtitle(&self, id: i32) -> Option<OnlineSubtitle> {
        let index = id.checked_neg()?.checked_sub(2)? as usize;
        self.state
            .online_subtitles
            .lock()
            .unwrap()
            .get(index)
            .cloned()
    }

    /// The file is already fetched on a worker. mpv opens only this local file;
    /// adding a subtitle never reloads or reconnects the video source.
    pub fn add_online_subtitle(
        &self,
        path: &str,
        title: &str,
        language: &str,
    ) -> Result<i32, String> {
        self.add_online_subtitle_for_slot(path, title, language, false)
    }

    pub(super) fn add_online_subtitle_for_slot(
        &self,
        path: &str,
        title: &str,
        language: &str,
        secondary: bool,
    ) -> Result<i32, String> {
        if self.state.pending.lock().unwrap().is_some() {
            return Err("video is still loading".into());
        }
        let mpv = self.state.mpv.lock().unwrap();
        let mpv = mpv.as_ref().ok_or("player is unavailable")?;
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return Err("video has changed".into());
        }
        // Keep primary capture, native add and restoration under the same
        // engine lock so Android recovery cannot invalidate a numeric ID.
        let primary = secondary.then(|| {
            mpv.get_property::<String>("sid")
                .unwrap_or_else(|_| "no".into())
        });
        let id = add_local_subtitle(mpv, path, title, language)?;
        if let Some(primary) = primary {
            let _ = mpv.set_property("sid", primary);
        }
        let base = self
            .state
            .preferences
            .lock()
            .unwrap()
            .preferences
            .subtitle_style()
            .delay;
        let mut sync = self.state.subtitle_sync.lock().unwrap();
        sync.register(id);
        sync.primary_selection_changed(mpv, base as f64);
        drop(sync);
        #[cfg(target_os = "android")]
        {
            self.state
                .loaded_online_subtitles
                .lock()
                .unwrap()
                .register(path, title, language, id);
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_caption_language_prevents_replacement_without_blocking_unknown_or_manual_choices() {
        use nova_config::playback_preferences::{SessionPreferences, TitlePreferences};
        let mut preferences = SessionPreferences::from_settings(
            &nova_config::CacheSettings::default(),
            TitlePreferences::default(),
        );
        preferences.subtitle_language = "en".into();
        let choices = vec![
            OnlineSubtitle {
                url: "https://addon.invalid/en".into(),
                language: "en".into(),
                title: "Addon English".into(),
            },
            OnlineSubtitle {
                url: "https://addon.invalid/hr".into(),
                language: "hr".into(),
                title: "Addon Croatian".into(),
            },
        ];
        let raw = r#"[{"id":5,"type":"sub","external":true,"external-filename":"https://included.invalid/eng.vtt?cut=one"}]"#;
        let mut metadata = source_subtitles::SourceSubtitles::default();
        metadata.begin(
            "video",
            1,
            vec![SourceSubtitle {
                url: "https://included.invalid/eng.vtt?cut=one".into(),
                language: Some("eng".into()),
            }],
        );
        let included = metadata.enrich("video", 1, raw.into());
        assert_eq!(
            automatic_online_choice(&preferences, false, &included, &choices),
            None
        );
        assert_eq!(
            automatic_online_choice(&preferences, false, raw, &choices),
            Some(-2),
            "unknown attached captions still allow the preferred addon language"
        );
        assert_eq!(
            automatic_online_choice(&preferences, true, raw, &choices),
            None,
            "viewer picks win"
        );
        preferences.title.subtitles = Some(TrackChoice::Off);
        assert_eq!(
            automatic_online_choice(&preferences, false, raw, &choices),
            None
        );
        preferences.title.subtitles = None;
        preferences.subtitle_language = "hr,en".into();
        assert_eq!(
            automatic_online_choice(&preferences, false, &included, &choices),
            Some(-3),
            "the first preferred language still wins over another included language"
        );
        metadata.begin("video", 2, Vec::new());
        preferences.subtitle_language = "en".into();
        assert_eq!(
            automatic_online_choice(
                &preferences,
                false,
                &metadata.enrich("video", 2, raw.into()),
                &choices
            ),
            Some(-2),
            "a new source cannot inherit old declarations"
        );
    }

    #[test]
    fn repeated_recovery_retains_a_secondary_only_addon() {
        let mut loaded = LoadedSubtitles::default();
        loaded.register("second.srt", "Second language", "hr", 7);
        assert_eq!(loaded.arm_pair(2, 7), [None, Some(7)]);
        assert_eq!(loaded.tracks.len(), 1);
        assert!(loaded.tracks[0].secondary);
        assert!(loaded.tracks[0].restore);
        assert_eq!(loaded.arm_pair(-1, -1), [None, Some(7)]);
        assert_eq!(
            loaded.tracks.len(),
            1,
            "a reload without tracks must retain its pending file identity"
        );
    }

    #[test]
    fn local_subtitle_selection_and_recovery_keep_the_selected_addon() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
                "nova-subtitle-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir(&scratch.0).unwrap();
        let audio = scratch.0.join("audio.wav");
        let subtitle = scratch.0.join("downloaded.sub");
        let size = 80_000u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((size + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(8000u32.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(size.to_le_bytes());
        wav.resize(wav.len() + size as usize, 128);
        std::fs::write(&audio, wav).unwrap();
        std::fs::write(
            &subtitle,
            "1\n00:00:00,000 --> 00:00:05,000\nSubtitle addon test\n",
        )
        .unwrap();
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("pause", true)
        })
        .unwrap();
        mpv.command("loadfile", &[&audio.to_string_lossy()])
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mpv.get_property::<f64>("duration").is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "mpv did not load the fixture"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let path = mpv.get_property::<String>("path").unwrap();
        let id = add_local_subtitle(
            &mpv,
            &subtitle.to_string_lossy(),
            "OpenSubtitles · test",
            "en",
        )
        .unwrap();
        assert!(id > 0);
        assert_eq!(mpv.get_property::<String>("path").unwrap(), path);
        assert!(mpv.get_property::<bool>("pause").unwrap());
        let tracks =
            track_preferences::parse_tracks(&mpv.get_property::<String>("track-list").unwrap());
        assert!(tracks.iter().any(|track| !track.audio
            && track.id == id
            && track.descriptor.language.as_deref() == Some("en")));

        let other = scratch.0.join("other.srt");
        std::fs::write(
            &other,
            "1\n00:00:00,000 --> 00:00:05,000\nAnother addon subtitle\n",
        )
        .unwrap();
        let mut loaded = LoadedSubtitles::default();
        loaded.register(&subtitle.to_string_lossy(), "Addon A", "en", id);
        let other_id = add_local_subtitle(&mpv, &other.to_string_lossy(), "Addon B", "hr").unwrap();
        loaded.register(&other.to_string_lossy(), "Addon B", "hr", other_id);
        assert_ne!(id, other_id);
        let mut sync = subtitle_sync::SubtitleSync::default();
        sync.register(id);
        sync.register(other_id);

        // A -> B -> native A is the regression: the last downloaded file is
        // not the restore target when the viewer reselects an earlier track.
        mpv.set_property("sid", id.to_string()).unwrap();
        let selected = mpv.get_property::<String>("sid").unwrap().parse().unwrap();
        let restore = loaded.arm(selected);
        assert_eq!(restore, Some(id));
        sync.suspend(&mpv, restore);
        mpv.command("loadfile", &[&audio.to_string_lossy()])
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let tracks = mpv.get_property::<String>("track-list").unwrap();
            if mpv.get_property::<f64>("duration").is_ok()
                && !track_preferences::parse_tracks(&tracks)
                    .iter()
                    .any(|track| !track.audio)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "mpv did not reload the fixture"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Occupy the old A numeric ID in the recovered track list. Restoring
        // A must remap its identity instead of treating that number as stable.
        add_local_subtitle(&mpv, &other.to_string_lossy(), "Replacement track", "hr").unwrap();
        assert!(loaded.restore(&mpv, &mut sync, 0.0).unwrap());
        let restored_id = mpv
            .get_property::<String>("sid")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_ne!(restored_id, id);
        assert_eq!(loaded.tracks.len(), 1);
        assert_eq!(loaded.tracks[0].id, restored_id);
        assert_eq!(loaded.tracks[0].path, subtitle.to_string_lossy());
        let tracks: Vec<serde_json::Value> =
            serde_json::from_str(&mpv.get_property::<String>("track-list").unwrap()).unwrap();
        let restored = tracks
            .iter()
            .find(|track| {
                track["id"].as_i64() == Some(restored_id as i64) && track["type"] == "sub"
            })
            .unwrap();
        assert_eq!(
            restored["external-filename"].as_str(),
            Some(subtitle.to_string_lossy().as_ref())
        );
        assert_eq!(restored["title"], "Addon A");
        assert_eq!(restored["lang"], "en");
        assert!(
            !loaded.restore(&mpv, &mut sync, 0.0).unwrap(),
            "restore runs once per reload"
        );
        assert_eq!(loaded.arm(restored_id), Some(restored_id));
        // Both languages selected from addons must survive a renderer reload.
        loaded.clear();
        let primary =
            add_local_subtitle(&mpv, &subtitle.to_string_lossy(), "Primary", "en").unwrap();
        loaded.register(&subtitle.to_string_lossy(), "Primary", "en", primary);
        let secondary = add_local_subtitle(&mpv, &other.to_string_lossy(), "Second", "hr").unwrap();
        loaded.register(&other.to_string_lossy(), "Second", "hr", secondary);
        mpv.set_property("sid", primary.to_string()).unwrap();
        mpv.set_property("secondary-sid", secondary.to_string())
            .unwrap();
        sync.register(primary);
        sync.register(secondary);
        sync.poll(&mpv, 0.0, false);
        mpv.set_property("sub-delay", -1.25_f64).unwrap();
        mpv.set_property("secondary-sub-delay", 2.4_f64).unwrap();
        sync.poll(&mpv, 0.0, false);
        let restore = loaded.arm_pair(primary, secondary);
        assert_eq!(restore, [Some(primary), Some(secondary)]);
        sync.suspend_pair(&mpv, restore);
        assert_eq!(
            loaded.arm_pair(-1, -1),
            restore,
            "a second teardown before recovery keeps both file identities"
        );
        assert_eq!(loaded.tracks.len(), 2);
        mpv.set_property("sub-delay", 0.0_f64).unwrap();
        mpv.set_property("secondary-sub-delay", 0.0_f64).unwrap();
        assert!(loaded.restore(&mpv, &mut sync, 0.0).unwrap());
        assert!(mpv.get_property::<bool>("pause").unwrap());
        // Do not poll: the normal cached-pause guard skips observations, so
        // both corrections must already be applied by the restore itself.
        assert_eq!(mpv.get_property::<f64>("sub-delay").unwrap(), -1.25);
        assert!((mpv.get_property::<f64>("secondary-sub-delay").unwrap() - 2.4).abs() < 1e-6);
        let primary = mpv.get_property::<String>("sid").unwrap();
        let secondary = mpv.get_property::<String>("secondary-sid").unwrap();
        assert_ne!(primary, secondary);
        assert_ne!(primary, "no");
        assert_ne!(secondary, "no");
        assert!(
            mpv.get_property::<bool>("secondary-sub-visibility")
                .unwrap()
        );
        let main_position = mpv.get_property::<f64>("sub-pos").unwrap();
        let main_override = mpv.get_property::<String>("sub-ass-override").unwrap();
        let mut placement = secondary_subtitle_position::Placement::default();
        placement.set_mode(2);
        placement.apply(&mpv);
        assert_eq!(mpv.get_property::<f64>("secondary-sub-pos").unwrap(), 100.0);
        placement.set_mode(1);
        placement.apply(&mpv);
        assert_eq!(mpv.get_property::<f64>("secondary-sub-pos").unwrap(), 0.0);
        assert_eq!(mpv.get_property::<f64>("sub-pos").unwrap(), main_position);
        assert_eq!(
            mpv.get_property::<String>("sub-ass-override").unwrap(),
            main_override
        );
        loaded.clear();
        assert!(loaded.tracks.is_empty());
        assert!(!loaded.restore(&mpv, &mut sync, 0.0).unwrap());
    }

    #[test]
    fn loaded_subtitle_catalog_bounds_paths_and_retires_old_track_ids() {
        let mut loaded = LoadedSubtitles::default();
        for id in 1..=LOADED_SUBTITLE_LIMIT as i32 + 2 {
            loaded.register(&format!("/{id}.srt"), "Addon", "en", id);
        }
        assert_eq!(loaded.tracks.len(), LOADED_SUBTITLE_LIMIT);
        assert_eq!(loaded.tracks.front().unwrap().id, 3);
        loaded.register("/replacement.srt", "Replacement", "hr", 3);
        assert_eq!(loaded.tracks.len(), LOADED_SUBTITLE_LIMIT);
        assert_eq!(loaded.tracks.back().unwrap().path, "/replacement.srt");
        assert_eq!(loaded.arm(3), Some(3));
        assert_eq!(loaded.tracks.len(), 1);
        assert_eq!(
            loaded.arm(-1),
            Some(3),
            "an incomplete recovery keeps its armed file"
        );
        loaded.clear();
        assert_eq!(
            loaded.arm(3),
            None,
            "a closed/new session cannot reuse old paths"
        );
        loaded.register("/later.srt", "Later", "en", 8);
        assert_eq!(
            loaded.arm(-1),
            None,
            "Off/embedded selection must not restore an addon"
        );
        assert!(loaded.tracks.is_empty());
    }
}

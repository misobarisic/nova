//! Unloaded addon choices share the native subtitle menu, without downloading
//! every language or assigning persisted meaning to stream-local track IDs.
use super::*;

#[derive(Clone, Debug)]
pub struct OnlineSubtitle {
    pub url: String,
    pub language: String,
    pub title: String,
}

#[cfg(target_os = "android")]
pub(super) struct LoadedSubtitle {
    path: String,
    title: String,
    language: String,
    id: i32,
    restore: bool,
}

fn add_local_subtitle(mpv: &Mpv, path: &str, title: &str, language: &str) -> Result<i32, String> {
    mpv.command("sub-add", &[path, "select", title, language])
        .map_err(|error| error.to_string())?;
    mpv.get_property::<String>("sid")
        .map_err(|error| error.to_string())?
        .parse::<i32>()
        .map_err(|error| error.to_string())
}

impl Player {
    #[cfg(target_os = "android")]
    pub(super) fn arm_online_subtitle_restore(&self) {
        let selected = self.selected_track_id(false);
        let restore = {
            let mut loaded = self.state.loaded_online_subtitle.lock().unwrap();
            loaded.as_mut().and_then(|subtitle| {
                subtitle.restore = selected == subtitle.id;
                subtitle.restore.then_some(subtitle.id)
            })
        };
        if let Some(mpv) = self.state.mpv.lock().unwrap().as_ref() {
            self.state
                .subtitle_sync
                .lock()
                .unwrap()
                .suspend(mpv, restore);
        }
    }

    #[cfg(target_os = "android")]
    pub(super) fn restore_online_subtitle(&self, mpv: &Mpv) {
        // The old path/track-list can remain visible during a same-URL reload.
        // Wait for the recovered file to produce frames before adding tracks.
        if !self.state.started.load(Ordering::SeqCst) {
            return;
        }
        let mut loaded = self.state.loaded_online_subtitle.lock().unwrap();
        let Some(subtitle) = loaded.as_mut().filter(|subtitle| subtitle.restore) else {
            return;
        };
        if let Ok(id) = add_local_subtitle(mpv, &subtitle.path, &subtitle.title, &subtitle.language)
        {
            self.state
                .subtitle_sync
                .lock()
                .unwrap()
                .remap(subtitle.id, id);
            subtitle.id = id;
            subtitle.restore = false;
            self.state.preferences.lock().unwrap().manual_subtitles = true;
        }
    }

    /// Prefer an embedded track whenever it already satisfies the saved or
    /// global language. A viewer's explicit track/Off choice always wins.
    pub fn automatic_online_subtitle(&self) -> Option<i32> {
        use nova_config::playback_preferences::{language_preferences, normalize_language};
        let preferences = self.state.preferences.lock().unwrap();
        if preferences.manual_subtitles {
            return None;
        }
        let wanted = match &preferences.preferences.title.subtitles {
            Some(TrackChoice::Off) => return None,
            Some(TrackChoice::Track(track)) => vec![track.language.clone()?],
            None => language_preferences(&preferences.preferences.subtitle_language),
        };
        drop(preferences);
        let mpv = self.state.mpv.lock().unwrap();
        let raw = mpv.as_ref()?.get_property::<String>("track-list").ok()?;
        let tracks = track_preferences::parse_tracks(&raw);
        let subtitles = self.state.online_subtitles.lock().unwrap();
        for language in wanted {
            if tracks.iter().any(|track| {
                !track.audio && track.descriptor.language.as_deref() == Some(&language)
            }) {
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
        let id = add_local_subtitle(mpv, path, title, language)?;
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
        let _ = mpv.set_property("sub-delay", sync.delay(mpv, base as f64));
        drop(sync);
        #[cfg(target_os = "android")]
        {
            *self.state.loaded_online_subtitle.lock().unwrap() = Some(LoadedSubtitle {
                path: path.into(),
                title: title.into(),
                language: language.into(),
                id,
                restore: false,
            });
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_subtitle_is_selected_without_reopening_the_media() {
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
    }
}

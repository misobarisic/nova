//! Secondary captions are semantic preferences; native numeric IDs never survive
//! a source switch or renderer recovery. mpv places the second language at top.
use super::*;

impl Player {
    pub fn secondary_subtitle_preference(&self) -> TrackChoice {
        self.state.secondary_subtitles.lock().unwrap().clone()
    }

    pub fn set_secondary_subtitle_preference(&self, choice: TrackChoice) {
        *self.state.secondary_subtitles.lock().unwrap() = choice;
        self.reapply_secondary_subtitle();
        self.refresh_tracks();
    }

    pub fn pick_secondary_subtitle(&self, id: i32) -> Option<TrackChoice> {
        if self.state.pending.lock().unwrap().is_some() {
            return None;
        }
        let engine = self.state.mpv.lock().ok()?;
        let mpv = engine.as_ref()?;
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return None;
        }
        let choice = if id < 0 {
            TrackChoice::Off
        } else {
            if mpv.get_property::<String>("sid").ok().as_deref() == Some(id.to_string().as_str()) {
                return None;
            }
            let raw = mpv.get_property::<String>("track-list").ok()?;
            TrackChoice::Track(
                track_preferences::parse_tracks(&raw)
                    .into_iter()
                    .find(|track| !track.audio && track.id == id)?
                    .descriptor,
            )
        };
        drop(engine);
        self.set_secondary_subtitle_preference(choice.clone());
        Some(choice)
    }

    pub(super) fn resolve_secondary_subtitle(
        &self,
        mpv: &Mpv,
        tracks: &[nova_config::playback_preferences::SemanticTrack],
    ) {
        let choice = self.state.secondary_subtitles.lock().unwrap().clone();
        let primary = mpv
            .get_property::<String>("sid")
            .ok()
            .and_then(|id| id.parse::<i32>().ok());
        let id = secondary_track_id(tracks, &choice, primary);
        apply_secondary_track(
            mpv,
            &mut self.state.subtitle_sync.lock().unwrap(),
            &choice,
            id,
        );
        if !matches!(choice, TrackChoice::Off) {
            self.state
                .secondary_subtitle_position
                .lock()
                .unwrap()
                .apply(mpv);
        }
    }

    pub(super) fn reapply_secondary_subtitle(&self) {
        if self.state.pending.lock().unwrap().is_some() {
            return;
        }
        let engine = self.state.mpv.lock().unwrap();
        let Some(mpv) = engine.as_ref() else {
            return;
        };
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return;
        }
        if let Ok(raw) = mpv.get_property::<String>("track-list") {
            self.resolve_secondary_subtitle(mpv, &track_preferences::parse_tracks(&raw));
        }
    }

    pub fn set_secondary_subtitle_position(&self, index: i32) {
        self.state
            .secondary_subtitle_position
            .lock()
            .unwrap()
            .set_mode(index);
        self.poll_secondary_subtitle_position();
    }

    pub(super) fn poll_secondary_subtitle_position(&self) {
        if self.state.pending.lock().unwrap().is_some()
            || !self.state.started.load(Ordering::SeqCst)
            || matches!(
                &*self.state.secondary_subtitles.lock().unwrap(),
                TrackChoice::Off
            )
        {
            return;
        }
        let engine = self.state.mpv.lock().unwrap();
        let Some(mpv) = engine.as_ref() else {
            return;
        };
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return;
        }
        self.state
            .secondary_subtitle_position
            .lock()
            .unwrap()
            .apply(mpv);
    }

    pub fn automatic_secondary_online_subtitle(&self) -> Option<i32> {
        let TrackChoice::Track(choice) = self.secondary_subtitle_preference() else {
            return None;
        };
        let language = choice.language.as_deref()?;
        let engine = self.state.mpv.lock().unwrap();
        let mpv = engine.as_ref()?;
        let tracks =
            track_preferences::parse_tracks(&mpv.get_property::<String>("track-list").ok()?);
        let primary = mpv
            .get_property::<String>("sid")
            .ok()
            .and_then(|id| id.parse().ok());
        if tracks.iter().any(|track| {
            Some(track.id) == primary && track.descriptor.language.as_deref() == Some(language)
        }) || secondary_track_id(&tracks, &TrackChoice::Track(choice.clone()), primary) >= 0
        {
            return None;
        }
        drop(engine);
        self.state
            .online_subtitles
            .lock()
            .unwrap()
            .iter()
            .position(|subtitle| {
                nova_config::playback_preferences::normalize_language(&subtitle.language).as_deref()
                    == Some(language)
            })
            .map(|index| -2 - index as i32)
    }

    pub fn add_online_secondary_subtitle(
        &self,
        path: &str,
        title: &str,
        language: &str,
    ) -> Result<i32, String> {
        self.add_online_subtitle_for_slot(path, title, language, true)
    }

    /// Preserve what the viewer currently hears/sees during an explicit source
    /// retry, even when Automatic originally chose these tracks.
    pub fn retry_preferences(&self) -> SessionPreferences {
        let mut preferences = self.state.preferences.lock().unwrap().preferences.clone();
        if self.state.pending.lock().unwrap().is_none()
            && let Some(mpv) = self.state.mpv.lock().unwrap().as_ref()
            && mpv.get_property::<String>("path").ok().as_deref()
                == Some(self.state.preference_source.lock().unwrap().as_str())
            && let Ok(raw) = mpv.get_property::<String>("track-list")
        {
            let tracks = track_preferences::parse_tracks(&raw);
            for audio in [true, false] {
                let (saved, language) = if audio {
                    (
                        preferences.title.audio.as_ref(),
                        preferences.audio_language.as_str(),
                    )
                } else {
                    (
                        preferences.title.subtitles.as_ref(),
                        preferences.subtitle_language.as_str(),
                    )
                };
                // A missing preferred language must remain preferred on the
                // replacement source, rather than turning into fallback/Off.
                if saved.is_some() || !language.is_empty() {
                    continue;
                }
                let selected = mpv
                    .get_property::<String>(if audio { "aid" } else { "sid" })
                    .ok();
                let choice = selected
                    .as_deref()
                    .and_then(|id| id.parse::<i32>().ok())
                    .and_then(|id| {
                        tracks
                            .iter()
                            .find(|track| track.audio == audio && track.id == id)
                    })
                    .map(|track| TrackChoice::Track(track.descriptor.clone()))
                    .or_else(|| (selected.as_deref() == Some("no")).then_some(TrackChoice::Off));
                if let Some(choice) = choice {
                    if audio {
                        preferences.title.audio = Some(choice);
                    } else {
                        preferences.title.subtitles = Some(choice);
                    }
                }
            }
        }
        preferences
    }
}

/// Off describes the viewer's visible slot, while automatic timing may own a
/// hidden native reference. Unrelated track refreshes must not steal it.
pub(super) fn apply_secondary_track(
    mpv: &Mpv,
    sync: &mut subtitle_sync::SubtitleSync,
    choice: &TrackChoice,
    id: i32,
) {
    if matches!(choice, TrackChoice::Off) && sync.owns_reference(mpv) {
        return;
    }
    if !matches!(choice, TrackChoice::Off) {
        sync.release_reference(mpv);
    }
    let _ = mpv.set_property(
        "secondary-sid",
        if id < 0 {
            "no".to_owned()
        } else {
            id.to_string()
        },
    );
    let _ = mpv.set_property("secondary-sub-visibility", true);
    sync.secondary_selection_changed(mpv);
}

fn secondary_track_id(
    tracks: &[nova_config::playback_preferences::SemanticTrack],
    choice: &TrackChoice,
    primary: Option<i32>,
) -> i32 {
    let candidates = tracks
        .iter()
        .filter(|track| Some(track.id) != primary)
        .cloned()
        .collect::<Vec<_>>();
    select_track(&candidates, false, Some(choice), "off").unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_language_remaps_track_ids_and_never_duplicates_primary() {
        let original = track_preferences::parse_tracks(
            r#"[
            {"id":2,"type":"sub","lang":"en"},
            {"id":7,"type":"sub","lang":"hr"}
        ]"#,
        );
        let choice = TrackChoice::Track(original[1].descriptor.clone());
        assert_eq!(secondary_track_id(&original, &choice, Some(2)), 7);
        let replacement = track_preferences::parse_tracks(
            r#"[
            {"id":7,"type":"sub","lang":"en"},
            {"id":11,"type":"sub","lang":"hr"}
        ]"#,
        );
        assert_eq!(secondary_track_id(&replacement, &choice, Some(7)), 11);
        assert_eq!(secondary_track_id(&replacement, &choice, Some(11)), -1);
        assert_eq!(secondary_track_id(&replacement[..1], &choice, Some(7)), -1);
        assert_eq!(
            secondary_track_id(&replacement, &TrackChoice::Off, Some(7)),
            -1
        );
    }
}

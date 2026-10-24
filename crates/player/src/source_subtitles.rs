//! Source-attached caption facts stay with the accepted playback session.
//! mpv's asynchronous `sub-files` loader often omits their declared language.
use nova_config::playback_preferences::normalize_language;

/// Runtime-only source metadata, never serialized into preferences or sync.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceSubtitle {
    pub url: String,
    pub language: Option<String>,
}

// Caption URLs can contain credentials. Keep diagnostics semantic.
impl std::fmt::Debug for SourceSubtitle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSubtitle")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl From<String> for SourceSubtitle {
    fn from(url: String) -> Self {
        Self {
            url,
            language: None,
        }
    }
}

impl From<&str> for SourceSubtitle {
    fn from(url: &str) -> Self {
        Self::from(url.to_owned())
    }
}

pub(super) fn bounded(subtitles: &[SourceSubtitle]) -> Vec<SourceSubtitle> {
    let mut total = 0usize;
    subtitles
        .iter()
        .take(20)
        .filter_map(|subtitle| {
            let url = &subtitle.url;
            total = total.saturating_add(url.len());
            if total > 32 * 1024
                || url.len() > 4096
                || !(url.starts_with("https://") || url.starts_with("http://"))
                || url.bytes().any(|byte| byte < b' ' || byte == 127)
            {
                return None;
            }
            Some(SourceSubtitle {
                url: url.clone(),
                language: subtitle
                    .language
                    .as_deref()
                    .filter(|language| language.len() <= 64)
                    .and_then(normalize_language),
            })
        })
        .collect()
}

#[derive(Default)]
pub(super) struct SourceSubtitles {
    source: String,
    session: u64,
    subtitles: Vec<SourceSubtitle>,
}

impl SourceSubtitles {
    pub(super) fn begin(&mut self, source: &str, session: u64, subtitles: Vec<SourceSubtitle>) {
        self.source = source.into();
        self.session = session;
        self.subtitles = subtitles;
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn enrich(&self, source: &str, session: u64, raw: String) -> String {
        if self.subtitles.is_empty() || source != self.source || session != self.session {
            return raw;
        }
        let Ok(serde_json::Value::Array(mut tracks)) = serde_json::from_str(&raw) else {
            return raw;
        };
        for track in &mut tracks {
            if track["type"] != "sub" || track["external"].as_bool() != Some(true) {
                continue;
            }
            let Some(url) = track["external-filename"].as_str() else {
                continue;
            };
            let matching = self
                .subtitles
                .iter()
                .filter(|subtitle| subtitle.url == url)
                .collect::<Vec<_>>();
            if matching.is_empty() {
                continue;
            }
            // Never strip URL queries or infer language from filenames/titles.
            // Conflicting declarations for one URL do not establish a language.
            let languages = matching
                .iter()
                .filter_map(|subtitle| subtitle.language.as_deref().and_then(normalize_language))
                .collect::<std::collections::BTreeSet<_>>();
            let native_language = track["lang"].as_str().and_then(normalize_language);
            if native_language.is_none() && languages.len() == 1 {
                track["lang"] = serde_json::json!(languages.into_iter().next().unwrap());
            }
            // Menu provenance is separate from title/kind, so saved semantic
            // choices never acquire a URL, source identity or translated label.
            track["nova-source-subtitle"] = serde_json::Value::Bool(true);
        }
        serde_json::to_string(&tracks).unwrap_or(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_config::playback_preferences::{TrackChoice, select_track};
    use serde_json::json;

    fn source(url: &str, language: Option<&str>) -> SourceSubtitle {
        SourceSubtitle {
            url: url.into(),
            language: language.map(str::to_owned),
        }
    }

    #[test]
    fn exact_url_language_and_origin_survive_same_session_recovery_but_not_replacement() {
        let url = "https://example.invalid/eng.vtt?edition=one&token=ephemeral";
        let raw = json!([
            {"id":1,"type":"sub","external":true,"external-filename":url},
            {"id":2,"type":"sub","external":true,"external-filename":"https://example.invalid/eng.vtt?edition=two&token=ephemeral"},
            {"id":3,"type":"sub","external":false,"title":"eng.vtt"},
            {"id":4,"type":"sub","external":true,"external-filename":url,"lang":"hr"}
        ]).to_string();
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin("video", 7, vec![source(url, Some("ENG"))]);
        let enriched = subtitles.enrich("video", 7, raw.clone());
        let tracks: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(tracks[0]["lang"], "en");
        assert_eq!(tracks[0]["nova-source-subtitle"], true);
        assert!(tracks[1]["lang"].is_null());
        assert!(tracks[1]["nova-source-subtitle"].is_null());
        assert!(tracks[2]["nova-source-subtitle"].is_null());
        assert_eq!(tracks[3]["lang"], "hr", "native known language wins");
        assert_eq!(subtitles.enrich("video", 7, raw.clone()), enriched);
        assert_eq!(subtitles.enrich("different video", 7, raw.clone()), raw);
        assert_eq!(subtitles.enrich("video", 8, raw.clone()), raw);
        subtitles.begin("video", 8, Vec::new());
        assert_eq!(subtitles.enrich("video", 8, raw.clone()), raw);
        subtitles.clear();
        assert_eq!(subtitles.enrich("video", 7, raw.clone()), raw);
    }

    #[test]
    fn provided_main_and_second_languages_select_independently_without_persisting_origin() {
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin(
            "video",
            1,
            vec![
                source("https://x/en", Some("eng")),
                source("https://x/hr", Some("hrv")),
            ],
        );
        let raw = json!([
            {"id":5,"type":"sub","external":true,"external-filename":"https://x/en"},
            {"id":9,"type":"sub","external":true,"external-filename":"https://x/hr"}
        ])
        .to_string();
        let raw = subtitles.enrich("video", 1, raw);
        let tracks = super::super::track_preferences::parse_tracks(&raw);
        assert_eq!(select_track(&tracks, false, None, "en"), Some(5));
        assert_eq!(select_track(&tracks, false, None, "hr"), Some(9));
        assert_eq!(
            select_track(&tracks, false, Some(&TrackChoice::Off), "en"),
            Some(-1)
        );
        assert!(tracks.iter().all(|track| track.descriptor.title.is_none()));
        assert!(
            !serde_json::to_string(&tracks[0].descriptor)
                .unwrap()
                .contains("https")
        );
    }

    #[test]
    fn unknown_and_conflicting_languages_stay_unknown_and_url_limits_still_apply() {
        let mut subtitles = SourceSubtitles::default();
        subtitles.begin(
            "video",
            1,
            vec![
                source("https://x/eng.vtt", None),
                source("https://x/conflict", Some("en")),
                source("https://x/conflict", Some("hr")),
            ],
        );
        let raw = subtitles.enrich(
            "video",
            1,
            json!([
                {"id":1,"type":"sub","external":true,"external-filename":"https://x/eng.vtt"},
                {"id":2,"type":"sub","external":true,"external-filename":"https://x/conflict"}
            ])
            .to_string(),
        );
        assert!(
            super::super::track_preferences::parse_tracks(&raw)
                .iter()
                .all(|track| track.descriptor.language.is_none())
        );
        let entries = bounded(&[
            source("file:///tmp/sub.vtt", Some("en")),
            source("https://x/sub\n", Some("en")),
            source("https://x/sub", Some("ENG")),
        ]);
        assert_eq!(entries, vec![source("https://x/sub", Some("en"))]);
        assert!(!format!("{:?}", entries[0]).contains("https"));
    }

    #[test]
    fn native_sub_files_keep_async_loading_and_recover_declared_caption_facts() {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "nova-source-captions-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let video = scratch.0.join("audio.wav");
        let caption = scratch.0.join("opaque-caption.vtt");
        let size = 80_000_u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((size + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(size.to_le_bytes());
        wav.resize(wav.len() + size as usize, 128);
        std::fs::write(&video, wav).unwrap();
        std::fs::write(
            &caption,
            "WEBVTT\n\n00:00.000 --> 00:05.000\nLocal caption fixture\n",
        )
        .unwrap();
        let video = video.to_string_lossy().into_owned();
        let caption = caption.to_string_lossy().into_owned();
        let mpv = libmpv2::Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("pause", true)
        })
        .unwrap();
        super::super::set_string_list(&mpv, c"sub-files", vec![caption.clone()]).unwrap();
        let mut metadata = SourceSubtitles::default();
        metadata.begin(&video, 42, vec![source(&caption, Some("eng"))]);
        for _ in 0..2 {
            mpv.command("loadfile", &[&video]).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "local fixture did not load");
                // This private fixture owns its event queue. No media is fetched
                // over the network, and a second FileLoaded proves a real reload.
                let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), 0.01) };
                if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_FILE_LOADED {
                    break;
                }
            }
            let raw = mpv.get_property::<String>("track-list").unwrap();
            let native = super::super::track_preferences::parse_tracks(&raw);
            let untagged = native.iter().find(|track| !track.audio).unwrap();
            assert!(
                untagged.descriptor.language.is_none(),
                "sub-files does not carry declared language"
            );
            let enriched = metadata.enrich(&video, 42, raw.clone());
            let tracks = super::super::track_preferences::parse_tracks(&enriched);
            assert_eq!(select_track(&tracks, false, None, "en"), Some(untagged.id));
            let listed: serde_json::Value = serde_json::from_str(&enriched).unwrap();
            assert!(
                listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|track| track["nova-source-subtitle"] == true)
            );
            mpv.set_property("sid", "no").unwrap();
            metadata.enrich(&video, 42, raw);
            assert_eq!(
                mpv.get_property::<String>("sid").unwrap(),
                "no",
                "enrichment never changes a viewer selection"
            );
            assert!(mpv.get_property::<bool>("pause").unwrap());
        }
        mpv.command("stop", &[]).unwrap();
    }
}

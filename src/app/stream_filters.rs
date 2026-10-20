//! Universal offer rules share the existing settings sync/override policy.
use super::*;
use crate::StreamFiltersBridge;
use nova_config::stream_filters::{
    StreamFacts, StreamFilterRules, VideoCodec, advertised, audio_language, resolution,
    structured_video_codec,
};

pub(super) const RESOLUTIONS: [u16; 7] = [0, 480, 720, 1080, 1440, 2160, 4320];
const SIZES_GIB: [u64; 9] = [0, 1, 2, 4, 8, 16, 32, 64, 128];

/// Structured, explicitly audio/video-owned addon metadata wins over release
/// text. No URL/credential parsing and no guessing languages from subtitle rows.
pub(super) fn stream_facts(stream: &addons::Stream) -> StreamFacts {
    let extra = &stream.extra;
    let hints = extra.get("behaviorHints");
    let filename = hints
        .and_then(|hints| hints.get("filename"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let advertised_text = [
        filename,
        stream.name.as_deref().unwrap_or(""),
        stream.title_legacy.as_deref().unwrap_or(""),
        stream.description.as_deref().unwrap_or(""),
    ]
    .join("\n");
    let mut facts = advertised(&advertised_text);
    let value = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            extra
                .get(*key)
                .or_else(|| hints.and_then(|hints| hints.get(*key)))
        })
    };
    let number =
        |value: &serde_json::Value| value.as_u64().or_else(|| value.as_str()?.parse().ok());
    if let Some(height) = value(&["height", "videoHeight", "resolution", "videoResolution"])
        .and_then(|value| {
            value.as_str().and_then(resolution).or_else(|| {
                number(value)
                    .and_then(|height| u16::try_from(height).ok())
                    .filter(|height| (144..=8640).contains(height))
            })
        })
        .or_else(|| {
            extra
                .get("video")
                .and_then(|video| video.get("height"))
                .and_then(number)
                .and_then(|height| u16::try_from(height).ok())
                .filter(|height| (144..=8640).contains(height))
        })
    {
        facts.height = Some(height);
    }
    if let Some(bytes) = hints
        .and_then(|hints| hints.get("videoSize"))
        .or_else(|| value(&["size", "videoSize"]))
        .and_then(number)
        .filter(|bytes| *bytes > 0)
    {
        facts.bytes = Some(bytes);
    }
    if let Some(codec) = value(&["videoCodec", "video_codec"])
        .and_then(serde_json::Value::as_str)
        .and_then(structured_video_codec)
        .or_else(|| {
            extra
                .get("video")
                .and_then(|video| video.get("codec"))
                .and_then(serde_json::Value::as_str)
                .and_then(structured_video_codec)
        })
        .or_else(|| {
            value(&["codec"])
                .and_then(serde_json::Value::as_str)
                .and_then(VideoCodec::parse)
        })
    {
        facts.codec = Some(codec);
    }
    let audio = value(&["audioLanguages", "audioLanguage", "audio_languages"]).or_else(|| {
        extra
            .get("audio")
            .and_then(|audio| audio.get("languages").or_else(|| audio.get("language")))
    });
    if let Some(audio) = audio {
        let values = match audio {
            serde_json::Value::String(value) => value
                .split([',', '/', ';', ' '])
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .take(32)
                .collect(),
            _ => Vec::new(),
        };
        let languages = values
            .iter()
            .filter_map(|value| audio_language(value))
            .fold(Vec::new(), |mut languages, language| {
                if !languages.contains(&language) {
                    languages.push(language);
                }
                languages
            });
        if !languages.is_empty() {
            facts.audio_languages = languages;
        }
    }
    facts
}

/// Keep completed artifacts accessible. Online candidates use one ruleset no
/// matter which addon returned them; stable ties retain provider/source order.
pub(super) fn apply_stream_rules(rows: &mut Vec<StreamUi>, rules: &StreamFilterRules) -> usize {
    if !rules.enabled {
        return 0;
    }
    let before = rows.len();
    rows.retain(|row| {
        matches!(row.source, StreamSource::Downloaded { .. })
            || rules.accepts(&row.card.filter_facts)
    });
    rank_stream_rules(rows, rules);
    before - rows.len()
}

pub(super) fn rank_stream_rules(rows: &mut [StreamUi], rules: &StreamFilterRules) {
    if rules.enabled && !rules.audio_languages.is_empty() {
        rows.sort_by_key(|row| {
            (
                usize::from(!matches!(row.source, StreamSource::Downloaded { .. })),
                rules.audio_rank(&row.card.filter_facts),
            )
        });
    }
}

/// Automatic decisions cannot outrank an explicitly preferred audio language
/// with a remembered release family. The ordinary picker still shows all valid
/// fallback languages unless the viewer chose to require a match.
pub(super) fn preferred_stream_rules<'a>(
    rows: &'a [StreamUi],
    rules: &StreamFilterRules,
) -> &'a [StreamUi] {
    if !rules.enabled || rules.audio_languages.is_empty() {
        return rows;
    }
    let Some(first) = rows.first() else {
        return rows;
    };
    let offline = matches!(first.source, StreamSource::Downloaded { .. });
    let rank = rules.audio_rank(&first.card.filter_facts);
    let count = rows
        .iter()
        .take_while(|row| {
            matches!(row.source, StreamSource::Downloaded { .. }) == offline
                && rules.audio_rank(&row.card.filter_facts) == rank
        })
        .count();
    &rows[..count]
}

impl Bridge {
    pub(super) fn wire_stream_filters(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let filters = app.global::<StreamFiltersBridge>();
        let b = self.clone();
        filters.on_edited(move |field, value| b.edit_stream_filter_rule(field, value.as_str()));
        let b = self.clone();
        filters.on_reset(move || b.store_stream_filter_rules(StreamFilterRules::default()));
        self.refresh_stream_filter_settings();
    }

    pub(super) fn refresh_stream_filter_settings(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let rules = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .stream_filters
            .clone()
            .normalized();
        let bridge = app.global::<StreamFiltersBridge>();
        bridge.set_enabled(rules.enabled);
        bridge.set_max_resolution_index(
            RESOLUTIONS
                .iter()
                .position(|height| Some(*height) == rules.max_height)
                .unwrap_or(0) as i32,
        );
        bridge.set_max_size_index(
            SIZES_GIB
                .iter()
                .position(|size| Some(size << 30) == rules.max_bytes)
                .unwrap_or(0) as i32,
        );
        bridge.set_max_resolution_label(
            rules
                .max_height
                .map(|height| format!("{height}p"))
                .unwrap_or_default()
                .into(),
        );
        bridge.set_max_size_label(
            rules
                .max_bytes
                .map(text::stream_size)
                .unwrap_or_default()
                .into(),
        );
        bridge.set_allow_avc(rules.video_codecs.contains(&VideoCodec::Avc));
        bridge.set_allow_hevc(rules.video_codecs.contains(&VideoCodec::Hevc));
        bridge.set_allow_av1(rules.video_codecs.contains(&VideoCodec::Av1));
        bridge.set_allow_vp9(rules.video_codecs.contains(&VideoCodec::Vp9));
        bridge.set_allow_other(rules.video_codecs.contains(&VideoCodec::Other));
        bridge.set_audio_languages(rules.audio_languages.join(", ").into());
        bridge.set_require_audio(rules.require_audio_language);
        bridge.set_retain_unknown(rules.retain_unknown);
    }

    fn edit_stream_filter_rule(&self, field: i32, value: &str) {
        let mut rules = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .stream_filters
            .clone()
            .normalized();
        let index = value.parse::<usize>().ok();
        let boolean = value == "true";
        match field {
            0 => rules.enabled = boolean,
            1 => {
                rules.max_height = index
                    .and_then(|index| RESOLUTIONS.get(index).copied())
                    .filter(|height| *height > 0)
            }
            2 => {
                rules.max_bytes = index
                    .and_then(|index| SIZES_GIB.get(index).copied())
                    .filter(|size| *size > 0)
                    .map(|size| size << 30)
            }
            3..=6 => {
                let codec = [
                    VideoCodec::Avc,
                    VideoCodec::Hevc,
                    VideoCodec::Av1,
                    VideoCodec::Vp9,
                ][(field - 3) as usize];
                rules.video_codecs.retain(|value| *value != codec);
                if boolean {
                    rules.video_codecs.push(codec);
                }
            }
            10 => {
                rules
                    .video_codecs
                    .retain(|value| *value != VideoCodec::Other);
                if boolean {
                    rules.video_codecs.push(VideoCodec::Other);
                }
            }
            7 => {
                let tokens = value
                    .split([',', ' ', ';'])
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>();
                let languages = tokens
                    .iter()
                    .map(|value| nova_config::playback_preferences::normalize_language(value))
                    .collect::<Option<Vec<_>>>();
                let Some(languages) = languages else {
                    if let Some(app) = self.app() {
                        app.global::<StreamFiltersBridge>()
                            .set_notice(text::tr("Use language codes such as en, ja, hr.").into());
                    }
                    return;
                };
                rules.audio_languages = languages;
            }
            8 => rules.require_audio_language = boolean,
            9 => rules.retain_unknown = boolean,
            _ => return,
        }
        self.store_stream_filter_rules(rules.normalized());
    }

    fn store_stream_filter_rules(&self, rules: StreamFilterRules) {
        if !writable_key("settings") {
            return;
        }
        allow_setting_field("stream_filters");
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.stream_filters = rules;
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings.clone());
        notify_setting_choice("stream_filters", &settings);
        write_settings(&settings);
        if let Some(app) = self.app() {
            app.global::<StreamFiltersBridge>().set_notice("".into());
        }
        self.refresh_stream_filter_settings();
        self.refresh_setting_sync();
        self.apply_stream_filter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_facts_override_conflicting_release_text_and_subtitles_are_not_audio() {
        let stream = addons::Stream {
            name: Some("2160p x265 8 GiB".into()),
            extra: HashMap::from([
                ("resolution".into(), serde_json::json!(1080)),
                ("videoCodec".into(), serde_json::json!("h264")),
                ("audioLanguages".into(), serde_json::json!(["jpn", "eng"])),
                (
                    "behaviorHints".into(),
                    serde_json::json!({"videoSize": 12345}),
                ),
            ]),
            ..Default::default()
        };
        let facts = stream_facts(&stream);
        assert_eq!(facts.height, Some(1080));
        assert_eq!(facts.codec, Some(VideoCodec::Avc));
        assert_eq!(facts.bytes, Some(12345));
        assert_eq!(facts.audio_languages, ["ja", "en"]);
        let unknown = stream_facts(&addons::Stream {
            description: Some("An English film\nSubtitles: Japanese".into()),
            ..Default::default()
        });
        assert!(unknown.audio_languages.is_empty());
    }
}

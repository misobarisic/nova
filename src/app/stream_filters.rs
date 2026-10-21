//! Universal offer rules share the existing settings sync/override policy.
use super::*;
use crate::StreamFiltersBridge;
use nova_config::stream_filters::{
    RESOLUTION_CHOICES, StreamFacts, StreamFilterRules, VideoCodec, advertised, audio_language,
    resolution, structured_video_codec,
};

pub(super) const RESOLUTIONS: [u16; 7] = [0, 480, 720, 1080, 1440, 2160, 4320];
const SIZES_GIB: [u64; 9] = [0, 1, 2, 4, 8, 16, 32, 64, 128];

/// Virtual account fields retain the nested local rules object. Audio source
/// and custom order form one coherent value; other choices sync independently.
pub(super) const RULE_SCOPES: &[(&str, &[&str])] = &[
    ("stream_filters_enabled", &["enabled"]),
    ("stream_filters_resolution", &["max_height"]),
    (
        "stream_filters_excluded_resolutions",
        &["excluded_resolutions"],
    ),
    (
        "stream_filters_resolution_priority",
        &["resolution_priority"],
    ),
    (
        "stream_filters_require_resolution",
        &["require_preferred_resolution"],
    ),
    ("stream_filters_size", &["max_bytes"]),
    ("stream_filters_allowed_codecs", &["video_codecs"]),
    ("stream_filters_excluded_codecs", &["excluded_video_codecs"]),
    ("stream_filters_codec_priority", &["codec_priority"]),
    ("stream_filters_sort_order", &["sort_order"]),
    (
        "stream_filters_audio",
        &["audio_languages", "use_preferred_audio_languages"],
    ),
    ("stream_filters_require_audio", &["require_audio_language"]),
    ("stream_filters_require_codec", &["require_preferred_codec"]),
    ("stream_filters_retain_unknown", &["retain_unknown"]),
];

pub(super) fn scope_fields(key: &str) -> Option<&'static [&'static str]> {
    RULE_SCOPES
        .iter()
        .find(|(scope, _)| *scope == key)
        .map(|(_, fields)| *fields)
}

pub(super) fn scope_value(rules: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    let fields = scope_fields(key)?;
    if fields.len() == 1 {
        return rules.get(fields[0]).cloned();
    }
    let mut audio = serde_json::Map::new();
    audio.insert(
        "audio_languages".into(),
        rules.get("audio_languages")?.clone(),
    );
    audio.insert(
        "use_preferred_audio_languages".into(),
        rules
            .get("use_preferred_audio_languages")
            .cloned()
            .unwrap_or_else(|| {
                serde_json::Value::Bool(
                    rules
                        .get("audio_languages")
                        .and_then(serde_json::Value::as_array)
                        .is_none_or(Vec::is_empty),
                )
            }),
    );
    Some(serde_json::Value::Object(audio))
}

pub(super) fn flatten_settings(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if let Some(rules) = map.remove("stream_filters") {
        for (key, _) in RULE_SCOPES {
            if let Some(value) = scope_value(&rules, key) {
                map.entry((*key).to_owned()).or_insert(value);
            }
        }
    }
}

pub(super) fn settings_value(settings: &CacheSettings) -> serde_json::Value {
    let mut value = serde_json::to_value(settings).unwrap_or_default();
    flatten_settings(&mut value);
    value
}

pub(super) fn settings_from_value(
    mut value: serde_json::Value,
) -> Result<CacheSettings, serde_json::Error> {
    if let Some(map) = value.as_object_mut() {
        let mut rules = serde_json::to_value(StreamFilterRules::default()).unwrap();
        for (key, fields) in RULE_SCOPES {
            if let Some(value) = map.remove(*key) {
                if fields.len() == 1 {
                    rules[fields[0]] = value;
                } else {
                    for field in *fields {
                        // Missing coupled fields are invalid, not a reset.
                        rules[*field] = value
                            .get(*field)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                    }
                }
            }
        }
        map.insert("stream_filters".into(), rules);
    }
    serde_json::from_value(value)
}

pub(super) fn migrate_overrides(settings: &mut CacheSettings) {
    if let Some(legacy) = settings.sync_overrides.get("stream_filters").cloned()
        && let Ok(rules) = serde_json::from_value::<StreamFilterRules>(legacy)
    {
        let rules = serde_json::to_value(rules).unwrap();
        for (key, _) in RULE_SCOPES {
            if let Some(value) = scope_value(&rules, key) {
                settings
                    .sync_overrides
                    .entry((*key).into())
                    .or_insert(value);
            }
        }
        settings.sync_overrides.remove("stream_filters");
    }
}

pub(super) fn effective_rules(settings: &CacheSettings) -> StreamFilterRules {
    settings.stream_filters.resolved_with_subtitles(
        &settings.default_audio_language,
        &settings.default_subtitle_language,
    )
}

fn codec_code(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Avc => "avc",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
        VideoCodec::Vp9 => "vp9",
        VideoCodec::Other => "other",
    }
}
fn codec_value(value: &str) -> Option<VideoCodec> {
    if value.trim() == "other" {
        Some(VideoCodec::Other)
    } else {
        VideoCodec::parse(value)
    }
}

/// Structured, explicitly audio/video-owned addon metadata wins over release
/// text. No URL/credential parsing and no guessing languages from subtitle rows.
pub(super) fn stream_facts(stream: &addons::Stream) -> StreamFacts {
    let extra = &stream.extra;
    let hints = extra.get("behaviorHints");
    let canonical = extra
        .get("streamFacts")
        .or_else(|| hints.and_then(|hints| hints.get("streamFacts")));
    // Providers can publish the exact evidence used by their candidate sort,
    // avoiding disagreement caused by formatting or reparsing their display text.
    if let Some(canonical) = canonical
        && let Ok(mut canonical) = serde_json::from_value::<StreamFacts>(canonical.clone())
    {
        canonical.height = canonical
            .height
            .filter(|height| (144..=8640).contains(height));
        canonical.bytes = canonical.bytes.filter(|bytes| *bytes > 0);
        canonical.audio_languages = canonical
            .audio_languages
            .iter()
            .take(32)
            .filter_map(|value| audio_language(value))
            .collect();
        canonical.subtitle_languages = canonical
            .subtitle_languages
            .iter()
            .take(32)
            .filter_map(|value| audio_language(value))
            .collect();
        // Unknown fields in this explicit evidence object stay unknown; do not
        // turn a provider's null codec/cache claim into a guess from its label.
        return canonical;
    }
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
    let boolean = |value: &serde_json::Value| {
        value.as_bool().or_else(|| match value.as_str()? {
            "true" | "yes" => Some(true),
            "false" | "no" => Some(false),
            _ => None,
        })
    };
    if let Some(cached) = value(&["cached", "isCached", "is_cached"]).and_then(boolean) {
        facts.cached = Some(cached);
    }
    if let Some(pack) = value(&["seasonPack", "season_pack", "isSeasonPack"]).and_then(boolean) {
        facts.season_pack = Some(pack);
    }
    if let Some(seeders) = value(&["seeders", "seedCount", "seed_count"]).and_then(number) {
        facts.seeders = Some(seeders);
    }
    let languages = |value: &serde_json::Value| {
        let values = match value {
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .take(32)
                .collect::<Vec<_>>(),
            serde_json::Value::String(value) => {
                value.split([',', '/', ';', ' ']).take(32).collect()
            }
            _ => Vec::new(),
        };
        values
            .into_iter()
            .filter_map(audio_language)
            .fold(Vec::new(), |mut list, language| {
                if !list.contains(&language) {
                    list.push(language);
                }
                list
            })
    };
    if let Some(subtitles) = value(&[
        "subtitleLanguages",
        "subtitleLanguage",
        "subtitle_languages",
    ]) {
        facts.subtitle_languages = languages(subtitles);
    } else {
        for subtitle in stream.subtitles.iter().take(32) {
            if let Some(language) = subtitle.lang.as_deref().and_then(audio_language)
                && !facts.subtitle_languages.contains(&language)
            {
                facts.subtitle_languages.push(language);
            }
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
    if rules.enabled {
        rows.sort_by(|a, b| {
            usize::from(!matches!(a.source, StreamSource::Downloaded { .. }))
                .cmp(&usize::from(!matches!(
                    b.source,
                    StreamSource::Downloaded { .. }
                )))
                .then_with(|| rules.compare(&a.card.filter_facts, &b.card.filter_facts))
        });
    }
}

/// A remembered release family can choose only within the best complete sort
/// bucket. It cannot override a higher-priority criterion chosen by the viewer.
pub(super) fn preferred_stream_rules<'a>(
    rows: &'a [StreamUi],
    rules: &StreamFilterRules,
) -> &'a [StreamUi] {
    if !rules.enabled {
        return rows;
    }
    let Some(first) = rows.first() else {
        return rows;
    };
    let offline = matches!(first.source, StreamSource::Downloaded { .. });
    let count = rows
        .iter()
        .take_while(|row| {
            matches!(row.source, StreamSource::Downloaded { .. }) == offline
                && rules
                    .compare(&first.card.filter_facts, &row.card.filter_facts)
                    .is_eq()
        })
        .count();
    &rows[..count]
}

impl Bridge {
    pub(super) fn effective_stream_filter_rules(&self) -> StreamFilterRules {
        effective_rules(&self.shared.lock().unwrap().cache_settings)
    }

    pub(super) fn wire_stream_filters(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let filters = app.global::<StreamFiltersBridge>();
        let b = self.clone();
        filters.on_edited(move |field, value| b.edit_stream_filter_rule(field, value.as_str()));
        let b = self.clone();
        filters.on_reset(move || b.store_stream_filter_rules(StreamFilterRules::default()));
        let b = self.clone();
        filters.on_sort_edited(move |criterion, action| {
            b.edit_stream_sort(criterion.as_str(), action)
        });
        let b = self.clone();
        filters.on_language_search(move |query| b.filter_stream_audio_languages(query.as_str()));
        let b = self.clone();
        filters.on_language_edited(move |code, action| {
            b.edit_stream_audio_language(code.as_str(), action)
        });
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
        bridge.set_sort_order(
            Rc::new(VecModel::from(
                rules
                    .sort_order
                    .iter()
                    .map(|rule| SharedString::from(rule.criterion.key()))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        bridge.set_sort_ascending(
            Rc::new(VecModel::from(
                rules
                    .sort_order
                    .iter()
                    .map(|rule| rule.direction == nova_config::stream_filters::SortDirection::Asc)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
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
        bridge.set_resolution_options(
            Rc::new(VecModel::from(
                RESOLUTION_CHOICES
                    .iter()
                    .map(|height| i32::from(*height))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        bridge.set_resolution_option_names(
            Rc::new(VecModel::from(
                RESOLUTION_CHOICES
                    .iter()
                    .map(|height| format!("{height}p").into())
                    .collect::<Vec<SharedString>>(),
            ))
            .into(),
        );
        bridge.set_excluded_resolutions(
            Rc::new(VecModel::from(
                rules
                    .excluded_resolutions
                    .iter()
                    .map(|height| i32::from(*height))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        bridge.set_resolution_priority_order(
            Rc::new(VecModel::from(
                rules
                    .resolution_priority
                    .iter()
                    .map(|height| i32::from(*height))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        bridge.set_require_preferred_resolution(rules.require_preferred_resolution);
        bridge.set_allow_avc(rules.video_codecs.contains(&VideoCodec::Avc));
        bridge.set_allow_hevc(rules.video_codecs.contains(&VideoCodec::Hevc));
        bridge.set_allow_av1(rules.video_codecs.contains(&VideoCodec::Av1));
        bridge.set_allow_vp9(rules.video_codecs.contains(&VideoCodec::Vp9));
        bridge.set_allow_other(rules.video_codecs.contains(&VideoCodec::Other));
        bridge.set_block_avc(rules.excluded_video_codecs.contains(&VideoCodec::Avc));
        bridge.set_block_hevc(rules.excluded_video_codecs.contains(&VideoCodec::Hevc));
        bridge.set_block_av1(rules.excluded_video_codecs.contains(&VideoCodec::Av1));
        bridge.set_block_vp9(rules.excluded_video_codecs.contains(&VideoCodec::Vp9));
        bridge.set_block_other(rules.excluded_video_codecs.contains(&VideoCodec::Other));
        bridge.set_codec_priority(
            rules
                .codec_priority
                .iter()
                .map(|codec| codec_code(*codec))
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        );
        let order = rules
            .codec_priority
            .iter()
            .map(|codec| match codec {
                VideoCodec::Avc => 0,
                VideoCodec::Hevc => 1,
                VideoCodec::Av1 => 2,
                VideoCodec::Vp9 => 3,
                VideoCodec::Other => 4,
            })
            .collect::<Vec<i32>>();
        bridge.set_codec_priority_order(Rc::new(VecModel::from(order)).into());
        bridge.set_use_preferred_audio_languages(rules.use_preferred_audio_languages);
        let inherited = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .default_audio_language
            .clone();
        let label = nova_config::playback_preferences::language_preferences(&inherited)
            .iter()
            .map(|code| nova_config::playback_preferences::language_label(code))
            .collect::<Vec<_>>()
            .join(" → ");
        bridge.set_preferred_audio_languages(if label.is_empty() {
            text::tr("Automatic").into()
        } else {
            label.into()
        });
        bridge.set_audio_languages(rules.audio_languages.join(", ").into());
        self.filter_stream_audio_languages("");
        bridge.set_require_audio(rules.require_audio_language);
        bridge.set_require_preferred_codec(rules.require_preferred_codec);
        bridge.set_retain_unknown(rules.retain_unknown);
    }

    fn filter_stream_audio_languages(&self, query: &str) {
        let Some(app) = self.app() else { return };
        let bridge = app.global::<StreamFiltersBridge>();
        let choices = nova_config::playback_preferences::language_choices(query);
        let model = |items: Vec<SharedString>| -> slint::ModelRc<SharedString> {
            Rc::new(VecModel::from(items)).into()
        };
        bridge.set_language_codes(model(
            choices
                .iter()
                .map(|(code, _)| code.clone().into())
                .collect(),
        ));
        bridge.set_language_names(model(
            choices.into_iter().map(|(_, name)| name.into()).collect(),
        ));
        let selected = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .stream_filters
            .clone()
            .normalized()
            .audio_languages;
        bridge.set_selected_language_codes(model(
            selected.iter().map(|code| code.clone().into()).collect(),
        ));
        bridge.set_selected_language_names(model(
            selected
                .iter()
                .map(|code| nova_config::playback_preferences::language_label(code).into())
                .collect(),
        ));
    }

    fn edit_stream_audio_language(&self, code: &str, action: i32) {
        let Some(code) = nova_config::playback_preferences::normalize_language(code) else {
            return;
        };
        let mut rules = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .stream_filters
            .clone()
            .normalized();
        let index = rules
            .audio_languages
            .iter()
            .position(|value| *value == code);
        match (action, index) {
            (0, None) if rules.audio_languages.len() < 16 => rules.audio_languages.push(code),
            (1, Some(index)) => {
                rules.audio_languages.remove(index);
            }
            (2, Some(index)) if index > 0 => rules.audio_languages.swap(index, index - 1),
            (3, Some(index)) if index + 1 < rules.audio_languages.len() => {
                rules.audio_languages.swap(index, index + 1)
            }
            _ => return,
        }
        rules.use_preferred_audio_languages = false;
        self.store_stream_filter_rules(rules);
    }

    fn edit_stream_sort(&self, criterion: &str, action: i32) {
        let mut rules = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .stream_filters
            .clone()
            .normalized();
        let Some(index) = rules
            .sort_order
            .iter()
            .position(|rule| rule.criterion.key() == criterion)
        else {
            return;
        };
        match action {
            0 if index > 0 => rules.sort_order.swap(index, index - 1),
            1 if index + 1 < rules.sort_order.len() => rules.sort_order.swap(index, index + 1),
            2 => {
                use nova_config::stream_filters::SortDirection;
                rules.sort_order[index].direction =
                    if rules.sort_order[index].direction == SortDirection::Asc {
                        SortDirection::Desc
                    } else {
                        SortDirection::Asc
                    };
            }
            _ => return,
        }
        self.store_stream_filter_rules(rules);
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
                rules.use_preferred_audio_languages = false;
            }
            11..=15 => {
                let codec = [
                    VideoCodec::Avc,
                    VideoCodec::Hevc,
                    VideoCodec::Av1,
                    VideoCodec::Vp9,
                    VideoCodec::Other,
                ][(field - 11) as usize];
                rules.excluded_video_codecs.retain(|value| *value != codec);
                if boolean {
                    rules.excluded_video_codecs.push(codec);
                }
            }
            16 => {
                if let Some((action, code)) = value.split_once(':') {
                    let Some(codec) = codec_value(code) else {
                        return;
                    };
                    let index = rules
                        .codec_priority
                        .iter()
                        .position(|value| *value == codec);
                    match (action, index) {
                        ("add", None) => rules.codec_priority.push(codec),
                        ("remove", Some(index)) => {
                            rules.codec_priority.remove(index);
                        }
                        ("up", Some(index)) if index > 0 => {
                            rules.codec_priority.swap(index, index - 1)
                        }
                        ("down", Some(index)) if index + 1 < rules.codec_priority.len() => {
                            rules.codec_priority.swap(index, index + 1)
                        }
                        _ => return,
                    }
                } else {
                    let Some(priority) = value
                        .split(',')
                        .filter(|value| !value.trim().is_empty())
                        .map(codec_value)
                        .collect::<Option<Vec<_>>>()
                    else {
                        return;
                    };
                    rules.codec_priority = priority;
                }
            }
            17 => rules.use_preferred_audio_languages = boolean,
            18 => rules.require_preferred_codec = boolean,
            19 | 20 => {
                let Some((action, height)) = value.split_once(':') else {
                    return;
                };
                let Some(height) = resolution(height) else {
                    return;
                };
                let list = if field == 19 {
                    &mut rules.excluded_resolutions
                } else {
                    &mut rules.resolution_priority
                };
                let index = list.iter().position(|item| *item == height);
                match (action, index) {
                    ("add", None) => list.push(height),
                    ("remove", Some(index)) => {
                        list.remove(index);
                    }
                    ("up", Some(index)) if field == 20 && index > 0 => list.swap(index, index - 1),
                    ("down", Some(index)) if field == 20 && index + 1 < list.len() => {
                        list.swap(index, index + 1)
                    }
                    _ => return,
                }
            }
            21 => rules.require_preferred_resolution = boolean,
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
        let (settings, changed) = {
            let mut state = self.shared.lock().unwrap();
            migrate_overrides(&mut state.cache_settings);
            let before = serde_json::to_value(&state.cache_settings.stream_filters).unwrap();
            let after = serde_json::to_value(&rules).unwrap();
            let changed = RULE_SCOPES
                .iter()
                .filter_map(|(key, _)| {
                    (scope_value(&before, key) != scope_value(&after, key)).then_some(*key)
                })
                .collect::<Vec<_>>();
            state.cache_settings.stream_filters = rules;
            (state.cache_settings.clone(), changed)
        };
        set_active_cache_settings(settings.clone());
        for field in changed {
            allow_setting_field(field);
            notify_setting_choice(field, &settings);
        }
        write_settings(&settings);
        if let Some(app) = self.app() {
            app.global::<StreamFiltersBridge>().set_notice("".into());
        }
        self.refresh_stream_filter_settings();
        self.refresh_setting_sync();
        self.refresh_stream_preferences_policy();
        self.apply_stream_filter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_sort_crosses_addon_groups_and_only_true_ties_keep_source_order() {
        use nova_config::stream_filters::{SortCriterion, SortDirection};
        let row = |id: &str, addon: &str, language: &str, seeds| StreamUi {
            id: id.into(),
            choice_id: Some(id.into()),
            owner_url: addon.into(),
            addon: addon.into(),
            display: id.into(),
            source: StreamSource::Url(format!("https://example.test/{id}")),
            download: None,
            card: StreamCardText {
                filter_facts: StreamFacts {
                    audio_languages: vec![language.into()],
                    codec: Some(VideoCodec::Av1),
                    seeders: seeds,
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let mut rules = StreamFilterRules {
            enabled: true,
            audio_languages: vec!["ja".into(), "en".into()],
            codec_priority: vec![VideoCodec::Av1],
            use_preferred_audio_languages: false,
            ..Default::default()
        };
        let mut rows = vec![
            row("early-en", "first-addon", "en", Some(200)),
            row("late-ja", "later-addon", "ja", Some(100)),
            row("late-ja-tie", "later-addon", "ja", Some(100)),
            row("unknown-seeds", "first-addon", "ja", None),
        ];
        apply_stream_rules(&mut rows, &rules);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["late-ja", "late-ja-tie", "unknown-seeds", "early-en"]
        );
        assert_eq!(preferred_stream_rules(&rows, &rules).len(), 2);
        let index = rules
            .sort_order
            .iter()
            .position(|rule| rule.criterion == SortCriterion::Seeders)
            .unwrap();
        let seeders = rules.sort_order.remove(index);
        rules.sort_order.insert(0, seeders);
        rank_stream_rules(&mut rows, &rules);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["early-en", "late-ja", "late-ja-tie", "unknown-seeds"]
        );
        rules.sort_order[0].direction = SortDirection::Asc;
        rank_stream_rules(&mut rows, &rules);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["late-ja", "late-ja-tie", "early-en", "unknown-seeds"]
        );
        let mut download = row("download", "offline", "en", None);
        download.source = StreamSource::Downloaded {
            job_id: "job".into(),
            path: PathBuf::from("episode.mkv"),
        };
        rows.push(download);
        rules.excluded_video_codecs = vec![VideoCodec::Av1];
        apply_stream_rules(&mut rows, &rules);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].id, "download",
            "completed artifacts remain pinned and accessible"
        );
    }

    #[test]
    fn canonical_provider_facts_stay_identical_instead_of_guessing_from_presentation() {
        let stream = addons::Stream {
            name: Some("2160p x265".into()),
            description: Some(
                "Cached: yes\nSeason pack: yes\nSeeders: 900\nAudio: English\nSubtitles: Japanese"
                    .into(),
            ),
            extra: HashMap::from([(
                "behaviorHints".into(),
                serde_json::json!({"streamFacts": {
                    "height": 1080, "bytes": 123456789, "codec": null, "cached": null,
                    "season_pack": false, "seeders": 0, "audio_languages": ["hr"], "subtitle_languages": ["en"]
                }}),
            )]),
            ..Default::default()
        };
        let facts = stream_facts(&stream);
        assert_eq!(facts.height, Some(1080));
        assert_eq!(facts.bytes, Some(123456789));
        assert_eq!(facts.codec, None);
        assert_eq!(facts.cached, None);
        assert_eq!(facts.season_pack, Some(false));
        assert_eq!(facts.seeders, Some(0));
        assert_eq!(facts.audio_languages, ["hr"]);
        assert_eq!(facts.subtitle_languages, ["en"]);
    }

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

//! Provider-independent stream rules. Unknown metadata stays visible by default;
//! audio languages describe advertised audio, never external subtitle choices.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    Avc,
    Hevc,
    Av1,
    Vp9,
    Other,
}

impl VideoCodec {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('.', "").as_str() {
            "avc" | "avc1" | "h264" | "x264" => Some(Self::Avc),
            "hevc" | "hev1" | "hvc1" | "h265" | "x265" => Some(Self::Hevc),
            "av1" | "av01" => Some(Self::Av1),
            "vp9" | "vp09" => Some(Self::Vp9),
            "vp8" | "mpeg2" | "mpeg2video" | "mpeg4" | "xvid" | "divx" | "h263" | "vc1"
            | "wmv3" | "prores" => Some(Self::Other),
            _ => None,
        }
    }
}

/// An explicit video-codec field can establish a known incompatible codec even
/// when this version has no dedicated checkbox for that codec family.
pub fn structured_video_codec(value: &str) -> Option<VideoCodec> {
    VideoCodec::parse(value).or_else(|| {
        let value = value.trim();
        (!value.is_empty()
            && value.len() <= 64
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "unknown" | "n/a" | "none" | "auto"
            )
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            }))
        .then_some(VideoCodec::Other)
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortCriterion {
    Cached,
    Resolution,
    Size,
    SeasonPack,
    Seeders,
    Audio,
    Codec,
    Subtitle,
}
impl SortCriterion {
    pub fn key(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Resolution => "resolution",
            Self::Size => "size",
            Self::SeasonPack => "season_pack",
            Self::Seeders => "seeders",
            Self::Audio => "audio",
            Self::Codec => "codec",
            Self::Subtitle => "subtitle",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamSortRule {
    pub criterion: SortCriterion,
    pub direction: SortDirection,
}
pub fn default_sort_order() -> Vec<StreamSortRule> {
    use SortCriterion::*;
    use SortDirection::*;
    [
        (Audio, Asc),
        (Codec, Asc),
        (Cached, Desc),
        (Resolution, Desc),
        (Size, Asc),
        (SeasonPack, Desc),
        (Seeders, Desc),
        (Subtitle, Asc),
    ]
    .map(|(criterion, direction)| StreamSortRule {
        criterion,
        direction,
    })
    .to_vec()
}

// Direction changes never move an unavailable value ahead of known evidence.
fn compare_known<T: Ord>(
    a: Option<T>,
    b: Option<T>,
    direction: SortDirection,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) => {
            if direction == SortDirection::Asc {
                a.cmp(&b)
            } else {
                b.cmp(&a)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StreamFacts {
    pub height: Option<u16>,
    pub bytes: Option<u64>,
    pub codec: Option<VideoCodec>,
    pub audio_languages: Vec<String>,
    pub cached: Option<bool>,
    pub season_pack: Option<bool>,
    pub seeders: Option<u64>,
    pub subtitle_languages: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StreamFilterRules {
    pub enabled: bool,
    pub max_height: Option<u16>,
    pub max_bytes: Option<u64>,
    /// An empty set accepts all video codecs.
    pub video_codecs: Vec<VideoCodec>,
    /// Explicit blocks always win over the allowed set.
    pub excluded_video_codecs: Vec<VideoCodec>,
    /// Ordered preference among accepted codecs; unlisted codecs tie last.
    pub codec_priority: Vec<VideoCodec>,
    /// Resolve audio preferences from the profile's Preferred audio setting.
    pub use_preferred_audio_languages: bool,
    /// Ordered ISO language preferences. The first advertised match ranks first.
    pub audio_languages: Vec<String>,
    pub require_audio_language: bool,
    pub require_preferred_codec: bool,
    pub retain_unknown: bool,
    pub sort_order: Vec<StreamSortRule>,
    /// Derived from existing Preferred subtitles for request-time ranking only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subtitle_languages: Vec<String>,
}

impl Default for StreamFilterRules {
    fn default() -> Self {
        Self {
            enabled: false,
            max_height: None,
            max_bytes: None,
            video_codecs: Vec::new(),
            excluded_video_codecs: Vec::new(),
            codec_priority: Vec::new(),
            use_preferred_audio_languages: true,
            audio_languages: Vec::new(),
            require_audio_language: false,
            require_preferred_codec: false,
            retain_unknown: true,
            sort_order: default_sort_order(),
            subtitle_languages: Vec::new(),
        }
    }
}

// The absent inheritance flag has different meanings for old saved rules:
// preserve an explicit custom list, while untouched rules inherit Preferred audio.
impl<'de> Deserialize<'de> for StreamFilterRules {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Fields {
            enabled: bool,
            max_height: Option<u16>,
            max_bytes: Option<u64>,
            video_codecs: Vec<VideoCodec>,
            excluded_video_codecs: Vec<VideoCodec>,
            codec_priority: Vec<VideoCodec>,
            use_preferred_audio_languages: Option<bool>,
            audio_languages: Vec<String>,
            require_audio_language: bool,
            require_preferred_codec: bool,
            retain_unknown: Option<bool>,
            sort_order: Option<Vec<StreamSortRule>>,
            subtitle_languages: Vec<String>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            enabled: fields.enabled,
            max_height: fields.max_height,
            max_bytes: fields.max_bytes,
            video_codecs: fields.video_codecs,
            excluded_video_codecs: fields.excluded_video_codecs,
            codec_priority: fields.codec_priority,
            use_preferred_audio_languages: fields
                .use_preferred_audio_languages
                .unwrap_or(fields.audio_languages.is_empty()),
            audio_languages: fields.audio_languages,
            require_audio_language: fields.require_audio_language,
            require_preferred_codec: fields.require_preferred_codec,
            retain_unknown: fields.retain_unknown.unwrap_or(true),
            sort_order: fields.sort_order.unwrap_or_else(default_sort_order),
            subtitle_languages: fields.subtitle_languages,
        })
    }
}

impl StreamFilterRules {
    pub fn normalized(mut self) -> Self {
        self.max_height = self
            .max_height
            .filter(|height| (144..=8640).contains(height));
        self.max_bytes = self.max_bytes.filter(|bytes| *bytes > 0);
        for list in [
            &mut self.video_codecs,
            &mut self.excluded_video_codecs,
            &mut self.codec_priority,
        ] {
            let mut unique = Vec::new();
            for codec in list.drain(..) {
                if !unique.contains(&codec) {
                    unique.push(codec);
                }
            }
            *list = unique;
        }
        let mut languages = Vec::new();
        for value in self.audio_languages.iter().take(16) {
            if let Some(language) = crate::playback_preferences::normalize_language(value)
                && !languages.contains(&language)
            {
                languages.push(language);
            }
        }
        self.audio_languages = languages;
        let mut sort_order = Vec::new();
        for rule in self.sort_order.into_iter().chain(default_sort_order()) {
            if !sort_order
                .iter()
                .any(|old: &StreamSortRule| old.criterion == rule.criterion)
            {
                sort_order.push(rule);
            }
        }
        self.sort_order = sort_order;
        self
    }

    pub fn resolved(&self, preferred_audio: &str) -> Self {
        let mut resolved = self.clone().normalized();
        if resolved.use_preferred_audio_languages {
            resolved.audio_languages =
                crate::playback_preferences::language_preferences(preferred_audio);
        }
        resolved
    }

    pub fn resolved_with_subtitles(
        &self,
        preferred_audio: &str,
        preferred_subtitles: &str,
    ) -> Self {
        let mut resolved = self.resolved(preferred_audio);
        resolved.subtitle_languages =
            crate::playback_preferences::language_preferences(preferred_subtitles);
        resolved
    }

    pub fn compare(&self, a: &StreamFacts, b: &StreamFacts) -> std::cmp::Ordering {
        if !self.enabled {
            return std::cmp::Ordering::Equal;
        }
        let rank = |value| (value != usize::MAX).then_some(value);
        for rule in &self.sort_order {
            let order = match rule.criterion {
                SortCriterion::Cached => compare_known(a.cached, b.cached, rule.direction),
                SortCriterion::Resolution => compare_known(a.height, b.height, rule.direction),
                SortCriterion::Size => compare_known(a.bytes, b.bytes, rule.direction),
                SortCriterion::SeasonPack => {
                    compare_known(a.season_pack, b.season_pack, rule.direction)
                }
                SortCriterion::Seeders => compare_known(a.seeders, b.seeders, rule.direction),
                SortCriterion::Audio => compare_known(
                    rank(self.audio_rank(a)),
                    rank(self.audio_rank(b)),
                    rule.direction,
                ),
                SortCriterion::Codec => compare_known(
                    rank(self.codec_rank(a)),
                    rank(self.codec_rank(b)),
                    rule.direction,
                ),
                SortCriterion::Subtitle => compare_known(
                    rank(language_rank(
                        &self.subtitle_languages,
                        &a.subtitle_languages,
                    )),
                    rank(language_rank(
                        &self.subtitle_languages,
                        &b.subtitle_languages,
                    )),
                    rule.direction,
                ),
            };
            if !order.is_eq() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    }

    pub fn codec_rank(&self, facts: &StreamFacts) -> usize {
        if !self.enabled || self.codec_priority.is_empty() {
            return 0;
        }
        facts
            .codec
            .and_then(|codec| {
                self.codec_priority
                    .iter()
                    .position(|wanted| *wanted == codec)
            })
            .unwrap_or(usize::MAX)
    }

    pub fn rank(&self, facts: &StreamFacts) -> (usize, usize) {
        (self.audio_rank(facts), self.codec_rank(facts))
    }

    pub fn accepts(&self, facts: &StreamFacts) -> bool {
        if !self.enabled {
            return true;
        }
        let under = |limit: Option<u64>, known: Option<u64>| {
            limit.is_none_or(|limit| known.map_or(self.retain_unknown, |known| known <= limit))
        };
        under(self.max_height.map(u64::from), facts.height.map(u64::from))
            && under(self.max_bytes, facts.bytes)
            && facts.codec.map_or(
                self.excluded_video_codecs.is_empty() || self.retain_unknown,
                |codec| !self.excluded_video_codecs.contains(&codec),
            )
            && (self.video_codecs.is_empty()
                || facts.codec.map_or(self.retain_unknown, |codec| {
                    self.video_codecs.contains(&codec)
                }))
            && (!self.require_preferred_codec
                || self.codec_priority.is_empty()
                || facts.codec.map_or(self.retain_unknown, |codec| {
                    self.codec_priority.contains(&codec)
                }))
            && (!self.require_audio_language
                || self.audio_languages.is_empty()
                || if facts.audio_languages.is_empty() {
                    self.retain_unknown
                } else {
                    self.audio_rank(facts) < usize::MAX
                })
    }

    pub fn audio_rank(&self, facts: &StreamFacts) -> usize {
        if !self.enabled || self.audio_languages.is_empty() {
            return 0;
        }
        language_rank(&self.audio_languages, &facts.audio_languages)
    }
}

fn language_rank(wanted: &[String], actual: &[String]) -> usize {
    if wanted.is_empty() {
        return 0;
    }
    wanted
        .iter()
        .position(|wanted| {
            actual.iter().any(|actual| {
                actual == wanted || actual.split('-').next() == wanted.split('-').next()
            })
        })
        .unwrap_or(usize::MAX)
}

fn unique<T: PartialEq>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut result = None;
    for value in values {
        if result.as_ref().is_some_and(|old| old != &value) {
            return None;
        }
        result = Some(value);
    }
    result
}

pub fn resolution(value: &str) -> Option<u16> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "4k" | "uhd" => Some(2160),
        "8k" => Some(4320),
        "full hd" | "fhd" => Some(1080),
        "hd" => Some(720),
        "sd" => Some(480),
        _ => value
            .strip_suffix('p')
            .or_else(|| value.strip_suffix('i'))
            .unwrap_or(&value)
            .parse::<u16>()
            .ok()
            .filter(|height| (144..=8640).contains(height))
            .or_else(|| {
                let (width, height) = value.split_once('x')?;
                let width = width.parse::<u16>().ok()?;
                let height = height.parse::<u16>().ok()?;
                ((144..=8640).contains(&height) && width >= height && width <= 16384)
                    .then_some(height)
            }),
    }
}

fn size(value: &str) -> Option<u64> {
    let value = value.trim().to_ascii_lowercase();
    let boundary = value.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let amount = value[..boundary].parse::<f64>().ok()?;
    let unit = value[boundary..].trim();
    let multiplier = match unit {
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        _ => return None,
    };
    let bytes = amount * multiplier;
    (bytes.is_finite() && bytes > 0.0 && bytes <= 1_000_000_000_000_000.0)
        .then_some(bytes.round() as u64)
}

pub fn audio_language(value: &str) -> Option<String> {
    static NAMES: std::sync::LazyLock<std::collections::HashMap<String, String>> =
        std::sync::LazyLock::new(|| {
            isolang::Language::match_names(|_| true)
                .map(|language| {
                    (
                        language.to_name().to_lowercase(),
                        language
                            .to_639_1()
                            .unwrap_or_else(|| language.to_639_3())
                            .to_owned(),
                    )
                })
                .collect()
        });
    crate::playback_preferences::normalize_language(value)
        .or_else(|| NAMES.get(&value.trim().to_lowercase()).cloned())
}

/// Parse only unambiguous advertised technical tokens. Conflicting resolution,
/// codec or size claims remain unknown. Bare language words in titles, flags and
/// the stream's subtitle list do not establish an audio language.
pub fn advertised(text: &str) -> StreamFacts {
    let text = text.chars().take(8192).collect::<String>();
    let normalized = text
        .to_ascii_lowercase()
        .replace("h.264", "h264")
        .replace("h.265", "h265");
    let tokens = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let height = unique(
        tokens
            .iter()
            .filter(|token| {
                token.ends_with('p')
                    || token.ends_with('i')
                    || token.contains('x')
                    || matches!(**token, "4k" | "8k" | "fhd" | "uhd")
            })
            .filter_map(|token| resolution(token)),
    );
    let codec = unique(tokens.iter().filter_map(|token| VideoCodec::parse(token)));
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    let bytes =
        unique(words.iter().enumerate().filter_map(|(index, word)| {
            size(word.trim_matches(|character: char| {
                matches!(character, '[' | ']' | '(' | ')' | ',' | '|')
            }))
            .or_else(|| {
                words
                    .get(index + 1)
                    .and_then(|unit| size(&format!("{word} {unit}")))
            })
        }));
    let mut audio_languages = Vec::new();
    let mut subtitle_languages = Vec::new();
    let mut cached = Vec::new();
    let mut packs = Vec::new();
    let mut seeders = Vec::new();
    let boolean = |value: &str| match value.trim() {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    };
    for line in normalized.lines().take(64) {
        let Some((label, value)) = line.split_once(':') else {
            continue;
        };
        match label.trim() {
            "cached" => {
                if let Some(value) = boolean(value) {
                    cached.push(value);
                }
            }
            "season pack" => {
                if let Some(value) = boolean(value) {
                    packs.push(value);
                }
            }
            "seeders" => {
                if let Ok(value) = value.trim().parse::<u64>() {
                    seeders.push(value);
                }
            }
            _ => {}
        }
        let languages = match label.trim() {
            "audio" | "audio language" | "audio languages" => &mut audio_languages,
            "subtitle" | "subtitles" | "subtitle language" | "subtitle languages" => {
                &mut subtitle_languages
            }
            _ => continue,
        };
        for token in value
            .split(|character: char| !character.is_alphabetic())
            .filter(|token| !token.is_empty())
            .take(32)
        {
            if let Some(language) = audio_language(token)
                && !languages.contains(&language)
            {
                languages.push(language);
            }
        }
    }
    StreamFacts {
        height,
        bytes,
        codec,
        audio_languages,
        subtitle_languages,
        cached: unique(cached.into_iter()),
        season_pack: unique(packs.into_iter()),
        seeders: unique(seeders.into_iter()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_ranks_before_codec_and_strictness_switches_are_independent() {
        let mut rules = StreamFilterRules {
            enabled: true,
            codec_priority: vec![VideoCodec::Av1, VideoCodec::Hevc],
            excluded_video_codecs: vec![VideoCodec::Vp9],
            ..Default::default()
        }
        .resolved("ja,en");
        let facts = |language: &str, codec| StreamFacts {
            audio_languages: vec![language.into()],
            codec: Some(codec),
            ..Default::default()
        };
        let japanese_avc = facts("ja", VideoCodec::Avc);
        let english_av1 = facts("en", VideoCodec::Av1);
        let croatian_av1 = facts("hr", VideoCodec::Av1);
        assert!(rules.rank(&japanese_avc) < rules.rank(&english_av1));
        assert!(rules.rank(&english_av1) < rules.rank(&facts("en", VideoCodec::Hevc)));
        assert!(rules.accepts(&japanese_avc) && rules.accepts(&croatian_av1));
        rules.require_audio_language = true;
        assert!(rules.accepts(&japanese_avc));
        assert!(!rules.accepts(&croatian_av1));
        rules.require_audio_language = false;
        rules.require_preferred_codec = true;
        assert!(!rules.accepts(&japanese_avc));
        assert!(rules.accepts(&croatian_av1));
        rules.codec_priority.push(VideoCodec::Vp9);
        assert!(!rules.accepts(&facts("ja", VideoCodec::Vp9)));
        rules.enabled = false;
        assert!(rules.accepts(&facts("ja", VideoCodec::Vp9)));
        assert_eq!(rules.rank(&japanese_avc), rules.rank(&english_av1));
    }

    #[test]
    fn all_sort_criteria_keep_unknown_last_in_both_directions_and_compare_exact_numbers() {
        let known = StreamFacts {
            height: Some(1080),
            bytes: Some(u64::MAX - 1),
            codec: Some(VideoCodec::Av1),
            cached: Some(false),
            season_pack: Some(false),
            seeders: Some(0),
            audio_languages: vec!["ja".into()],
            subtitle_languages: vec!["hr".into()],
        };
        let unknown = StreamFacts::default();
        let mut rules = StreamFilterRules {
            enabled: true,
            codec_priority: vec![VideoCodec::Av1],
            audio_languages: vec!["ja".into()],
            subtitle_languages: vec!["hr".into()],
            ..Default::default()
        };
        for rule in default_sort_order() {
            for direction in [SortDirection::Asc, SortDirection::Desc] {
                rules.sort_order = vec![StreamSortRule { direction, ..rule }];
                assert!(
                    rules.compare(&known, &unknown).is_lt(),
                    "{:?}/{direction:?}",
                    rule.criterion
                );
                assert!(rules.compare(&unknown, &known).is_gt());
                assert!(rules.compare(&known, &known).is_eq());
            }
        }
        rules.sort_order = vec![StreamSortRule {
            criterion: SortCriterion::Size,
            direction: SortDirection::Asc,
        }];
        let mut larger = known.clone();
        larger.bytes = Some(u64::MAX);
        assert!(
            rules.compare(&known, &larger).is_lt(),
            "u64 sizes must not round through float comparison"
        );
        rules.enabled = false;
        assert!(rules.compare(&known, &larger).is_eq());
        let mut partial = StreamFilterRules {
            sort_order: vec![
                StreamSortRule {
                    criterion: SortCriterion::Seeders,
                    direction: SortDirection::Asc
                };
                2
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(partial.sort_order.len(), 8);
        assert_eq!(partial.sort_order[0].criterion, SortCriterion::Seeders);
        partial = partial.resolved_with_subtitles("en", "hr,ja");
        assert_eq!(partial.subtitle_languages, ["hr", "ja"]);
    }

    #[test]
    fn legacy_custom_audio_survives_inheritance_and_blocked_codecs_win_allow_lists() {
        let custom: StreamFilterRules =
            serde_json::from_str(r#"{"audio_languages":["ja","en"]}"#).unwrap();
        assert!(!custom.use_preferred_audio_languages);
        assert_eq!(custom.resolved("hr").audio_languages, ["ja", "en"]);
        let inherited: StreamFilterRules = serde_json::from_str("{}").unwrap();
        assert!(inherited.use_preferred_audio_languages);
        assert_eq!(inherited.resolved("hr,en").audio_languages, ["hr", "en"]);
        let mut rules = custom;
        rules.enabled = true;
        rules.use_preferred_audio_languages = true;
        rules.video_codecs = vec![VideoCodec::Hevc];
        rules.excluded_video_codecs = vec![VideoCodec::Hevc];
        assert!(!rules.accepts(&StreamFacts {
            codec: Some(VideoCodec::Hevc),
            ..Default::default()
        }));
        let effective = rules.resolved("hr");
        assert_eq!(effective.audio_languages, ["hr"]);
        assert_eq!(
            rules.audio_languages,
            ["ja", "en"],
            "resolving must retain the optional custom order"
        );
        rules.use_preferred_audio_languages = false;
        assert_eq!(rules.resolved("hr").audio_languages, ["ja", "en"]);
        rules.video_codecs.clear();
        rules.codec_priority = vec![VideoCodec::Av1];
        let avc = StreamFacts {
            codec: Some(VideoCodec::Avc),
            ..Default::default()
        };
        assert!(
            rules.accepts(&avc),
            "codec preferences rank softly by default"
        );
        rules.require_preferred_codec = true;
        assert!(!rules.accepts(&avc));
        assert!(rules.accepts(&StreamFacts::default()));
        rules.retain_unknown = false;
        assert!(!rules.accepts(&StreamFacts::default()));
        rules.require_preferred_codec = false;
        assert!(
            !rules.accepts(&StreamFacts {
                codec: Some(VideoCodec::Hevc),
                ..Default::default()
            }),
            "explicit blocks remain hard when preference strictness is off"
        );
    }

    #[test]
    fn disabled_defaults_retain_every_offer_and_unknowns_are_explicit() {
        let facts = StreamFacts::default();
        assert!(StreamFilterRules::default().accepts(&facts));
        let mut rules = StreamFilterRules {
            enabled: true,
            max_height: Some(1080),
            max_bytes: Some(8 << 30),
            video_codecs: vec![VideoCodec::Avc],
            audio_languages: vec!["hr".into()],
            require_audio_language: true,
            ..Default::default()
        };
        assert!(rules.accepts(&facts));
        rules.retain_unknown = false;
        assert!(!rules.accepts(&facts));
        let mut known = StreamFacts {
            height: Some(1080),
            bytes: Some(4 << 30),
            codec: Some(VideoCodec::Avc),
            audio_languages: vec!["hr".into()],
            ..Default::default()
        };
        assert!(rules.accepts(&known));
        known.height = Some(2160);
        assert!(!rules.accepts(&known));
        known.height = Some(1080);
        known.audio_languages = vec!["en".into()];
        assert!(!rules.accepts(&known));
        rules.require_audio_language = false;
        assert!(rules.accepts(&known));
    }
    #[test]
    fn technical_parser_rejects_conflicts_and_does_not_infer_audio_from_subtitles_or_titles() {
        let facts =
            advertised("Release.2160p.x265\nSize: 4.5 GiB\nAudio: eng / hrv\nSubtitles: Japanese");
        assert_eq!(facts.height, Some(2160));
        assert_eq!(facts.codec, Some(VideoCodec::Hevc));
        assert_eq!(facts.bytes, Some(4_831_838_208));
        assert_eq!(facts.audio_languages, ["en", "hr"]);
        let facts =
            advertised("An English Movie 1080p / 2160p x264 x265 2GB 4GB\nSubtitles: en ja");
        assert_eq!(facts.height, None);
        assert_eq!(facts.codec, None);
        assert_eq!(facts.bytes, None);
        assert!(facts.audio_languages.is_empty());
    }
    #[test]
    fn ordered_audio_preferences_rank_without_hiding_other_languages() {
        let rules = StreamFilterRules {
            enabled: true,
            audio_languages: vec!["ja".into(), "en".into()],
            ..Default::default()
        };
        let facts = |language: &str| StreamFacts {
            audio_languages: vec![language.into()],
            ..Default::default()
        };
        assert_eq!(rules.audio_rank(&facts("ja")), 0);
        assert_eq!(rules.audio_rank(&facts("en")), 1);
        assert_eq!(rules.audio_rank(&facts("hr")), usize::MAX);
        assert!(rules.accepts(&facts("hr")));
        let malformed = StreamFilterRules {
            audio_languages: vec!["ENG".into(), "en".into(), "invalid".into()],
            ..Default::default()
        }
        .normalized();
        assert_eq!(malformed.audio_languages, ["en"]);
    }
}

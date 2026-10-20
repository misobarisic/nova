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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamFacts {
    pub height: Option<u16>,
    pub bytes: Option<u64>,
    pub codec: Option<VideoCodec>,
    pub audio_languages: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StreamFilterRules {
    pub enabled: bool,
    pub max_height: Option<u16>,
    pub max_bytes: Option<u64>,
    /// An empty set accepts all video codecs.
    pub video_codecs: Vec<VideoCodec>,
    /// Ordered ISO language preferences. The first advertised match ranks first.
    pub audio_languages: Vec<String>,
    pub require_audio_language: bool,
    pub retain_unknown: bool,
}

impl Default for StreamFilterRules {
    fn default() -> Self {
        Self {
            enabled: false,
            max_height: None,
            max_bytes: None,
            video_codecs: Vec::new(),
            audio_languages: Vec::new(),
            require_audio_language: false,
            retain_unknown: true,
        }
    }
}

impl StreamFilterRules {
    pub fn normalized(mut self) -> Self {
        self.max_height = self
            .max_height
            .filter(|height| (144..=8640).contains(height));
        self.max_bytes = self.max_bytes.filter(|bytes| *bytes > 0);
        let mut codecs = Vec::new();
        for codec in self.video_codecs {
            if !codecs.contains(&codec) {
                codecs.push(codec);
            }
        }
        self.video_codecs = codecs;
        let mut languages = Vec::new();
        for value in self.audio_languages.iter().take(16) {
            if let Some(language) = crate::playback_preferences::normalize_language(value)
                && !languages.contains(&language)
            {
                languages.push(language);
            }
        }
        self.audio_languages = languages;
        self
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
            && (self.video_codecs.is_empty()
                || facts.codec.map_or(self.retain_unknown, |codec| {
                    self.video_codecs.contains(&codec)
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
        self.audio_languages
            .iter()
            .position(|wanted| {
                facts.audio_languages.iter().any(|actual| {
                    actual == wanted || actual.split('-').next() == wanted.split('-').next()
                })
            })
            .unwrap_or(usize::MAX)
    }
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
    for line in normalized.lines().take(64) {
        let Some((label, languages)) = line.split_once(':') else {
            continue;
        };
        if !matches!(label.trim(), "audio" | "audio language" | "audio languages") {
            continue;
        }
        for token in languages
            .split(|character: char| !character.is_alphabetic())
            .filter(|token| !token.is_empty())
            .take(32)
        {
            let language = audio_language(token);
            if let Some(language) = language
                && !audio_languages.contains(&language)
            {
                audio_languages.push(language);
            }
        }
    }
    StreamFacts {
        height,
        bytes,
        codec,
        audio_languages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

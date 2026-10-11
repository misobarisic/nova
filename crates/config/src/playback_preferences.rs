//! Semantic playback choices: stream-local track ids must never be persisted.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Normal,
    Commentary,
    Full,
    Forced,
    Signs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackDescriptor {
    pub language: Option<String>,
    pub kind: Option<TrackKind>,
    /// Only used for untagged tracks, and only when uniquely matched.
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackChoice {
    Off,
    Track(TrackDescriptor),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PreferenceValue {
    Audio(TrackChoice),
    Subtitles(TrackChoice),
    Speed(f32),
}

#[derive(Clone, Debug, Default)]
pub struct TitlePreferences {
    pub audio: Option<TrackChoice>,
    pub subtitles: Option<TrackChoice>,
    pub speed: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct SessionPreferences {
    pub audio_language: String,
    pub subtitle_language: String,
    pub default_speed: f32,
    pub title: TitlePreferences,
}

impl SessionPreferences {
    pub fn from_settings(settings: &crate::CacheSettings, title: TitlePreferences) -> Self {
        Self {
            audio_language: settings.default_audio_language.clone(),
            subtitle_language: settings.default_subtitle_language.clone(),
            default_speed: crate::clamp_playback_speed(settings.playback_speed),
            title,
        }
    }

    pub fn speed(&self) -> f32 {
        crate::clamp_playback_speed(self.title.speed.unwrap_or(self.default_speed))
    }
}

#[derive(Clone, Debug)]
pub struct SemanticTrack {
    pub id: i32,
    pub audio: bool,
    pub descriptor: TrackDescriptor,
    pub default: bool,
}

/// Canonicalize ISO aliases without throwing away region/script distinctions.
pub fn normalize_language(raw: &str) -> Option<String> {
    let raw = raw.trim().replace('_', "-").to_ascii_lowercase();
    let mut parts = raw.split('-');
    let base = parts.next()?;
    if matches!(base, "" | "und" | "unknown" | "off" | "auto") {
        return None;
    }
    // Bibliographic ISO 639-2 aliases are common in Matroska files.
    let base = match base {
        "alb" => "sqi",
        "arm" => "hye",
        "baq" => "eus",
        "bur" => "mya",
        "chi" => "zho",
        "cze" => "ces",
        "dut" => "nld",
        "fre" => "fra",
        "geo" => "kat",
        "ger" => "deu",
        "gre" => "ell",
        "ice" => "isl",
        "mac" => "mkd",
        "mao" => "mri",
        "may" => "msa",
        "per" => "fas",
        "rum" => "ron",
        "slo" => "slk",
        "tib" => "bod",
        "wel" => "cym",
        other => other,
    };
    let language =
        isolang::Language::from_639_1(base).or_else(|| isolang::Language::from_639_3(base))?;
    let mut tag = language
        .to_639_1()
        .unwrap_or(language.to_639_3())
        .to_owned();
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric()) || part.len() > 8 {
            return None;
        }
        tag.push('-');
        tag.push_str(part);
    }
    Some(tag)
}

fn language_rank(wanted: &str, actual: Option<&str>) -> u8 {
    match actual {
        Some(actual) if actual == wanted => 2,
        Some(actual) if actual.split('-').next() == wanted.split('-').next() => 1,
        _ => 0,
    }
}

fn match_choice(tracks: &[SemanticTrack], audio: bool, choice: &TrackChoice) -> Option<i32> {
    let TrackChoice::Track(wanted) = choice else {
        return Some(-1);
    };
    let candidates: Vec<_> = tracks
        .iter()
        .filter(|t| t.audio == audio)
        .filter(|t| wanted.kind.is_none() || t.descriptor.kind == wanted.kind)
        .filter(|t| match &wanted.language {
            Some(language) => language_rank(language, t.descriptor.language.as_deref()) > 0,
            None => wanted
                .title
                .as_ref()
                .is_some_and(|title| t.descriptor.title.as_ref() == Some(title)),
        })
        .collect();
    if wanted.language.is_none() {
        return (candidates.len() == 1).then(|| candidates[0].id);
    }
    candidates
        .into_iter()
        .max_by_key(|t| {
            (
                language_rank(
                    wanted.language.as_deref().unwrap_or(""),
                    t.descriptor.language.as_deref(),
                ),
                t.default,
                std::cmp::Reverse(t.id),
            )
        })
        .map(|t| t.id)
}

/// None delegates to mpv Automatic. -1 disables the track. Unavailable saved
/// choices fall back without changing the stored preference.
pub fn select_track(
    tracks: &[SemanticTrack],
    audio: bool,
    choice: Option<&TrackChoice>,
    global: &str,
) -> Option<i32> {
    if let Some(id) = choice.and_then(|choice| match_choice(tracks, audio, choice)) {
        return Some(id);
    }
    if !audio && global == "off" {
        return Some(-1);
    }
    if global.is_empty() {
        // Automatic preserves old behavior for untouched titles. A missing
        // explicit subtitle preference must not silently pick another language.
        return (!audio && choice.is_some()).then_some(-1);
    }
    let Some(language) = normalize_language(global) else {
        return (!audio).then_some(-1);
    };
    let preferred_kind = if audio {
        TrackKind::Normal
    } else {
        TrackKind::Full
    };
    tracks
        .iter()
        .filter(|t| t.audio == audio)
        .filter(|t| language_rank(&language, t.descriptor.language.as_deref()) > 0)
        .max_by_key(|t| {
            (
                language_rank(&language, t.descriptor.language.as_deref()),
                t.descriptor.kind.as_ref() == Some(&preferred_kind),
                t.default,
                std::cmp::Reverse(t.id),
            )
        })
        .map(|t| t.id)
        .or_else(|| (!audio).then_some(-1))
}

pub fn preference_key(media_type: &str, id: &str, control: i32) -> String {
    // JSON tuple keys avoid collisions from provider ids containing separators.
    serde_json::to_string(&(media_type, id, control)).expect("string tuple serializes")
}

/// Search ISO names/codes; include all two-letter languages in the empty list.
pub fn language_choices(query: &str) -> Vec<(String, String)> {
    let query = query.to_lowercase();
    isolang::Language::match_names(|_| true)
        .filter(|language| {
            if query.is_empty() {
                language.to_639_1().is_some()
            } else {
                language.to_name().to_lowercase().contains(&query)
                    || language.to_639_3().contains(&query)
                    || language
                        .to_639_1()
                        .is_some_and(|code| code.contains(&query))
            }
        })
        .map(|language| {
            let code = language
                .to_639_1()
                .unwrap_or(language.to_639_3())
                .to_owned();
            (code.clone(), format!("{} ({code})", language.to_name()))
        })
        .collect()
}

pub fn language_label(code: &str) -> String {
    let base = code.split('-').next().unwrap_or(code);
    isolang::Language::from_639_1(base)
        .or_else(|| isolang::Language::from_639_3(base))
        .map(|language| format!("{} ({code})", language.to_name()))
        .unwrap_or_else(|| code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: i32, language: &str, kind: TrackKind) -> SemanticTrack {
        SemanticTrack {
            id,
            audio: false,
            default: false,
            descriptor: TrackDescriptor {
                language: normalize_language(language),
                kind: Some(kind),
                title: None,
            },
        }
    }
    #[test]
    fn aliases_and_regions() {
        assert_eq!(normalize_language("ENG"), Some("en".into()));
        assert_eq!(normalize_language("ger"), Some("de".into()));
        assert_eq!(normalize_language("pt_BR"), Some("pt-br".into()));
        assert_eq!(normalize_language("Track 1"), None);
        assert_eq!(normalize_language("und"), None);
    }
    #[test]
    fn saved_kind_language_and_missing_fallback() {
        let tracks = vec![
            track(9, "eng", TrackKind::Forced),
            track(3, "en", TrackKind::Full),
            track(5, "ja", TrackKind::Full),
        ];
        let saved = TrackChoice::Track(tracks[0].descriptor.clone());
        assert_eq!(select_track(&tracks, false, Some(&saved), "ja"), Some(9));
        assert_eq!(
            select_track(&tracks[1..], false, Some(&saved), "ja"),
            Some(5)
        );
        assert_eq!(
            select_track(&tracks[1..], false, Some(&saved), "hr"),
            Some(-1)
        );
        assert_eq!(
            select_track(&tracks[1..], false, Some(&saved), ""),
            Some(-1)
        );
        assert_eq!(select_track(&tracks, false, None, ""), None);
        assert_eq!(select_track(&tracks, false, None, "en"), Some(3));
        assert_eq!(
            select_track(&tracks, false, Some(&TrackChoice::Off), "en"),
            Some(-1)
        );
    }
    #[test]
    fn exact_region_beats_base_and_untagged_requires_unique_title() {
        let mut tracks = vec![
            track(1, "pt", TrackKind::Full),
            track(7, "pt-br", TrackKind::Full),
        ];
        assert_eq!(select_track(&tracks, false, None, "pt-BR"), Some(7));
        tracks[0].descriptor.language = None;
        tracks[0].descriptor.title = Some("english full".into());
        let saved = TrackChoice::Track(tracks[0].descriptor.clone());
        assert_eq!(select_track(&tracks, false, Some(&saved), "off"), Some(1));
        tracks.push(tracks[0].clone());
        assert_eq!(select_track(&tracks, false, Some(&saved), "off"), Some(-1));
    }
}

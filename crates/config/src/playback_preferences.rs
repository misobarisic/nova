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

/// Text subtitle appearance. Units follow mpv's 720p-scaled subtitle canvas;
/// positive delay displays subtitles later. Authored ASS styling is preserved
/// unless the viewer explicitly opts into overriding it. Bitmap appearance is
/// controlled by the source and cannot be restyled.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleStyle {
    pub delay: f32,
    /// None retains the native mpv version's default on this device.
    pub font_size: Option<f32>,
    pub margin: f32,
    pub position: f32,
    pub outline: Option<f32>,
    pub color: u8,
    pub override_ass: bool,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        // Font and outline defaults differ across supported mpv versions.
        Self {
            delay: 0.0,
            font_size: None,
            margin: 19.0,
            position: 100.0,
            outline: None,
            color: 0,
            override_ass: false,
        }
    }
}

impl SubtitleStyle {
    pub fn normalized(self) -> Self {
        fn bounded(v: f32, min: f32, max: f32, fallback: f32) -> f32 {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                fallback
            }
        }
        Self {
            delay: (bounded(self.delay, -60.0, 60.0, 0.0) * 10.0).round() / 10.0,
            font_size: self
                .font_size
                .map(|v| (bounded(v, 16.0, 100.0, 55.0) * 100.0).round() / 100.0),
            margin: bounded(self.margin, 0.0, 200.0, 19.0).round(),
            position: bounded(self.position, 0.0, 100.0, 100.0).round(),
            outline: self
                .outline
                .map(|v| (bounded(v, 0.0, 10.0, 3.0) * 100.0).round() / 100.0),
            color: self.color.min(2),
            override_ass: self.override_ass,
        }
    }

    /// Resolve native-only values without persisting a device/version baseline.
    pub fn resolved(self, native: Self) -> Self {
        let mut style = self.normalized();
        style.font_size = style.font_size.or(native.font_size);
        style.outline = style.outline.or(native.outline);
        style
    }

    pub fn values(self) -> [f32; 7] {
        let s = self.normalized();
        [
            s.delay,
            s.font_size.unwrap_or(55.0),
            s.margin,
            s.position,
            s.outline.unwrap_or(3.0),
            s.color as f32,
            if s.override_ass { 1.0 } else { 0.0 },
        ]
    }

    pub fn with_value(mut self, index: i32, value: f32) -> Self {
        if !value.is_finite() {
            return self.normalized();
        }
        match index {
            0 => self.delay = value,
            1 => self.font_size = Some(value),
            2 => self.margin = value,
            3 => self.position = value,
            4 => self.outline = Some(value),
            5 => self.color = value.round().clamp(0.0, 2.0) as u8,
            6 => self.override_ass = value >= 0.5,
            _ => {}
        }
        self.normalized()
    }
}

/// Each edited field inherits independently; changing timing must not freeze
/// appearance when the viewer later changes a global default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleStylePatch {
    pub delay: Option<f32>,
    pub font_size: Option<f32>,
    pub margin: Option<f32>,
    pub position: Option<f32>,
    pub outline: Option<f32>,
    pub color: Option<u8>,
    pub override_ass: Option<bool>,
}

impl SubtitleStylePatch {
    pub fn apply(self, default: SubtitleStyle) -> SubtitleStyle {
        SubtitleStyle {
            delay: self.delay.unwrap_or(default.delay),
            font_size: self.font_size.or(default.font_size),
            margin: self.margin.unwrap_or(default.margin),
            position: self.position.unwrap_or(default.position),
            outline: self.outline.or(default.outline),
            color: self.color.unwrap_or(default.color),
            override_ass: self.override_ass.unwrap_or(default.override_ass),
        }
        .normalized()
    }

    pub fn with_value(mut self, index: i32, value: f32) -> Self {
        if !value.is_finite() {
            return self;
        }
        let value = SubtitleStyle::default().with_value(index, value);
        match index {
            0 => self.delay = Some(value.delay),
            1 => self.font_size = value.font_size,
            2 => self.margin = Some(value.margin),
            3 => self.position = Some(value.position),
            4 => self.outline = value.outline,
            5 => self.color = Some(value.color),
            6 => self.override_ass = Some(value.override_ass),
            _ => {}
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PreferenceValue {
    Audio(TrackChoice),
    Subtitles(TrackChoice),
    Speed(f32),
    SubtitleStyle(SubtitleStylePatch),
    IntroSkip(crate::IntroSkipMode),
}

#[derive(Clone, Debug, Default)]
pub struct TitlePreferences {
    pub audio: Option<TrackChoice>,
    pub subtitles: Option<TrackChoice>,
    pub speed: Option<f32>,
    pub subtitle_style: Option<SubtitleStylePatch>,
    pub intro_skip: Option<crate::IntroSkipMode>,
}

#[derive(Clone, Debug)]
pub struct SessionPreferences {
    pub audio_language: String,
    pub subtitle_language: String,
    pub default_speed: f32,
    pub default_subtitle_style: SubtitleStyle,
    pub title: TitlePreferences,
}

impl SessionPreferences {
    pub fn from_settings(settings: &crate::CacheSettings, title: TitlePreferences) -> Self {
        Self {
            audio_language: settings.default_audio_language.clone(),
            subtitle_language: settings.default_subtitle_language.clone(),
            default_speed: crate::clamp_playback_speed(settings.playback_speed),
            default_subtitle_style: settings.subtitle_style.normalized(),
            title,
        }
    }

    pub fn intro_skip_mode(&self, global: crate::IntroSkipMode) -> crate::IntroSkipMode {
        self.title.intro_skip.unwrap_or(global)
    }

    pub fn subtitle_style(&self) -> SubtitleStyle {
        self.title
            .subtitle_style
            .unwrap_or_default()
            .apply(self.default_subtitle_style)
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

/// Ordered mpv language hints. Retaining the existing string fields keeps old
/// single-language settings and per-field sync/device overrides compatible.
pub fn language_preferences(raw: &str) -> Vec<String> {
    let mut languages = Vec::new();
    for language in raw.split(',').filter_map(normalize_language) {
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    languages
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
    let preferred_kind = if audio {
        TrackKind::Normal
    } else {
        TrackKind::Full
    };
    language_preferences(global)
        .into_iter()
        .find_map(|language| {
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
        })
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
    #[test]
    fn intro_modes_roundtrip_and_title_override_inherits_current_global() {
        use crate::IntroSkipMode;
        let mut prefs = SessionPreferences::from_settings(
            &crate::CacheSettings::default(),
            TitlePreferences::default(),
        );
        assert_eq!(
            prefs.intro_skip_mode(IntroSkipMode::Prompt),
            IntroSkipMode::Prompt
        );
        assert_eq!(
            prefs.intro_skip_mode(IntroSkipMode::Off),
            IntroSkipMode::Off
        );
        for mode in [
            IntroSkipMode::Off,
            IntroSkipMode::Prompt,
            IntroSkipMode::Auto,
        ] {
            assert_eq!(IntroSkipMode::from_index(mode.index()), Some(mode));
            let value = PreferenceValue::IntroSkip(mode);
            assert_eq!(
                serde_json::from_str::<PreferenceValue>(&serde_json::to_string(&value).unwrap())
                    .unwrap(),
                value
            );
            prefs.title.intro_skip = Some(mode);
            assert_eq!(prefs.intro_skip_mode(IntroSkipMode::Off), mode);
            assert_eq!(prefs.intro_skip_mode(IntroSkipMode::Auto), mode);
        }
        prefs.title.intro_skip = None;
        assert_eq!(
            prefs.intro_skip_mode(IntroSkipMode::Prompt),
            IntroSkipMode::Prompt
        );
        assert_eq!(IntroSkipMode::from_index(-1), None);
        assert_eq!(IntroSkipMode::from_index(3), None);
    }

    #[test]
    fn subtitle_defaults_migrate_and_partial_title_edits_inherit() {
        let settings: crate::CacheSettings =
            serde_json::from_str(r#"{"enabled":false,"format":"webp","quality":85}"#).unwrap();
        assert_eq!(settings.subtitle_style, SubtitleStyle::default());
        let partial: SubtitleStyle = serde_json::from_str(r#"{"delay":0.3}"#).unwrap();
        assert_eq!(partial.font_size, None);
        let patch = SubtitleStylePatch::default().with_value(0, 0.36);
        let mut prefs = SessionPreferences::from_settings(
            &settings,
            TitlePreferences {
                subtitle_style: Some(patch),
                ..Default::default()
            },
        );
        assert_eq!(prefs.subtitle_style().delay, 0.4);
        prefs.default_subtitle_style.font_size = Some(70.0);
        prefs.default_subtitle_style.delay = -1.0;
        assert_eq!(prefs.subtitle_style().delay, 0.4);
        assert_eq!(prefs.subtitle_style().font_size, Some(70.0));
        let raw = serde_json::to_string(&PreferenceValue::SubtitleStyle(patch)).unwrap();
        assert_eq!(
            serde_json::from_str::<PreferenceValue>(&raw).unwrap(),
            PreferenceValue::SubtitleStyle(patch)
        );
        assert_eq!(
            serde_json::from_str::<PreferenceValue>(r#"{"Speed":1.25}"#).unwrap(),
            PreferenceValue::Speed(1.25)
        );
        prefs.title.subtitle_style = None;
        assert_eq!(prefs.subtitle_style().delay, -1.0);
    }

    #[test]
    fn subtitle_native_defaults_remain_unpersisted_across_device_versions() {
        let saved = SubtitleStyle::default().with_value(0, 0.3);
        for (font, outline) in [(38.0, 1.65), (55.0, 3.0)] {
            let native = SubtitleStyle {
                font_size: Some(font),
                outline: Some(outline),
                ..Default::default()
            };
            assert_eq!(saved.resolved(native).values()[1], font);
            assert_eq!(saved.resolved(native).values()[4], outline);
            let edited = saved.with_value(4, 1.75);
            assert_eq!(edited.resolved(native).outline, Some(1.75));
            assert_eq!(edited.resolved(native).font_size, Some(font));
        }
        let json = serde_json::to_value(saved).unwrap();
        assert!(json["font_size"].is_null());
        assert!(json["outline"].is_null());
    }

    #[test]
    fn subtitle_values_reject_nonfinite_and_bound_all_controls() {
        let defaults = SubtitleStyle::default();
        assert_eq!(defaults.with_value(0, f32::NAN), defaults);
        assert_eq!(defaults.with_value(0, 80.0).delay, 60.0);
        assert_eq!(defaults.with_value(1, -1.0).font_size, Some(16.0));
        assert_eq!(defaults.with_value(2, 300.0).margin, 200.0);
        assert_eq!(defaults.with_value(3, -1.0).position, 0.0);
        assert_eq!(defaults.with_value(4, 99.0).outline, Some(10.0));
        assert_eq!(defaults.with_value(5, 99.0).color, 2);
        assert!(defaults.with_value(6, 1.0).override_ass);
    }

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
    fn ordered_languages_fall_back_without_overriding_saved_tracks() {
        assert_eq!(
            language_preferences("ENG, ja, en, pt_BR,invalid"),
            ["en", "ja", "pt-br"]
        );
        let tracks = vec![
            track(1, "en", TrackKind::Full),
            track(2, "ja", TrackKind::Full),
            track(3, "pt", TrackKind::Full),
            track(4, "pt-br", TrackKind::Full),
        ];
        assert_eq!(select_track(&tracks, false, None, "hr,ja,en"), Some(2));
        assert_eq!(select_track(&tracks, false, None, "hr,en,ja"), Some(1));
        assert_eq!(select_track(&tracks, false, None, "pt-br,en"), Some(4));
        assert_eq!(select_track(&tracks[..3], false, None, "pt-br,en"), Some(3));
        assert_eq!(select_track(&tracks, false, None, "hr,de"), Some(-1));
        let saved = TrackChoice::Track(tracks[0].descriptor.clone());
        assert_eq!(select_track(&tracks, false, Some(&saved), "ja,en"), Some(1));
        assert_eq!(
            select_track(&tracks, false, Some(&TrackChoice::Off), "ja,en"),
            Some(-1)
        );
        let mut audio = tracks.clone();
        for track in &mut audio {
            track.audio = true;
        }
        assert_eq!(select_track(&audio, true, None, "hr,ja,en"), Some(2));
        assert_eq!(select_track(&audio, true, None, "hr,de"), None);
        let settings = crate::CacheSettings {
            default_audio_language: "ja,en".into(),
            default_subtitle_language: "hr,en".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: crate::CacheSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.default_audio_language, "ja,en");
        assert_eq!(restored.default_subtitle_language, "hr,en");
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

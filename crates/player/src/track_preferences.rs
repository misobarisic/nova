//! Raw track metadata stays separate from localized popup labels.
use nova_config::playback_preferences::{SemanticTrack, TrackDescriptor, TrackKind};

pub(super) fn parse_tracks(raw: &str) -> Vec<SemanticTrack> {
    let Ok(serde_json::Value::Array(tracks)) = serde_json::from_str(raw) else {
        return Vec::new();
    };
    tracks
        .iter()
        .filter_map(|t| {
            let audio = match t.get("type")?.as_str()? {
                "audio" => true,
                "sub" => false,
                _ => return None,
            };
            let id = i32::try_from(t.get("id")?.as_i64()?).ok()?;
            let title = t
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_lowercase();
            let flag = |name| t.get(name).and_then(|v| v.as_bool()).unwrap_or(false);
            // Older mpv/container combinations omit flags. Recognize explicit
            // labels only; absent language is never inferred from display labels.
            let words: Vec<_> = title.split(|c: char| !c.is_alphanumeric()).collect();
            let kind = if audio {
                if flag("commentary") || words.contains(&"commentary") {
                    TrackKind::Commentary
                } else {
                    TrackKind::Normal
                }
            } else if flag("forced") || words.contains(&"forced") {
                TrackKind::Forced
            } else if words.contains(&"signs") {
                TrackKind::Signs
            } else {
                TrackKind::Full
            };
            Some(SemanticTrack {
                id,
                audio,
                default: flag("default"),
                descriptor: TrackDescriptor {
                    language: t
                        .get("lang")
                        .and_then(|v| v.as_str())
                        .and_then(nova_config::playback_preferences::normalize_language),
                    kind: Some(kind),
                    title: (!title.is_empty()).then_some(title),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_mpv_receives_global_languages_title_speed_and_subtitle_off() {
        use nova_config::playback_preferences::{
            SessionPreferences, TitlePreferences, TrackChoice,
        };
        let mpv = libmpv2::Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        let native = super::super::native_subtitle_defaults(&mpv);
        let settings = nova_config::CacheSettings {
            default_audio_language: "ja,en".into(),
            default_subtitle_language: "hr,en".into(),
            playback_speed: 1.15,
            ..Default::default()
        };
        let prefs = SessionPreferences::from_settings(
            &settings,
            TitlePreferences {
                subtitles: Some(TrackChoice::Off),
                speed: Some(1.6),
                ..Default::default()
            },
        );
        super::super::apply_preference_options(&mpv, &prefs, native);
        assert_eq!(mpv.get_property::<String>("alang").unwrap(), "ja,en");
        assert_eq!(mpv.get_property::<String>("slang").unwrap(), "hr,en");
        assert_eq!(mpv.get_property::<String>("sid").unwrap(), "no");
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.6_f32 as f64);
        let prefs = SessionPreferences::from_settings(&settings, TitlePreferences::default());
        super::super::apply_preference_options(&mpv, &prefs, native);
        assert_eq!(mpv.get_property::<String>("sid").unwrap(), "auto");
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.15_f32 as f64);
    }
    #[test]
    fn native_subtitle_settings_preserve_defaults_and_apply_title_patch_on_reload() {
        use nova_config::playback_preferences::{
            SessionPreferences, SubtitleStyle, SubtitleStylePatch, TitlePreferences,
        };
        let mpv = libmpv2::Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")
        })
        .unwrap();
        let properties = [
            "sub-delay",
            "sub-font-size",
            "sub-margin-x",
            "sub-pos",
            "sub-border-size",
        ];
        let mut before = properties.map(|key| mpv.get_property::<f64>(key).unwrap());
        before[1] = SubtitleStyle::DEFAULT_FONT_SIZE as f64;
        let native = super::super::native_subtitle_defaults(&mpv);
        super::super::apply_subtitle_style(&mpv, SubtitleStyle::default(), native);
        assert_eq!(
            properties.map(|key| mpv.get_property::<f64>(key).unwrap()),
            before
        );
        assert_eq!(
            mpv.get_property::<String>("sub-ass-override").unwrap(),
            "yes"
        );
        let settings = nova_config::CacheSettings {
            subtitle_style: SubtitleStyle {
                font_size: Some(70.0),
                margin: 80.0,
                position: 85.0,
                outline: Some(5.0),
                color: 1,
                override_ass: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let prefs = SessionPreferences::from_settings(
            &settings,
            TitlePreferences {
                subtitle_style: Some(SubtitleStylePatch::default().with_value(0, -0.5)),
                ..Default::default()
            },
        );
        // This same helper runs for fresh loads and Android renderer reloads.
        for _ in 0..2 {
            super::super::apply_preference_options(&mpv, &prefs, native);
            assert_eq!(
                properties.map(|key| mpv.get_property::<f64>(key).unwrap()),
                [-0.5, 70.0, 80.0, 85.0, 5.0]
            );
            assert_eq!(
                mpv.get_property::<String>("sub-ass-override").unwrap(),
                "force"
            );
        }
        super::super::apply_subtitle_style(&mpv, SubtitleStyle::default(), native);
        assert_eq!(
            properties.map(|key| mpv.get_property::<f64>(key).unwrap()),
            before,
            "clearing overrides restores Nova's text size and the native outline"
        );
        assert_eq!(
            mpv.get_property::<String>("sub-ass-override").unwrap(),
            "yes"
        );
    }

    #[test]
    fn raw_metadata_is_not_a_track_number_label() {
        let tracks = parse_tracks(
            r#"[{"type":"audio","id":7,"lang":"eng","commentary":true},{"type":"sub","id":2,"title":"Signs & Songs"},{"type":"sub","id":4,"forced":true}]"#,
        );
        assert_eq!(tracks[0].descriptor.language.as_deref(), Some("en"));
        assert_eq!(tracks[0].descriptor.kind, Some(TrackKind::Commentary));
        assert_eq!(tracks[1].descriptor.language, None);
        assert_eq!(tracks[1].descriptor.kind, Some(TrackKind::Signs));
        assert_eq!(tracks[2].descriptor.kind, Some(TrackKind::Forced));
    }
}

//! Compact source facts for the OSD; never infer codecs or HDR from release names.
use libmpv2::Mpv;
use serde_json::Value;

pub(super) fn read(mpv: &Mpv) -> Vec<slint::SharedString> {
    let node = |name| {
        mpv.get_property::<String>(name)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null)
    };
    labels(
        &node("video-params"),
        &mpv.get_property::<String>("video-format")
            .unwrap_or_default(),
        &node("current-tracks/audio"),
        &node("audio-params"),
    )
    .into_iter()
    .map(Into::into)
    .collect()
}

fn labels(video: &Value, codec: &str, audio: &Value, params: &Value) -> Vec<String> {
    let mut result = Vec::new();
    let w = video["w"].as_u64().unwrap_or(0);
    let h = video["h"].as_u64().unwrap_or(0);
    if w > 0 && h > 0 {
        let (long, short) = (w.max(h), w.min(h));
        result.push(match (long, short) {
            (7680.., _) | (_, 4320..) => "8K".into(),
            (3840.., _) | (_, 2160..) => "4K".into(),
            (2560.., _) | (_, 1440..) => "1440p".into(),
            (1920.., _) | (_, 1080..) => "1080p".into(),
            (1280.., _) | (_, 720..) => "720p".into(),
            _ => format!("{w}×{h}"),
        });
    }
    if let Some(codec) = match codec {
        "hevc" => Some("HEVC"),
        "h264" => Some("H.264"),
        "av1" => Some("AV1"),
        "vp9" => Some("VP9"),
        "vp8" => Some("VP8"),
        "mpeg2video" => Some("MPEG-2"),
        "mpeg4" => Some("MPEG-4"),
        _ => None,
    } {
        result.push(codec.into());
    }
    // PQ alone does not establish HDR10+ or Dolby Vision capability.
    match video["gamma"].as_str() {
        Some("pq") => result.push("HDR".into()),
        Some("hlg") => result.push("HLG".into()),
        _ => {}
    }
    if !audio.is_null() {
        let lang = audio["lang"].as_str().unwrap_or_default();
        let lang = nova_config::playback_preferences::language_label(lang);
        let lang = lang.split(" (").next().unwrap_or_default();
        let channels = params["channels"].as_str().unwrap_or_default();
        let channels = match channels {
            "stereo" => "2.0",
            "mono" => "1.0",
            value => value.split('(').next().unwrap_or_default(),
        };
        let value = [lang, channels]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !value.is_empty() {
            result.push(value);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cropped_and_portrait_video_keep_source_resolution_and_hdr_is_conservative() {
        for (w, h) in [(3840, 1600), (1600, 3840)] {
            assert_eq!(
                labels(
                    &json!({"w": w, "h": h, "gamma": "pq"}),
                    "hevc",
                    &json!({"lang": "eng"}),
                    &json!({"channels": "5.1(side)"}),
                ),
                ["4K", "HEVC", "HDR", "English 5.1"]
            );
        }
        assert!(labels(&Value::Null, "", &Value::Null, &Value::Null).is_empty());
        assert_eq!(
            labels(
                &json!({"w": 1920, "h": 800}),
                "av1",
                &Value::Null,
                &Value::Null
            ),
            ["1080p", "AV1"]
        );
    }
}

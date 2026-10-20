//! Embedded chapter metadata, owned by the loaded source rather than its name.
use libmpv2::Mpv;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chapter {
    pub position: f32,
    pub title: String,
}

pub(super) fn read(mpv: &Mpv, duration: f64) -> Vec<Chapter> {
    mpv.get_property::<String>("chapter-list")
        .ok()
        .map(|raw| parse(&raw, duration))
        .unwrap_or_default()
}

fn parse(raw: &str, duration: f64) -> Vec<Chapter> {
    if !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str(raw) else {
        return Vec::new();
    };
    let mut chapters: Vec<_> = rows
        .iter()
        .take(256)
        .enumerate()
        .filter_map(|(index, row)| {
            let position = row["time"].as_f64()?;
            if !position.is_finite()
                || position < 0.0
                || position >= duration
                || position > f32::MAX as f64
            {
                return None;
            }
            let title = row["title"].as_str().unwrap_or_default().trim();
            Some(Chapter {
                position: position as f32,
                title: if title.is_empty() {
                    format!("{} {}", nova_ui::backend_text::tr("Chapter"), index + 1)
                } else {
                    title.chars().take(256).collect()
                },
            })
        })
        .collect();
    chapters.sort_by(|a, b| a.position.total_cmp(&b.position));
    chapters.dedup_by(|a, b| a.position == b.position);
    chapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_chapters_are_sorted_bounded_and_not_invented() {
        let chapters = parse(
            r#"[{"time":12,"title":"Second"},{"time":0,"title":"Opening"},{"time":12,"title":"Duplicate"},{"time":-1},{"time":60},{"title":"Missing"}]"#,
            60.0,
        );
        assert_eq!(chapters.len(), 2);
        assert_eq!(
            (chapters[0].position, chapters[0].title.as_str()),
            (0.0, "Opening")
        );
        assert_eq!(
            (chapters[1].position, chapters[1].title.as_str()),
            (12.0, "Second")
        );
        assert!(parse("[]", 60.0).is_empty());
        assert!(parse("invalid", 60.0).is_empty());
        assert!(parse(r#"[{"time":0}]"#, f64::NAN).is_empty());
        assert_eq!(parse(r#"[{"time":0}]"#, 60.0)[0].title, "Chapter 1");
    }
}

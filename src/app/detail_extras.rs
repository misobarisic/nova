//! Optional, bounded presentation metadata from installed detail sources.
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(super) struct DetailExtras {
    pub runtime: String,
    pub rating: String,
    pub status: String,
    pub country: String,
    pub language: String,
    pub cast: Vec<String>,
    pub directors: Vec<String>,
    pub writers: Vec<String>,
    pub trailers: Vec<DetailLink>,
    pub related: Vec<MetaPreview>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct DetailLink {
    pub title: String,
    pub url: String,
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect::<String>()
        .trim()
        .to_string()
}
fn names(values: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for name in values
        .iter()
        .take(100)
        .map(|s| clean(s))
        .filter(|s| !s.is_empty())
    {
        if !result
            .iter()
            .any(|old: &String| old.eq_ignore_ascii_case(&name))
        {
            result.push(name);
        }
    }
    result
}
pub(super) fn web_url(value: &str) -> Option<String> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

impl DetailExtras {
    pub(super) fn from_item(item: &MetaItem) -> Self {
        let extra = |key: &str| item.extra.get(key).or_else(|| item.preview.extra.get(key));
        let field = |key: &str| match extra(key) {
            Some(serde_json::Value::String(s)) => clean(s),
            Some(serde_json::Value::Array(values)) => names(
                &values
                    .iter()
                    .take(100)
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>(),
            )
            .join(", "),
            _ => String::new(),
        };
        let mut result = Self {
            runtime: clean(item.preview.runtime.as_deref().unwrap_or_default()),
            rating: item
                .preview
                .rating_str()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
                .map(|v| format!("{v:.1}"))
                .unwrap_or_default(),
            status: clean(item.status.as_deref().unwrap_or_default()),
            country: field("country"),
            language: field("language"),
            cast: names(&item.cast),
            directors: names(&item.director),
            writers: names(&item.writer),
            ..Default::default()
        };
        for stream in item.trailer_streams.iter().take(12) {
            if let Some(url) = stream.web_url().and_then(|s| web_url(&s)) {
                let title = stream
                    .name
                    .as_deref()
                    .or(stream.title_legacy.as_deref())
                    .unwrap_or_default();
                result.add_trailer(
                    if title == item.preview.title() {
                        ""
                    } else {
                        title
                    },
                    url,
                );
            }
        }
        // Cinemeta's trailers extension uses a YouTube `source`, whereas
        // the standard trailerStreams resource uses Stream::yt_id.
        if let Some(values) = extra("trailers").and_then(serde_json::Value::as_array) {
            for value in values.iter().take(12) {
                let title = value["name"]
                    .as_str()
                    .or_else(|| value["title"].as_str())
                    .unwrap_or_default();
                let source = value["source"]
                    .as_str()
                    .or_else(|| value["ytId"].as_str())
                    .unwrap_or_default();
                let url = value["url"].as_str().and_then(web_url).or_else(|| {
                    (!source.is_empty()
                        && source.len() <= 128
                        && source
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)))
                    .then(|| format!("https://www.youtube.com/watch?v={source}"))
                });
                if let Some(url) = url {
                    result.add_trailer(title, url);
                }
            }
        }
        // These optional provider extensions are explicit title suggestions;
        // never manufacture recommendations from a shared word or genre.
        for key in ["recommendations", "related", "collection"] {
            if let Some(value) = extra(key) {
                let rows = value
                    .as_array()
                    .or_else(|| value.get("metas").and_then(serde_json::Value::as_array))
                    .or_else(|| value.get("parts").and_then(serde_json::Value::as_array));
                for value in rows.into_iter().flatten().take(24) {
                    if let Ok(preview) = serde_json::from_value::<MetaPreview>(value.clone())
                        && !preview.id.is_empty()
                        && preview.id.len() <= 512
                        && !preview.id.chars().any(char::is_control)
                        && !preview.type_.is_empty()
                        && preview.type_.len() <= 64
                        && !clean(&preview.title()).is_empty()
                        && (preview.id != item.preview.id || preview.type_ != item.preview.type_)
                        && !result
                            .related
                            .iter()
                            .any(|p| p.id == preview.id && p.type_ == preview.type_)
                        && result.related.len() < 24
                    {
                        result.related.push(preview);
                    }
                }
            }
        }
        result
    }
    fn add_trailer(&mut self, title: &str, url: String) {
        if self.trailers.len() < 12 && !self.trailers.iter().any(|link| link.url == url) {
            self.trailers.push(DetailLink {
                title: clean(title),
                url,
            });
        }
    }
    /// Thin refreshes must not erase optional fields supplied by a richer source.
    pub(super) fn merge(&mut self, fresh: &Self) -> bool {
        let before = self.clone();
        for (old, new) in [
            (&mut self.runtime, &fresh.runtime),
            (&mut self.rating, &fresh.rating),
            (&mut self.status, &fresh.status),
            (&mut self.country, &fresh.country),
            (&mut self.language, &fresh.language),
        ] {
            if !new.is_empty() {
                old.clone_from(new);
            }
        }
        for (old, new) in [
            (&mut self.cast, &fresh.cast),
            (&mut self.directors, &fresh.directors),
            (&mut self.writers, &fresh.writers),
        ] {
            if !new.is_empty() {
                old.clone_from(new);
            }
        }
        if !fresh.trailers.is_empty() {
            self.trailers.clone_from(&fresh.trailers);
        }
        if !fresh.related.is_empty() {
            self.related.clone_from(&fresh.related);
        }
        *self != before
    }
}

impl Bridge {
    pub(super) fn publish_detail_extras(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let state = self.shared.lock().unwrap();
        let Some(modal) = state.modal_item.as_ref() else {
            app.set_selected_details(Default::default());
            return;
        };
        let extras = &modal.details;
        let strings = |values: &[String]| {
            Rc::new(VecModel::from(
                values.iter().map(SharedString::from).collect::<Vec<_>>(),
            ))
            .into()
        };
        app.set_selected_details(crate::DetailInfo {
            facts: Rc::new(VecModel::from(
                [
                    ("IMDb rating", &extras.rating),
                    ("Runtime", &extras.runtime),
                    ("Status", &extras.status),
                    ("Country", &extras.country),
                    ("Language", &extras.language),
                ]
                .into_iter()
                .filter(|(_, value)| !value.is_empty())
                .map(|(label, value)| crate::DetailActionRow {
                    title: text::tr(label).into(),
                    subtitle: value.clone().into(),
                    target: Default::default(),
                })
                .collect::<Vec<_>>(),
            ))
            .into(),
            cast: strings(&extras.cast),
            directors: strings(&extras.directors),
            writers: strings(&extras.writers),
            trailers: Rc::new(VecModel::from(
                extras
                    .trailers
                    .iter()
                    .enumerate()
                    .map(|(index, link)| crate::DetailActionRow {
                        title: if link.title.is_empty() {
                            if extras.trailers.len() == 1 {
                                text::tr("Watch trailer").into()
                            } else {
                                text::detail_trailer_label(index + 1).into()
                            }
                        } else {
                            link.title.clone().into()
                        },
                        subtitle: text::tr("Opens in your browser").into(),
                        target: link.url.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
            related: Rc::new(VecModel::from(
                extras
                    .related
                    .iter()
                    .map(|preview| crate::DetailActionRow {
                        title: clean(&preview.title()).into(),
                        subtitle: preview.year_str().unwrap_or_default().into(),
                        target: serde_json::to_string(&(&preview.type_, &preview.id))
                            .unwrap_or_default()
                            .into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        });
    }

    pub(super) fn detail_extra_picked(&self, kind: i32, target: &str) {
        let Some(app) = self.app().filter(|app| {
            app.get_modal_visible()
                && !app.get_player_open()
                && !app.get_stream_selector_open()
                && !app.get_tracking_open()
        }) else {
            return;
        };
        let details = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|m| m.details.clone());
        let Some(details) = details else {
            return;
        };
        match kind {
            0 if details
                .cast
                .iter()
                .chain(&details.directors)
                .chain(&details.writers)
                .any(|name| name == target) =>
            {
                self.modal_closed();
                app.set_modal_visible(false);
                app.invoke_home_bridge_discover_picked();
                self.submit_search(target);
            }
            1 if details.trailers.iter().any(|link| link.url == target) => {
                if let Some(url) = web_url(target) {
                    let _ = crate::player::open_browser(&url);
                }
            }
            2 => {
                if let Some(preview) = details
                    .related
                    .iter()
                    .find(|preview| {
                        serde_json::to_string(&(&preview.type_, &preview.id))
                            .is_ok_and(|key| key == target)
                    })
                    .cloned()
                {
                    self.save_detail_snapshot();
                    self.open_preview(preview, 0, false, None, false);
                    // Related picks reuse the mounted Detail shell. Return to
                    // its header instead of leaving the new title at the old
                    // recommendation row's scroll position.
                    (*app)
                        .global::<nova_ui_shared::DetailContent>()
                        .invoke_scroll_list_to_top(-1_000_000.0);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detail_metadata_retains_optional_fields_and_deduplicates_trailers() {
        let item: MetaItem = serde_json::from_value(json!({
            "id":"tt1", "type":"movie", "name":"Example", "runtime":"120 min", "imdbRating":8.2,
            "cast":[" Alice ","alice","Bob"], "director":["Director"], "writer":["Writer"],
            "country":["US","UK"], "language":"English", "status":"released",
            "trailerStreams":[{"ytId":"abc-123_X","name":"Official trailer"}],
            "trailers":[{"source":"abc-123_X"},{"source":"second","name":"Teaser"}],
            "recommendations":[{"id":"tt2","type":"movie","name":"Related"}, {"id":"tt1","type":"movie","name":"Self"}, {"name":"Broken"}]
        })).unwrap();
        let details = DetailExtras::from_item(&item);
        assert_eq!(details.cast, ["Alice", "Bob"]);
        assert_eq!(details.country, "US, UK");
        assert_eq!(details.rating, "8.2");
        assert_eq!(details.trailers.len(), 2);
        assert_eq!(details.trailers[0].title, "Official trailer");
        assert_eq!(details.related.len(), 1);
        assert_eq!(details.related[0].id, "tt2");
        let mut restored: DetailExtras =
            serde_json::from_str(&serde_json::to_string(&details).unwrap()).unwrap();
        assert!(!restored.merge(&DetailExtras::default()));
        assert_eq!(restored, details);
    }

    #[test]
    fn unsafe_targets_are_omitted_and_old_headers_remain_readable() {
        let item: MetaItem = serde_json::from_value(json!({"id":"tt1","type":"movie",
            "trailerStreams":[{"url":"file:///secret"},{"url":"https://user:pass@example.org/video"},{"url":"javascript:alert(1)"}],
            "trailers":[{"source":"abc&malicious=1"}], "imdbRating":99
        })).unwrap();
        let details = DetailExtras::from_item(&item);
        assert!(details.trailers.is_empty());
        assert!(details.rating.is_empty());
        let header: MetaHeader =
            serde_json::from_str(r#"{"description":"Legacy synopsis"}"#).unwrap();
        assert_eq!(header.details, DetailExtras::default());
    }
}

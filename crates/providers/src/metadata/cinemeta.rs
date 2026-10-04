//! Repair Cinemeta's IMDb-numbered image URLs using its TMDB-numbered inventory.
use super::{AddonMetadataTransport, EnrichmentResult, EpisodeEnrichment, RESPONSE_LIMIT};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use url::Url;

fn series_id(raw_url: &str) -> Option<String> {
    let url = Url::parse(raw_url).ok()?;
    if url.scheme() != "https" || url.host_str()? != "v3-cinemeta.strem.io" {
        return None;
    }
    let id = url
        .path()
        .strip_prefix("/meta/series/")?
        .strip_suffix(".json")?;
    (id.starts_with("tt") && id.len() > 2 && id[2..].bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.to_owned())
}

fn episode_key(video: &Value) -> Option<(bool, String, String)> {
    let title = video.get("name").or_else(|| video.get("title"))?.as_str()?;
    let title = crate::sequence::episode_title(title);
    let date = video.get("released")?.as_str()?.get(..10)?;
    if title.is_empty()
        || date.len() != 10
        || !date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    Some((video.get("season")?.as_u64()? == 0, title, date.into()))
}

fn image_for_series(image: &str, id: &str) -> bool {
    let Ok(url) = Url::parse(image) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("episodes.metahub.space")
        && url.path().starts_with(&format!("/{id}/"))
}

pub(super) fn repair(raw_url: &str, meta: &mut Value, transport: &dyn AddonMetadataTransport) {
    repair_with(raw_url, meta, |url| {
        transport
            .fetch(url, Duration::from_secs(5), RESPONSE_LIMIT)
            .ok()
    });
}

pub(super) fn repair_with(
    raw_url: &str,
    meta: &mut Value,
    fetch: impl FnOnce(&str) -> Option<Vec<u8>>,
) {
    let Some(id) = series_id(raw_url) else {
        return;
    };
    let Some(videos) = meta.get("videos").and_then(Value::as_array) else {
        return;
    };
    if !videos.iter().any(|video| {
        video
            .get("thumbnail")
            .and_then(Value::as_str)
            .is_some_and(|image| image_for_series(image, &id))
    }) {
        return;
    }
    let key = artwork_key(raw_url, videos);
    let cached = super::cache::read(&key)
        .and_then(|raw| serde_json::from_str::<(u64, EnrichmentResult)>(&raw).ok())
        .filter(|(_, result)| result.status == "confirmed");
    if let Some((at, result)) = &cached
        && super::now().saturating_sub(*at) < super::CONFIRMED_CACHE_TTL
    {
        restore_images(meta, result, &id);
        return;
    }
    // One optional bounded request, rather than probing every image. Failure
    // leaves the original response usable, including offline/cached artwork.
    let Some(bytes) = fetch(&format!(
        "https://cinemeta-live.strem.io/meta/series/{id}.json"
    )) else {
        if let Some((_, result)) = cached {
            restore_images(meta, &result, &id);
        }
        return;
    };
    if bytes.len() > RESPONSE_LIMIT {
        return;
    }
    let Ok(response) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let Some(live) = response
        .get("meta")
        .filter(|m| m.get("id").and_then(Value::as_str) == Some(&id))
    else {
        return;
    };
    let episode_metadata = apply_images(meta, live, &id);
    if !episode_metadata.is_empty() {
        let result = EnrichmentResult {
            status: "confirmed".into(),
            episode_metadata,
            inventory_revision: 0,
            details: Default::default(),
            connections: vec![],
        };
        if let Ok(raw) = serde_json::to_string(&(super::now(), &result)) {
            super::cache::write(&key, &result, &raw);
        }
    }
}

fn artwork_key(raw_url: &str, videos: &[Value]) -> String {
    // Header changes do not alter an episode match; inventory/title/date/URL
    // changes do. The original Cinemeta IDs scope cached artwork to this list.
    let input = serde_json::to_vec(&serde_json::json!([
        raw_url,
        videos
            .iter()
            .map(|v| {
                [
                    "id",
                    "name",
                    "title",
                    "released",
                    "season",
                    "episode",
                    "thumbnail",
                ]
                .map(|field| v.get(field).cloned().unwrap_or(Value::Null))
            })
            .collect::<Vec<_>>()
    ]))
    .unwrap_or_default();
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &input);
    format!(
        "metadata:v2:cinemeta-artwork:{}",
        digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn restore_images(meta: &mut Value, result: &EnrichmentResult, id: &str) {
    if let Some(videos) = meta.get_mut("videos").and_then(Value::as_array_mut) {
        for video in videos {
            let Some(image) = video
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| result.episode_metadata.get(id))
                .and_then(|e| e.thumbnail.as_ref())
            else {
                continue;
            };
            if image_for_series(image, id) {
                video["thumbnail"] = Value::String(image.clone());
            }
        }
    }
}

fn apply_images(meta: &mut Value, live: &Value, id: &str) -> BTreeMap<String, EpisodeEnrichment> {
    let mut matched = BTreeMap::new();
    let Some(videos) = meta.get_mut("videos").and_then(Value::as_array_mut) else {
        return matched;
    };
    let Some(alternates) = live.get("videos").and_then(Value::as_array) else {
        return matched;
    };
    let mut images = BTreeMap::new();
    for video in alternates {
        if let Some(key) = episode_key(video) {
            images.entry(key).or_insert_with(Vec::new).push(video);
        }
    }
    let mut counts = BTreeMap::new();
    for video in videos.iter() {
        if let Some(key) = episode_key(video) {
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    for video in videos {
        let Some(key) = episode_key(video) else {
            continue;
        };
        if counts.get(&key) != Some(&1) {
            continue;
        }
        let Some(matches) = images.get(&key).filter(|matches| matches.len() == 1) else {
            continue;
        };
        let Some(image) = matches[0].get("thumbnail").and_then(Value::as_str) else {
            continue;
        };
        if image_for_series(image, id)
            && video
                .get("thumbnail")
                .and_then(Value::as_str)
                .is_some_and(|image| image_for_series(image, id))
        {
            // Never copy live numbering/IDs: those would change stream lookup
            // and disconnect existing history from its original episode.
            if let Some(id) = video.get("id").and_then(Value::as_str) {
                matched.insert(
                    id.into(),
                    EpisodeEnrichment {
                        thumbnail: Some(image.into()),
                        ..Default::default()
                    },
                );
            }
            video["thumbnail"] = Value::String(image.into());
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn video(season: u32, episode: u32, name: &str) -> Value {
        json!({"id": format!("tt5607616:{season}:{episode}"), "season": season,
            "episode": episode, "name": name, "released": "2020-07-08T13:30:00Z",
            "thumbnail": format!("https://episodes.metahub.space/tt5607616/{season}/{episode}/w780.jpg")})
    }

    #[test]
    fn repairs_numbering_without_changing_episode_identity() {
        for (season, absolute) in [(2, 26), (3, 51), (4, 67)] {
            let original = video(season, 1, "Each One's Promise");
            let mut meta = json!({"videos": [original.clone()]});
            let mut alternate = video(1, absolute, "Each One’s Promise");
            alternate["released"] = json!("2020-07-08T00:00:00Z");
            let live = json!({"videos": [alternate.clone()]});
            apply_images(&mut meta, &live, "tt5607616");
            let mut expected = original;
            expected["thumbnail"] = alternate["thumbnail"].clone();
            assert_eq!(meta["videos"][0], expected);
        }
    }

    #[test]
    fn rejects_ambiguous_dates_specials_and_unrelated_artwork() {
        let original = video(2, 1, "Promise");
        for alternates in [
            vec![video(1, 26, "Promise"), video(1, 27, "Promise")],
            vec![video(0, 26, "Promise")],
            vec![{
                let mut v = video(1, 26, "Promise");
                v["released"] = json!("2020-07-09");
                v
            }],
            vec![{
                let mut v = video(1, 26, "Promise");
                v["thumbnail"] = json!("https://episodes.metahub.space/tt123/1/26/w780.jpg");
                v
            }],
        ] {
            let mut meta = json!({"videos": [original.clone()]});
            apply_images(&mut meta, &json!({"videos": alternates}), "tt5607616");
            assert_eq!(meta["videos"][0], original);
        }
        let mut meta = json!({"videos": [original.clone(), original.clone()]});
        apply_images(
            &mut meta,
            &json!({"videos": [video(1, 26, "Promise")]}),
            "tt5607616",
        );
        assert_eq!(meta["videos"][0], original);
    }

    struct FixtureTransport {
        response: Value,
        fail: bool,
    }
    impl AddonMetadataTransport for FixtureTransport {
        fn fetch(
            &self,
            url: &str,
            timeout: Duration,
            limit: usize,
        ) -> Result<Vec<u8>, crate::ProviderHostError> {
            assert_eq!(
                url,
                "https://cinemeta-live.strem.io/meta/series/tt5607616.json"
            );
            assert_eq!(timeout, Duration::from_secs(5));
            assert_eq!(limit, RESPONSE_LIMIT);
            if self.fail {
                Err(crate::ProviderHostError("offline".into()))
            } else {
                Ok(serde_json::to_vec(&self.response).unwrap())
            }
        }
    }

    #[test]
    fn bounded_transport_repairs_and_preserves_original_on_failure_or_wrong_show() {
        let original = json!({"videos": [video(2, 1, "Promise")]});
        let response = json!({"meta": {"id": "tt5607616", "videos": [video(1, 26, "Promise")]}});
        let url = "https://v3-cinemeta.strem.io/meta/series/tt5607616.json";
        let mut meta = original.clone();
        repair(
            url,
            &mut meta,
            &FixtureTransport {
                response: response.clone(),
                fail: false,
            },
        );
        assert_eq!(
            meta["videos"][0]["thumbnail"],
            response["meta"]["videos"][0]["thumbnail"]
        );
        for fixture in [
            FixtureTransport {
                response: response.clone(),
                fail: true,
            },
            FixtureTransport {
                response: json!({"meta": {"id": "tt123", "videos": [video(1, 26, "Promise")]}}),
                fail: false,
            },
        ] {
            let mut meta = original.clone();
            repair(url, &mut meta, &fixture);
            assert_eq!(meta, original);
        }
    }

    #[test]
    fn cached_artwork_keeps_episode_identity_and_invalidates_changed_inputs() {
        let original = video(2, 1, "Promise");
        let key = artwork_key("source", std::slice::from_ref(&original));
        let mut repaired = json!({"videos": [original.clone()]});
        let mappings = apply_images(
            &mut repaired,
            &json!({"videos": [video(1, 26, "Promise")]}),
            "tt5607616",
        );
        let result = EnrichmentResult {
            status: "confirmed".into(),
            episode_metadata: mappings,
            inventory_revision: 0,
            details: Default::default(),
            connections: vec![],
        };
        let restored: EnrichmentResult =
            serde_json::from_slice(&serde_json::to_vec(&result).unwrap()).unwrap();
        let mut reopened = json!({"videos": [original.clone()]});
        restore_images(&mut reopened, &restored, "tt5607616");
        assert_eq!(reopened, repaired);
        let mut changed = original.clone();
        changed["released"] = json!("2020-07-09");
        assert_ne!(key, artwork_key("source", &[changed]));
        assert_ne!(key, artwork_key("source", &[original.clone(), original]));
    }

    #[test]
    fn accepts_only_official_cinemeta_series_requests() {
        assert_eq!(
            series_id("https://v3-cinemeta.strem.io/meta/series/tt5607616.json"),
            Some("tt5607616".into())
        );
        for url in [
            "https://other.example/meta/series/tt5607616.json",
            "https://v3-cinemeta.strem.io/meta/movie/tt5607616.json",
            "https://v3-cinemeta.strem.io/meta/series/bad.json",
        ] {
            assert!(series_id(url).is_none());
        }
    }
}

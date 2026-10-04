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

// Bump when matching changes so old partial/broken artwork does not remain
// pinned by the persistent cache. This is local cache data, not a wire schema.
pub(super) const ARTWORK_MATCH_VERSION: u32 = 2;
type EpisodeKey = (bool, String, Option<i64>);
type ArtworkIndex<'a> = BTreeMap<(bool, String), Vec<(usize, &'a Value)>>;

fn release_day(value: &str) -> Option<i64> {
    let date = value.get(..10)?;
    if !date.bytes().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 {
            b == b'-'
        } else {
            b.is_ascii_digit()
        }
    }) {
        return None;
    }
    let year: i64 = date[..4].parse().ok()?;
    let month: usize = date[5..7].parse().ok()?;
    let day: i64 = date[8..10].parse().ok()?;
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=lengths[month - 1]).contains(&day) {
        return None;
    }
    let previous = year - 1;
    Some(
        365 * previous + previous / 4 - previous / 100
            + previous / 400
            + lengths[..month - 1].iter().sum::<i64>()
            + day,
    )
}

fn episode_key(video: &Value) -> Option<EpisodeKey> {
    let title = video
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| video.get("title").and_then(Value::as_str))?;
    let title = crate::sequence::episode_title(title);
    let day = video
        .get("released")
        .and_then(Value::as_str)
        .and_then(release_day);
    if title.is_empty() {
        return None;
    }
    Some((video.get("season")?.as_u64()? == 0, title, day))
}

fn unique_match<'a>(images: &ArtworkIndex<'a>, key: &EpisodeKey) -> Option<(usize, &'a Value)> {
    let (special, title, day) = key;
    let candidates = images.get(&(*special, title.clone()))?;
    // A globally unique episode title within the same IMDb series is a strong
    // identity anchor even when providers disagree about (or omit) air dates.
    // The caller also checks the reverse direction before applying artwork.
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    let day = (*day)?;
    // Repeated titles need a unique nearby date. Japanese broadcast dates and
    // UTC/global release dates may differ by one calendar day.
    let mut matches = candidates.iter().copied().filter(|(_, video)| {
        episode_key(video)
            .and_then(|(_, _, day)| day)
            .is_some_and(|other| (other - day).abs() <= 1)
    });
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
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
    let live = fetch(&format!(
        "https://cinemeta-live.strem.io/meta/series/{id}.json"
    ))
    .filter(|bytes| bytes.len() <= RESPONSE_LIMIT)
    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    .and_then(|response| response.get("meta").cloned())
    .filter(|meta| meta.get("id").and_then(Value::as_str) == Some(&id));
    let Some(live) = live else {
        // Malformed, oversized or unrelated success responses are failures
        // too. They must not undo already confirmed artwork after TTL expiry.
        if let Some((_, result)) = &cached {
            restore_images(meta, result, &id);
        }
        return;
    };
    let mut episode_metadata = apply_images(meta, &live, &id);
    if let Some((_, result)) = &cached {
        restore_missing_images(meta, result, &episode_metadata, &id);
    }
    if !episode_metadata.is_empty() {
        if let Some((_, result)) = cached {
            for (key, old) in result.episode_metadata {
                episode_metadata.entry(key).or_insert(old);
            }
        }
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

fn restore_missing_images(
    meta: &mut Value,
    cached: &EnrichmentResult,
    fresh: &BTreeMap<String, EpisodeEnrichment>,
    id: &str,
) {
    let mut fallback = cached.clone();
    fallback
        .episode_metadata
        .retain(|key, _| !fresh.contains_key(key));
    restore_images(meta, &fallback, id);
}

fn artwork_key(raw_url: &str, videos: &[Value]) -> String {
    // Header changes do not alter an episode match; inventory/title/date/URL
    // changes do. The original Cinemeta IDs scope cached artwork to this list.
    let input = serde_json::to_vec(&serde_json::json!([
        ARTWORK_MATCH_VERSION,
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
    for (index, video) in alternates.iter().enumerate() {
        if let Some((special, title, _)) = episode_key(video) {
            images
                .entry((special, title))
                .or_insert_with(Vec::new)
                .push((index, video));
        }
    }
    let mut counts = BTreeMap::new();
    for video in videos.iter() {
        if let Some(key) = episode_key(video)
            && let Some((target_index, _)) = unique_match(&images, &key)
        {
            // The reverse direction must be unique too: two native entries
            // must never borrow artwork from the same near-dated episode.
            *counts.entry(target_index).or_insert(0) += 1;
        }
    }
    for video in videos {
        let Some(key) = episode_key(video) else {
            continue;
        };
        let Some((target_index, target)) = unique_match(&images, &key) else {
            continue;
        };
        if counts.get(&target_index) != Some(&1) {
            continue;
        }
        let Some(image) = target.get("thumbnail").and_then(Value::as_str) else {
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
    fn repairs_entire_shifted_season_from_cinemeta_without_other_addons() {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/cinemeta-numbering.json")).unwrap();
        let original = fixture["native"].clone();
        let mut meta = original.clone();
        repair_with(
            "https://v3-cinemeta.strem.io/meta/series/tt21209876.json",
            &mut meta,
            |url| {
                assert_eq!(
                    url,
                    "https://cinemeta-live.strem.io/meta/series/tt21209876.json"
                );
                Some(serde_json::to_vec(&json!({"meta": fixture["live"]})).unwrap())
            },
        );
        assert_eq!(meta["videos"].as_array().unwrap().len(), 13);
        for (index, video) in meta["videos"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                video["thumbnail"],
                fixture["live"]["videos"][index]["thumbnail"]
            );
            let mut expected = original["videos"][index].clone();
            expected["thumbnail"] = video["thumbnail"].clone();
            assert_eq!(*video, expected); // IDs, seasons, numbering and dates stay native
        }
    }

    #[test]
    fn tolerates_both_date_directions_across_month_year_and_leap_boundaries() {
        for (first, second) in [
            ("2025-01-31", "2025-02-01"),
            ("2025-12-31", "2026-01-01"),
            ("2024-02-28", "2024-02-29"),
            ("2024-02-29", "2024-03-01"),
            ("2025-02-28", "2025-03-01"),
        ] {
            assert_eq!(
                release_day(second).unwrap() - release_day(first).unwrap(),
                1
            );
            for (native_date, live_date) in [(first, second), (second, first)] {
                let mut native = video(2, 1, "Promise");
                native["released"] = json!(native_date);
                let mut alternate = video(1, 26, "Promise");
                alternate["released"] = json!(live_date);
                let mut meta = json!({"videos": [native]});
                apply_images(
                    &mut meta,
                    &json!({"videos": [alternate.clone()]}),
                    "tt5607616",
                );
                assert_eq!(meta["videos"][0]["thumbnail"], alternate["thumbnail"]);
            }
        }
        for invalid in [
            "2025-02-29",
            "2024-02-30",
            "2025-13-01",
            "0000-01-01",
            "2025-01-00",
            "bad date",
        ] {
            assert!(release_day(invalid).is_none());
        }
    }

    #[test]
    fn unique_titles_cover_missing_dates_while_repeated_titles_require_dates() {
        let mut original = video(2, 1, "Promise");
        original["released"] = Value::Null;
        let mut alternate = video(1, 26, "Promise");
        alternate["released"] = Value::Null;
        let mut meta = json!({"videos": [original.clone()]});
        apply_images(
            &mut meta,
            &json!({"videos": [alternate.clone()]}),
            "tt5607616",
        );
        assert_eq!(meta["videos"][0]["thumbnail"], alternate["thumbnail"]);
        let mut meta = json!({"videos": [original.clone()]});
        apply_images(
            &mut meta,
            &json!({"videos": [alternate, video(1, 27, "Promise")]}),
            "tt5607616",
        );
        assert_eq!(meta["videos"][0], original);
        let mut native = video(2, 1, "Promise");
        native["released"] = json!("2020-07-08");
        let mut nearby = video(1, 26, "Promise");
        nearby["released"] = json!("2020-07-09");
        let mut other = video(1, 27, "Promise");
        other["released"] = json!("2020-08-09");
        let mut meta = json!({"videos": [native]});
        apply_images(
            &mut meta,
            &json!({"videos": [nearby.clone(), other]}),
            "tt5607616",
        );
        assert_eq!(meta["videos"][0]["thumbnail"], nearby["thumbnail"]);
    }

    #[test]
    fn near_dates_still_require_unique_matches_in_both_directions() {
        let original = video(2, 1, "Promise");
        let mut following = video(2, 2, "Promise");
        following["released"] = json!("2020-07-09");
        let mut alternate = video(1, 26, "Promise");
        alternate["released"] = json!("2020-07-09");
        let mut meta = json!({"videos": [original.clone(), following]});
        let before = meta.clone();
        apply_images(&mut meta, &json!({"videos": [alternate]}), "tt5607616");
        assert_eq!(meta, before);
        let mut following = video(1, 27, "Promise");
        following["released"] = json!("2020-07-09");
        let mut meta = json!({"videos": [original.clone()]});
        apply_images(
            &mut meta,
            &json!({"videos": [video(1, 26, "Promise"), following]}),
            "tt5607616",
        );
        assert_eq!(meta["videos"][0], original);
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
            vec![
                {
                    let mut v = video(1, 26, "Promise");
                    v["released"] = json!("2020-07-10");
                    v
                },
                {
                    let mut v = video(1, 27, "Promise");
                    v["released"] = json!("2020-07-11");
                    v
                },
            ],
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
    fn cached_images_fill_partial_refreshes_without_overwriting_new_matches() {
        let mut meta = json!({"videos": [video(2, 1, "Promise"), video(2, 2, "Arrival")]});
        let old = apply_images(
            &mut meta,
            &json!({"videos": [video(1, 26, "Promise"), video(1, 27, "Arrival")]}),
            "tt5607616",
        );
        let old_arrival = meta["videos"][1]["thumbnail"].clone();
        let cached = EnrichmentResult {
            status: "confirmed".into(),
            episode_metadata: old,
            inventory_revision: 0,
            details: Default::default(),
            connections: vec![],
        };
        let mut refreshed = json!({"videos": [video(2, 1, "Promise"), video(2, 2, "Arrival")]});
        let fresh = apply_images(
            &mut refreshed,
            &json!({"videos": [video(3, 5, "Promise")]}),
            "tt5607616",
        );
        let new_promise = refreshed["videos"][0]["thumbnail"].clone();
        restore_missing_images(&mut refreshed, &cached, &fresh, "tt5607616");
        assert_eq!(refreshed["videos"][0]["thumbnail"], new_promise);
        assert_eq!(refreshed["videos"][1]["thumbnail"], old_arrival);
        assert_eq!(refreshed["videos"][0]["id"], "tt5607616:2:1");
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

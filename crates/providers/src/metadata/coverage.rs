//! Coverage is separate from display numbering and outbound stream identity.
//! Previously saved episode IDs anchor progress; another provider may split
//! those same episodes into different seasons or only return an aired prefix.
use std::collections::{BTreeMap, BTreeSet};

use addons::{MetaItem, Video};
use serde_json::{Value, json};

use super::{MetadataConnection, addon_metadata_id, all_ids, valid_alias};
use crate::{ExternalId, MediaItem, SequenceEpisode, SourceSequence};

pub(super) fn main_series(media: &MediaItem) -> bool {
    media.provider_id.is_empty()
        && media.media_type == "series"
        && !crate::private_provider_id(&media.source_id)
        && !crate::sequence::series_identity(&media.title).labeled
        && !crate::matching::ids_conflict(media, media)
        && all_ids(media)
            .iter()
            .any(|id| matches!(id, ExternalId::Imdb(_) | ExternalId::TmdbTv(_)))
}

fn regular(video: &Video) -> bool {
    video.season.is_some_and(|s| (1..=100).contains(&s))
        && video
            .episode_number()
            .is_some_and(|n| (1..=10_000).contains(&n))
        && valid_alias(&video.id)
}

fn title(video: &Video) -> String {
    crate::sequence::episode_title(&video.label())
}

fn day(video: &Video) -> Option<i64> {
    video
        .released
        .as_deref()
        .and_then(super::cinemeta::release_day)
}

fn owner(video: &Video) -> Option<&str> {
    video.extra.get("novaSourceUrl").and_then(Value::as_str)
}

fn global_id(id: &str) -> bool {
    let parts = id.split(':').collect::<Vec<_>>();
    let positive = |s: &str| s.parse::<u32>().is_ok_and(|n| n > 0);
    match parts.as_slice() {
        [head, season, episode] if ExternalId::parse(head).is_some() => {
            positive(season) && positive(episode)
        }
        ["kitsu" | "mal" | "anilist" | "tmdb", parent, episode] => {
            positive(parent) && positive(episode)
        }
        [
            "kitsu" | "mal" | "anilist" | "tmdb",
            parent,
            season,
            episode,
        ] => positive(parent) && positive(season) && positive(episode),
        _ => false,
    }
}

fn conflicts(video: &Video, media: &MediaItem) -> bool {
    connections(video).any(|connection| connection.ids.iter().any(|id| {
        matches!(media.external_ids.resolve_id(id.namespace()), crate::IdResolution::Unique(current) if current != *id)
            || matches!(media.external_ids.resolve_id(id.namespace()), crate::IdResolution::Conflict(_))
    }))
}

fn connections(video: &Video) -> impl Iterator<Item = MetadataConnection> + '_ {
    video
        .extra
        .get("novaConnections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
}

fn aliases(
    video: &Video,
    media: &MediaItem,
    revision: Option<u64>,
) -> BTreeMap<String, MetadataConnection> {
    if revision.is_none()
        || video
            .extra
            .get("novaMetadataRevision")
            .and_then(Value::as_u64)
            != revision
    {
        return BTreeMap::new();
    }
    let ids = video
        .extra
        .get("novaStreamIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let provenance = video
        .extra
        .get("novaConnections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    ids.zip(provenance).filter_map(|(id, connection)| {
        let id = id.as_str()?;
        let connection: MetadataConnection = serde_json::from_value(connection.clone()).ok()?;
        let contradicts = connection.ids.iter().any(|id| {
            matches!(media.external_ids.resolve_id(id.namespace()), crate::IdResolution::Unique(current) if current != *id)
                || matches!(media.external_ids.resolve_id(id.namespace()), crate::IdResolution::Conflict(_))
        });
        (valid_alias(id) && connection.basis != "conflicting-identifiers" && !contradicts
            && connection.ids.iter().any(|id| matches!(id, ExternalId::Imdb(_) | ExternalId::TmdbTv(_)) && all_ids(media).contains(id)))
            .then(|| (id.to_owned(), connection))
    }).collect()
}

fn sources(media: &MediaItem, videos: &[Video]) -> Vec<SourceSequence> {
    let mut seasons: BTreeMap<u32, Vec<SequenceEpisode>> = BTreeMap::new();
    for video in videos.iter().filter(|v| regular(v)).take(10_000) {
        seasons
            .entry(video.season.unwrap())
            .or_default()
            .push(SequenceEpisode {
                id: video.id.clone(),
                number: video.episode_number().unwrap(),
                title: video.label(),
                released: video.released.clone(),
            });
    }
    seasons
        .into_iter()
        .map(|(season, episodes)| SourceSequence {
            media_id: media.source_id.clone(),
            title: media.title.clone(),
            aliases: media.aliases.clone(),
            year: media.year.clone(),
            season: Some(season),
            part: Some(1),
            episodes,
            ..Default::default()
        })
        .collect()
}

fn unique_index<T: Ord>(items: impl Iterator<Item = (T, usize)>) -> BTreeMap<T, usize> {
    let mut values = BTreeMap::new();
    for (key, index) in items {
        values
            .entry(key)
            .and_modify(|v| *v = None)
            .or_insert(Some(index));
    }
    values
        .into_iter()
        .filter_map(|(key, index)| index.map(|index| (key, index)))
        .collect()
}

/// One-to-one coverage evidence. Counts alone must not silently associate
/// different episodes with a saved ID: shared IDs, aliases, unique titles or
/// unique release dates establish the numbering region before extending it.
fn alignment(
    media: &MediaItem,
    known: &[Video],
    fresh: &[Video],
    revision: Option<u64>,
) -> BTreeMap<usize, usize> {
    let old_ids = unique_index(known.iter().enumerate().map(|(i, v)| (v.id.clone(), i)));
    let new_ids = unique_index(fresh.iter().enumerate().map(|(i, v)| (v.id.clone(), i)));
    let old_titles = unique_index(known.iter().enumerate().filter_map(|(i, v)| {
        let title = title(v);
        if title.is_empty() {
            None
        } else {
            Some((title, i))
        }
    }));
    let new_titles = unique_index(fresh.iter().enumerate().filter_map(|(i, v)| {
        let title = title(v);
        if title.is_empty() {
            None
        } else {
            Some((title, i))
        }
    }));
    let old_days = unique_index(
        known
            .iter()
            .enumerate()
            .filter_map(|(i, v)| day(v).map(|day| (day, i))),
    );
    let new_days = unique_index(
        fresh
            .iter()
            .enumerate()
            .filter_map(|(i, v)| day(v).map(|day| (day, i))),
    );
    let old_aliases = unique_index(known.iter().enumerate().flat_map(|(i, v)| {
        aliases(v, media, revision)
            .into_keys()
            .map(move |id| (id, i))
    }));
    let mut proposed: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (j, v) in fresh.iter().enumerate().filter(|(_, v)| regular(v)) {
        if !new_ids.contains_key(&v.id) {
            continue;
        }
        if let Some(&i) = old_ids.get(&v.id)
            && (global_id(&v.id)
                || (!media.source_id.is_empty()
                    && v.id.starts_with(&format!("{}:", media.source_id)))
                || owner(v).is_some_and(|url| Some(url) == owner(&known[i])))
        {
            proposed.entry(i).or_default().insert(j);
            continue;
        }
        let mut has_alias = false;
        if let Some(&i) = old_aliases.get(&v.id) {
            has_alias = true;
            proposed.entry(i).or_default().insert(j);
        }
        for id in aliases(v, media, revision).keys() {
            if let Some(&i) = old_ids.get(id) {
                has_alias = true;
                proposed.entry(i).or_default().insert(j);
            }
        }
        if has_alias {
            continue;
        }
        if let Some(&i) = old_titles
            .get(&title(v))
            .filter(|_| new_titles.contains_key(&title(v)))
        {
            if day(&known[i])
                .zip(day(v))
                .is_none_or(|(a, b)| a.abs_diff(b) <= 1)
            {
                proposed.entry(i).or_default().insert(j);
            }
        } else if let Some(&i) =
            day(v).and_then(|d| old_days.get(&d).filter(|_| new_days.contains_key(&d)))
        {
            let old_title = title(&known[i]);
            if old_title.is_empty() || title(v).is_empty() || old_title == title(v) {
                proposed.entry(i).or_default().insert(j);
            }
        }
    }
    // Only unambiguous evidence establishes a constant numbering offset.
    let mut regions: BTreeMap<(u32, u32), BTreeSet<i64>> = BTreeMap::new();
    for (&i, targets) in &proposed {
        if targets.len() == 1 && regular(&known[i]) {
            let v = &fresh[*targets.first().unwrap()];
            regions
                .entry((known[i].season.unwrap(), v.season.unwrap()))
                .or_default()
                .insert(
                    i64::from(known[i].episode_number().unwrap())
                        - i64::from(v.episode_number().unwrap()),
                );
        }
    }
    let numbered = crate::sequence::map_episodes(media, fresh, &sources(media, known));
    for (old_id, new_id) in numbered {
        let (Some(&i), Some(&j)) = (old_ids.get(&old_id), new_ids.get(&new_id)) else {
            continue;
        };
        let key = (known[i].season.unwrap(), fresh[j].season.unwrap());
        if regions.get(&key).is_some_and(|offsets| offsets.len() == 1) {
            let a = title(&known[i]);
            let b = title(&fresh[j]);
            if a.is_empty() || b.is_empty() || a == b {
                proposed.entry(i).or_default().insert(j);
            }
        }
    }
    // A shortened refresh may no longer meet the mapper's full-count proof.
    // Its already-confirmed offset still identifies the surviving prefix.
    for (&(old_season, new_season), offsets) in &regions {
        if offsets.len() != 1 {
            continue;
        }
        let offset = *offsets.first().unwrap();
        let positions = unique_index(known.iter().enumerate().filter_map(|(i, v)| {
            (v.season == Some(old_season))
                .then(|| v.episode_number().map(|n| (i64::from(n), i)))
                .flatten()
        }));
        for (j, v) in fresh.iter().enumerate().filter(|(_, v)| {
            regular(v) && v.season == Some(new_season) && new_ids.contains_key(&v.id)
        }) {
            let Some(&i) = v
                .episode_number()
                .and_then(|n| positions.get(&(i64::from(n) + offset)))
            else {
                continue;
            };
            let a = title(&known[i]);
            let b = title(v);
            if a.is_empty() || b.is_empty() || a == b {
                proposed.entry(i).or_default().insert(j);
            }
        }
    }
    let mut reverse: BTreeMap<usize, usize> = BTreeMap::new();
    for targets in proposed.values() {
        for &j in targets {
            *reverse.entry(j).or_default() += 1;
        }
    }
    proposed
        .into_iter()
        .filter_map(|(i, targets)| {
            (targets.len() == 1)
                .then(|| *targets.first().unwrap())
                .filter(|j| reverse.get(j) == Some(&1))
                .map(|j| (i, j))
        })
        .collect()
}

fn merge_video(
    media: &MediaItem,
    previous: &Video,
    fresh: &Video,
    revision: Option<u64>,
    source: Option<&str>,
) -> Video {
    let mut video = fresh.clone();
    video.id = previous.id.clone();
    video.season = previous.season;
    video.episode = previous.episode;
    video.number = previous.number;
    if title(fresh).is_empty() && !title(previous).is_empty() {
        video.name = previous.name.clone();
        video.title = previous.title.clone();
    }
    for (current, prior) in [
        (&mut video.thumbnail, &previous.thumbnail),
        (&mut video.overview, &previous.overview),
        (&mut video.released, &previous.released),
    ] {
        if current.as_deref().is_none_or(|v| v.trim().is_empty()) {
            *current = prior.clone();
        }
    }
    if previous.id != fresh.id {
        let mut routes = aliases(previous, media, revision);
        routes.extend(aliases(fresh, media, revision));
        if let (Some(revision), Some(source)) = (revision, owner(fresh).or(source)) {
            routes.insert(
                fresh.id.clone(),
                MetadataConnection {
                    addon_id: addon_metadata_id(source),
                    media_id: media.source_id.clone(),
                    ids: all_ids(media),
                    basis: "confirmed-episode-coverage".into(),
                },
            );
            video
                .extra
                .insert("novaMetadataRevision".into(), json!(revision));
        }
        video.extra.insert(
            "novaSourceUrl".into(),
            json!(owner(previous).unwrap_or_default()),
        );
        video.extra.insert(
            "novaStreamIds".into(),
            json!(routes.keys().collect::<Vec<_>>()),
        );
        video.extra.insert(
            "novaConnections".into(),
            json!(routes.values().collect::<Vec<_>>()),
        );
    }
    video
}

pub(super) fn merge(
    media: &MediaItem,
    known: &[Video],
    fresh: &[Video],
    revision: Option<u64>,
    source: Option<&str>,
) -> Vec<Video> {
    let known = known
        .iter()
        .filter(|v| regular(v) && !conflicts(v, media))
        .cloned()
        .collect::<Vec<_>>();
    let matched = alignment(media, &known, fresh, revision);
    let covered = matched.values().copied().collect::<BTreeSet<_>>();
    let all_known_covered = known
        .iter()
        .enumerate()
        .all(|(i, _)| matched.contains_key(&i));
    let mut merged = known
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if let Some(&j) = matched.get(&i) {
                return merge_video(media, v, &fresh[j], revision, source);
            }
            let mut v = v.clone();
            if v.extra.get("novaMetadataRevision").and_then(Value::as_u64) != revision {
                for key in ["novaStreamIds", "novaConnections", "novaMetadataRevision"] {
                    v.extra.remove(key);
                }
            }
            v
        })
        .collect::<Vec<_>>();
    for (j, v) in fresh.iter().enumerate() {
        if !regular(v) {
            merged.push(v.clone());
            continue;
        }
        if covered.contains(&j) {
            continue;
        }
        // Never replace known content or silently add a second interpretation
        // of it. New coverage is safe only after the common prefix is proven.
        if all_known_covered
            && !merged.iter().any(|old| {
                old.id == v.id
                    || (old.season == v.season && old.episode_number() == v.episode_number())
            })
        {
            merged.push(v.clone());
        }
    }
    merged.sort_by_key(|v| (v.season, v.episode_number()));
    merged
}

pub(super) fn missing(media: &MediaItem, known: &[Video], donor: &[Video]) -> Vec<Vec<Video>> {
    let matched = alignment(media, known, donor, None);
    if !known
        .iter()
        .enumerate()
        .filter(|(_, v)| regular(v))
        .all(|(i, _)| matched.contains_key(&i))
    {
        return vec![];
    }
    let covered = matched.values().copied().collect::<BTreeSet<_>>();
    let mut seasons: BTreeMap<u32, Vec<(usize, Video)>> = BTreeMap::new();
    for (j, v) in donor.iter().enumerate().filter(|(_, v)| regular(v)) {
        seasons
            .entry(v.season.unwrap())
            .or_default()
            .push((j, v.clone()));
    }
    seasons
        .into_values()
        .filter_map(|mut videos| {
            videos.sort_by_key(|(_, v)| v.episode_number());
            let mut ids = BTreeSet::new();
            if !videos.iter().enumerate().all(|(i, (_, v))| {
                v.episode_number() == Some(i as u32 + 1) && ids.insert(v.id.clone())
            }) {
                return None;
            }
            let missing = videos
                .into_iter()
                .filter_map(|(j, v)| {
                    (!covered.contains(&j)
                        && !known.iter().any(|old| {
                            old.id == v.id
                                || (old.season == v.season
                                    && old.episode_number() == v.episode_number())
                        }))
                    .then_some(v)
                })
                .collect::<Vec<_>>();
            (!missing.is_empty()).then_some(missing)
        })
        .collect()
}

/// Reconcile an ordinary main-series detail response with the saved episode
/// cache. This is pure: the app owns persistence, progress and synchronization.
pub fn reconcile_episode_metadata(item: &MetaItem, known: &[Video]) -> Vec<Video> {
    let mut fields = item.preview.extra.clone();
    fields.extend(item.extra.clone());
    let mut ids = crate::normalize_external_ids(&fields, &item.preview.id, &item.preview.type_);
    for id in
        crate::normalize_external_ids(&item.preview.extra, &item.preview.id, &item.preview.type_)
            .typed
    {
        if !ids.typed.contains(&id) {
            ids.typed.push(id);
        }
    }
    let media = MediaItem {
        source_id: item.preview.id.clone(),
        media_type: item.preview.type_.clone(),
        title: item.preview.title(),
        year: item.preview.year_str(),
        external_ids: ids,
        ..Default::default()
    };
    if !main_series(&media) {
        return item.videos.clone();
    }
    merge(
        &media,
        known,
        &item.videos,
        fields.get("novaMetadataRevision").and_then(Value::as_u64),
        fields.get("novaSourceUrl").and_then(Value::as_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> MediaItem {
        MediaItem {
            source_id: "tt100".into(),
            media_type: "series".into(),
            title: "Example".into(),
            year: Some("2020".into()),
            external_ids: crate::ExternalIds {
                imdb: Some("tt100".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn episode(id: &str, season: u32, number: u32, name: &str) -> Video {
        Video {
            id: id.into(),
            season: Some(season),
            episode: Some(number),
            name: name.into(),
            released: Some(format!("2026-10-{number:02}")),
            ..Default::default()
        }
    }

    fn item(videos: Vec<Video>) -> MetaItem {
        let mut item: MetaItem = serde_json::from_value(
            json!({"id":"tt100", "type":"series", "name":"Example", "year":"2020",
            "novaMetadataRevision":7, "novaSourceUrl":"https://primary.example"}),
        )
        .unwrap();
        item.videos = videos;
        item
    }

    #[test]
    fn merged_and_split_seasons_have_one_episode_each_and_preserve_numbering() {
        let native = (1..=24)
            .map(|n| episode(&format!("tt100:1:{n}"), 1, n, &format!("Story {n}")))
            .collect::<Vec<_>>();
        let split = (1..=24)
            .map(|n| {
                episode(
                    &format!(
                        "kitsu:{}:{}",
                        if n <= 12 { 10 } else { 20 },
                        (n - 1) % 12 + 1
                    ),
                    (n - 1) / 12 + 1,
                    (n - 1) % 12 + 1,
                    &format!("Story {n}"),
                )
            })
            .collect::<Vec<_>>();
        let split = split
            .into_iter()
            .enumerate()
            .map(|(i, mut v)| {
                v.released = native[i].released.clone();
                v
            })
            .collect::<Vec<_>>();
        assert!(missing(&media(), &native, &split).is_empty());
        let merged = reconcile_episode_metadata(&item(split), &native);
        assert_eq!(merged.len(), 24);
        for (old, new) in native.iter().zip(&merged) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.season, new.season);
            assert_eq!(old.episode, new.episode);
        }
    }

    #[test]
    fn primary_catching_up_keeps_saved_ids_and_provides_current_stream_routes() {
        let mut known = (1..=13)
            .map(|n| {
                episode(
                    &format!("kitsu:20:{n}"),
                    2,
                    n,
                    if n == 1 { "Return" } else { "Episode" },
                )
            })
            .collect::<Vec<_>>();
        for v in &mut known {
            v.extra
                .insert("novaSourceUrl".into(), json!("https://donor.example"));
        }
        known[0].thumbnail = Some("https://images.example/old.jpg".into());
        let fresh = (1..=13)
            .map(|n| {
                episode(
                    &format!("tt100:2:{n}"),
                    2,
                    n,
                    if n == 1 { "Return" } else { "Episode" },
                )
            })
            .collect();
        let result = reconcile_episode_metadata(&item(fresh), &known);
        assert_eq!(result.len(), 13);
        for (n, v) in result.iter().enumerate() {
            assert_eq!(v.id, format!("kitsu:20:{}", n + 1));
            assert!(
                v.extra["novaStreamIds"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("tt100:2:{}", n + 1)))
            );
            assert_eq!(v.extra["novaSourceUrl"], "https://donor.example");
            assert_eq!(v.extra["novaMetadataRevision"], 7);
        }
        assert_eq!(result[0].thumbnail, known[0].thumbnail);
    }

    #[test]
    fn shortened_refresh_and_restart_retain_confirmed_episodes_and_new_fields() {
        let known = (1..=13)
            .map(|n| episode(&format!("kitsu:20:{n}"), 2, n, &format!("Story {n}")))
            .collect::<Vec<_>>();
        let restored: Vec<Video> =
            serde_json::from_slice(&serde_json::to_vec(&known).unwrap()).unwrap();
        let mut first = restored[0].clone();
        first.overview = Some("Fresh synopsis".into());
        let merged = reconcile_episode_metadata(&item(vec![first]), &restored);
        assert_eq!(merged.len(), 13);
        assert_eq!(merged[0].overview.as_deref(), Some("Fresh synopsis"));
        assert_eq!(reconcile_episode_metadata(&item(vec![]), &merged).len(), 13);
    }

    #[test]
    fn incomplete_primary_season_can_gain_the_remaining_confirmed_episodes() {
        let known = vec![
            episode("tt100:1:1", 1, 1, "Opening"),
            episode("tt100:2:1", 2, 1, "Return"),
        ];
        let donor = vec![
            episode("kitsu:10:1", 1, 1, "Opening"),
            episode("kitsu:20:1", 2, 1, "Return"),
            episode("kitsu:20:2", 2, 2, "Next story"),
        ];
        let missing = missing(&media(), &known, &donor);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].len(), 1);
        assert_eq!(missing[0][0].id, "kitsu:20:2");
        let merged = reconcile_episode_metadata(&item(donor), &known);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].id, "tt100:2:1");
        assert_eq!(merged[2].id, "kitsu:20:2");
    }

    #[test]
    fn ambiguity_holes_conflicts_and_other_entry_types_do_not_gain_speculative_coverage() {
        let mut known = vec![episode("tt100:1:1", 1, 1, "Episode")];
        known[0].released = None;
        let mut fresh = vec![
            episode("kitsu:10:1", 1, 1, "Episode"),
            episode("kitsu:10:2", 1, 2, "Episode"),
        ];
        for v in &mut fresh {
            v.released = None;
        }
        assert!(missing(&media(), &known, &fresh).is_empty());
        assert_eq!(
            reconcile_episode_metadata(&item(fresh.clone()), &known).len(),
            1
        );
        let mut duplicate = fresh.clone();
        duplicate[1].id = duplicate[0].id.clone();
        assert!(missing(&media(), &known, &duplicate).is_empty());
        let mut labeled = item(fresh.clone());
        labeled.preview.name = "Example Season 2".into();
        assert_eq!(
            reconcile_episode_metadata(&labeled, &known)[0].id,
            "kitsu:10:1"
        );
        let mut movie = item(fresh.clone());
        movie.preview.type_ = "movie".into();
        assert_eq!(reconcile_episode_metadata(&movie, &known).len(), 2);
        let mut bundled = item(fresh);
        bundled.preview.id = "anikoto:show".into();
        assert_eq!(reconcile_episode_metadata(&bundled, &known).len(), 2);
        let known = vec![episode("tt100:1:1", 1, 1, "Opening")];
        let gap = vec![known[0].clone(), episode("kitsu:20:2", 2, 2, "Next")];
        assert!(missing(&media(), &known, &gap).is_empty());
    }

    #[test]
    fn stale_routes_are_removed_from_retained_episodes_without_discarding_history_ids() {
        let mut old = episode("kitsu:20:1", 2, 1, "Opening");
        old.extra.extend([
            ("novaMetadataRevision".into(), json!(6)),
            ("novaStreamIds".into(), json!(["tt100:2:1"])),
            (
                "novaConnections".into(),
                json!([MetadataConnection {
                    addon_id: "disabled".into(),
                    media_id: "tt100".into(),
                    ids: all_ids(&media()),
                    basis: "confirmed".into()
                }]),
            ),
        ]);
        let result = reconcile_episode_metadata(&item(vec![]), &[old]);
        assert_eq!(result[0].id, "kitsu:20:1");
        assert!(!result[0].extra.contains_key("novaStreamIds"));
        assert!(!result[0].extra.contains_key("novaConnections"));
    }

    #[test]
    fn only_current_nonconflicting_aliases_can_override_changed_episode_fields() {
        let old = episode("kitsu:20:1", 2, 1, "Original title");
        let mut fresh = episode("tt100:2:1", 2, 1, "Corrected title");
        fresh.released = Some("2026-11-01".into());
        let connection = MetadataConnection {
            addon_id: "source".into(),
            media_id: "tt100".into(),
            ids: all_ids(&media()),
            basis: "confirmed".into(),
        };
        fresh.extra.extend([
            ("novaMetadataRevision".into(), json!(7)),
            ("novaStreamIds".into(), json!([old.id])),
            ("novaConnections".into(), json!([connection])),
        ]);
        let result =
            reconcile_episode_metadata(&item(vec![fresh.clone()]), std::slice::from_ref(&old));
        assert_eq!(result[0].id, old.id);
        assert_eq!(result[0].label(), "Corrected title");
        for invalid in ["revision", "identity", "malformed-provenance"] {
            let mut fresh = fresh.clone();
            match invalid {
                "revision" => {
                    fresh.extra.insert("novaMetadataRevision".into(), json!(6));
                }
                "identity" => {
                    fresh.extra.insert(
                        "novaConnections".into(),
                        json!([MetadataConnection {
                            ids: vec![ExternalId::Imdb("tt999".into())],
                            ..connection.clone()
                        }]),
                    );
                }
                _ => {
                    fresh
                        .extra
                        .insert("novaStreamIds".into(), json!([old.id, "unrelated-alias"]));
                    fresh
                        .extra
                        .insert("novaConnections".into(), json!([null, connection]));
                }
            }
            let result = reconcile_episode_metadata(&item(vec![fresh]), std::slice::from_ref(&old));
            assert_eq!(result[0].label(), "Original title", "{invalid}");
            assert_eq!(result[0].id, old.id);
        }
    }
}

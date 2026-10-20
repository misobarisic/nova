//! Worker-only identity/episode adapter for the account's installed addons.
//! Snapshots and an injected bounded transport keep UI locks and account
//! credentials out of metadata requests.
use super::{MetaItem, Video, confirmed_episode_stream_ids};
use addons::{Addon, Manifest};
use nova_stremio::LibraryItem;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const MAX_ADDONS: usize = 8;
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const VIDEO_LIMIT: usize = 20_000;
const REQUEST_BUDGET: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub(super) struct LocalTitle {
    pub media_type: String,
    pub id: String,
    pub videos: Vec<Video>,
}

pub(super) fn local_inventory_revision(local: Option<&LocalTitle>) -> u64 {
    let Some(local) = local.filter(|local| !local.videos.is_empty()) else {
        return 0;
    };
    let identities: Vec<_> = local
        .videos
        .iter()
        .map(|video| (&video.id, confirmed_episode_stream_ids(video)))
        .collect();
    nova_config::fnv1a(
        &serde_json::to_vec(&identities)
            .expect("episode IDs and aliases serialize as JSON strings"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingReason {
    InvalidIdentity,
    MetadataUnavailable,
    AmbiguousIdentity,
    EpisodeMappingUnavailable,
    UnsupportedIdentity,
    RemoteTitleCollision,
    WatchedHistoryUnavailable,
}

pub(super) struct ResolvedTitle {
    pub media_type: String,
    pub id: String,
    /// Full remote metadata; source-specific native caches must not be replaced
    /// with this list, whose episode IDs belong to the remote catalog.
    pub metadata: Option<MetaItem>,
    /// An existing Nova inventory belongs to its installed metadata source.
    /// Matching parent IDs do not license replacing AIOmetadata/source episode
    /// IDs with the account's Cinemeta inventory.
    pub(super) preserve_native_metadata: bool,
    pub local_inventory_revision: u64,
    /// Complete account-addon list, in its original order. Watched bitfields
    /// require the full list even when only one native season is mapped.
    pub videos: Vec<nova_stremio::Video>,
    /// Remote episode ID -> confirmed native episode ID. Missing entries are
    /// unknown coverage and must never become local unwatches.
    pub episode_ids: BTreeMap<String, String>,
}

impl ResolvedTitle {
    /// A remote parent is useful for decoding its watched bitfield, but may
    /// describe more seasons than the native source owns. Only exact typed
    /// identities may populate Nova's native episode/header caches.
    pub(super) fn native_metadata(&self) -> Option<&MetaItem> {
        self.metadata.as_ref().filter(|item| {
            !self.preserve_native_metadata
                && item.preview.id == self.id
                && item.preview.type_ == self.media_type
        })
    }
}

/// Refresh an existing native inventory through the device's installed sources.
/// The caller snapshots enabled/available sources before entering this worker;
/// account addon descriptors are deliberately not used to replace native IDs.
pub(super) fn hydrate_local(
    local: &LocalTitle,
    sources: &[nova_providers::MetadataAddon],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<MetaItem, PendingReason> {
    if !safe_segment(&local.id) || !safe_segment(&local.media_type) {
        return Err(PendingReason::InvalidIdentity);
    }
    let started = Instant::now();
    for addon in sources
        .iter()
        .filter(|source| {
            source
                .manifest
                .accepts("meta", &local.media_type, &local.id)
        })
        .filter_map(|source| Addon::new(&source.url).ok())
        .filter(|addon| {
            ["http://", "https://", "nova-provider://"]
                .iter()
                .any(|scheme| addon.base_url().starts_with(scheme))
        })
        .take(MAX_ADDONS)
    {
        let Some(remaining) = REQUEST_BUDGET.checked_sub(started.elapsed()) else {
            break;
        };
        let url = addon.meta_url(&local.media_type, &local.id);
        let Ok(body) = fetch(&url, remaining, BODY_LIMIT) else {
            continue;
        };
        if body.len() > BODY_LIMIT {
            continue;
        }
        let Some(remaining) = REQUEST_BUDGET.checked_sub(started.elapsed()) else {
            break;
        };
        // Ordinary HTTP details need the same normalization and confirmed
        // episode aliases as the Details page. Native providers may already
        // carry enrichment, which this entry point recognizes and preserves.
        let body = nova_providers::enrich_addon_response_with_budget(&url, body, remaining);
        if started.elapsed() >= REQUEST_BUDGET || body.len() > BODY_LIMIT {
            continue;
        }
        let Ok(Some(item)) = Addon::parse_meta(&body) else {
            continue;
        };
        if item.preview.id != local.id
            || item.preview.type_ != local.media_type
            || item.videos.is_empty()
            || item.videos.len() > VIDEO_LIMIT
        {
            continue;
        }
        let mut ids = std::collections::BTreeSet::new();
        if item.videos.iter().any(|video| {
            video.id.is_empty()
                || video.id.len() > 4096
                || video.id.chars().any(char::is_control)
                || !ids.insert(&video.id)
        }) {
            continue;
        }
        return Ok(item);
    }
    Err(PendingReason::MetadataUnavailable)
}

/// Retain an established native target only while its current identity evidence
/// still points to this exact remote title; refreshes cannot silently retarget.
pub(super) fn resolve_bound(
    remote: &LibraryItem,
    local: &LocalTitle,
    descriptors: &[Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<ResolvedTitle, PendingReason> {
    let (media_type, id) = outbound_identity(local, descriptors)?;
    if id != remote.id() || media_type != remote.media_type() {
        return Err(PendingReason::AmbiguousIdentity);
    }
    resolve(remote, std::slice::from_ref(local), descriptors, fetch)
}

/// The transport must enforce both supplied limits, including redirects/body
/// reads, and must not attach the Stremio account's authorization headers.
pub(super) fn resolve(
    remote: &LibraryItem,
    locals: &[LocalTitle],
    descriptors: &[Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<ResolvedTitle, PendingReason> {
    let id = remote.id();
    let media_type = remote.media_type();
    if !safe_segment(id) || !safe_segment(media_type) {
        return Err(PendingReason::InvalidIdentity);
    }
    let local = local_match(media_type, id, locals)?;
    if media_type == "movie" {
        let native_id = local.map_or(id, |title| title.id.as_str());
        return Ok(ResolvedTitle {
            media_type: media_type.into(),
            id: native_id.into(),
            metadata: None,
            preserve_native_metadata: false,
            local_inventory_revision: local_inventory_revision(local),
            videos: Vec::new(),
            episode_ids: [(id.to_owned(), native_id.to_owned())].into(),
        });
    }
    let metadata = fetch_metadata(media_type, id, descriptors, fetch)?;
    let videos = metadata
        .videos
        .iter()
        .map(|video| nova_stremio::Video {
            id: video.id.clone(),
            season: video.season,
            episode: video.episode.or(video.number),
            released_ms: video
                .released
                .as_deref()
                .and_then(nova_stremio::released_timestamp_ms),
        })
        .collect();
    let preserve_native_metadata = local.is_some_and(|local| !local.videos.is_empty());
    let episode_ids = match local {
        Some(local)
            if local.id != id || local.media_type != media_type || !local.videos.is_empty() =>
        {
            mapped_episodes(&metadata.videos, &local.videos)?
        }
        // A cold import adopts the account's episode IDs. Existing inventories
        // must prove each episode even when their parent IDs match exactly.
        _ => metadata
            .videos
            .iter()
            .map(|v| (v.id.clone(), v.id.clone()))
            .collect(),
    };
    Ok(ResolvedTitle {
        media_type: local
            .map_or(media_type, |title| title.media_type.as_str())
            .into(),
        id: local.map_or(id, |title| title.id.as_str()).into(),
        metadata: Some(metadata),
        preserve_native_metadata,
        local_inventory_revision: local_inventory_revision(local),
        videos,
        episode_ids,
    })
}

/// Choose only an account-supported native identity or a unique confirmed
/// canonical parent. Private provider IDs are never exported as catalog IDs.
/// The returned item is a membership shell; the actor applies merged progress.
pub(super) fn outbound(
    local: &LocalTitle,
    name: &str,
    modified_ms: u64,
    descriptors: &[Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<(LibraryItem, ResolvedTitle), PendingReason> {
    let (media_type, id) = outbound_identity(local, descriptors)?;
    let item = LibraryItem::new(&id, name, &media_type, modified_ms);
    let mapping = resolve(&item, std::slice::from_ref(local), descriptors, fetch)?;
    Ok((item, mapping))
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 1024
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'-' | b'.'))
}

fn account_addons(descriptors: &[Value]) -> Vec<(Addon, Manifest)> {
    descriptors
        .iter()
        .filter_map(|descriptor| {
            let url = descriptor.get("transportUrl")?.as_str()?;
            // Avoid interpreting provider pseudo-URLs as network endpoints.
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return None;
            }
            let addon = Addon::new(url).ok()?;
            let manifest = serde_json::from_value(descriptor.get("manifest")?.clone()).ok()?;
            Some((addon, manifest))
        })
        .collect()
}

fn supported(descriptors: &[Value], media_type: &str, id: &str) -> bool {
    account_addons(descriptors)
        .iter()
        .any(|(_, manifest)| manifest.accepts("meta", media_type, id))
}

/// Search only the signed-in account's declared catalogs, without credentials.
/// Results are proposals for explicit review, not identity evidence.
pub(super) fn search(
    media_type: &str,
    query: &str,
    descriptors: &[Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Vec<LibraryItem> {
    if query.len() > 512 {
        return Vec::new();
    }
    let mut results = Vec::new();
    if safe_segment(query) && supported(descriptors, media_type, query) {
        results.push(LibraryItem::new(query, query, media_type, 0));
    }
    let started = Instant::now();
    let mut requests = 0;
    for (addon, manifest) in account_addons(descriptors) {
        for catalog in manifest
            .catalogs
            .iter()
            .filter(|c| c.type_ == media_type && c.supports_extra("search"))
        {
            let Some(remaining) = REQUEST_BUDGET.checked_sub(started.elapsed()) else {
                return results;
            };
            if requests >= MAX_ADDONS {
                return results;
            }
            requests += 1;
            let Ok(bytes) = fetch(
                &addon.catalog_url(media_type, &catalog.id, &[("search", query)]),
                remaining,
                BODY_LIMIT,
            ) else {
                continue;
            };
            if bytes.len() > BODY_LIMIT {
                continue;
            }
            if let Ok(items) = Addon::parse_catalog(&bytes) {
                for item in items.into_iter().take(100).filter(|i| {
                    i.type_ == media_type
                        && safe_segment(&i.id)
                        && supported(descriptors, media_type, &i.id)
                }) {
                    results.push(LibraryItem::new(&item.id, &item.name, media_type, 0));
                    if results.len() >= 30 {
                        return results;
                    }
                }
            }
        }
    }
    results
}

fn fetch_metadata(
    media_type: &str,
    id: &str,
    descriptors: &[Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<MetaItem, PendingReason> {
    let started = Instant::now();
    for (addon, _) in account_addons(descriptors)
        .into_iter()
        .filter(|(_, manifest)| manifest.accepts("meta", media_type, id))
        .take(MAX_ADDONS)
    {
        let Some(remaining) = REQUEST_BUDGET.checked_sub(started.elapsed()) else {
            break;
        };
        let Ok(body) = fetch(&addon.meta_url(media_type, id), remaining, BODY_LIMIT) else {
            continue;
        };
        if body.len() > BODY_LIMIT {
            continue;
        }
        let Ok(Some(item)) = Addon::parse_meta(&body) else {
            continue;
        };
        // An addon returning a different title has not established identity.
        if item.preview.id != id
            || item.preview.type_ != media_type
            || item.videos.is_empty()
            || item.videos.len() > VIDEO_LIMIT
        {
            continue;
        }
        let mut ids = std::collections::BTreeSet::new();
        if item
            .videos
            .iter()
            .any(|video| !safe_segment(&video.id) || !ids.insert(&video.id))
        {
            continue;
        }
        return Ok(item);
    }
    Err(PendingReason::MetadataUnavailable)
}

/// Keep the account's canonical coordination row virtual when existing native
/// sources already prove its identity. This does not merge/delete saved titles,
/// and a partial-season binding still retains its own IDs and episode coverage.
pub(super) fn represented_by_native(
    remote: &LibraryItem,
    locals: &[LocalTitle],
    descriptors: &[Value],
) -> bool {
    remote.media_type() != "movie"
        && !locals
            .iter()
            .any(|local| local.id == remote.id() && local.media_type == remote.media_type())
        && locals.iter().any(|local| {
            outbound_identity(local, descriptors).is_ok_and(|(media_type, id)| {
                id == remote.id() && media_type == remote.media_type()
            }) || (local.media_type != "movie"
                && remote.media_type() == "series"
                && supported(descriptors, "series", remote.id())
                && confirmed_parent_matches(local, remote.id()))
        })
}

/// Export preference is not an identity boundary: an account may accept both
/// Kitsu and IMDb while the saved Kitsu source also proves the IMDb parent.
/// Compare within one namespace so split Kitsu seasons do not conflict with
/// their shared IMDb series; competing parents in that namespace still reject.
fn confirmed_parent_matches(local: &LocalTitle, target: &str) -> bool {
    let namespace = |id: &str| {
        if matches!(
            nova_providers::ExternalId::parse(id),
            Some(nova_providers::ExternalId::Imdb(_))
        ) {
            Some("imdb")
        } else {
            id.strip_prefix("kitsu:")
                .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                .map(|_| "kitsu")
        }
    };
    let Some(target_namespace) = namespace(target) else {
        return false;
    };
    if namespace(&local.id) == Some(target_namespace) && local.id != target {
        return false;
    }
    let mut parents: Vec<_> = local
        .videos
        .iter()
        .flat_map(confirmed_episode_stream_ids)
        .filter_map(|alias| episode_parent(&alias).map(str::to_owned))
        .filter(|parent| namespace(parent) == Some(target_namespace))
        .collect();
    parents.sort();
    parents.dedup();
    matches!(parents.as_slice(), [parent] if parent == target)
}

fn local_match<'a>(
    media_type: &str,
    id: &str,
    locals: &'a [LocalTitle],
) -> Result<Option<&'a LocalTitle>, PendingReason> {
    let exact: Vec<_> = locals
        .iter()
        .filter(|local| local.media_type == media_type && local.id == id)
        .collect();
    match exact.as_slice() {
        [local] => return Ok(Some(local)),
        [] => {}
        _ => return Err(PendingReason::AmbiguousIdentity),
    }
    if media_type == "movie" {
        return Ok(None);
    }
    let matches: Vec<_> = locals
        .iter()
        .filter(|local| {
            local.media_type != "movie"
                && local.videos.iter().any(|video| {
                    confirmed_episode_stream_ids(video)
                        .iter()
                        .any(|alias| episode_parent(alias) == Some(id))
                })
        })
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [local] => Ok(Some(local)),
        _ => Err(PendingReason::AmbiguousIdentity),
    }
}

fn mapped_episodes(
    remote: &[Video],
    native: &[Video],
) -> Result<BTreeMap<String, String>, PendingReason> {
    let mut mapped = BTreeMap::new();
    for video in remote {
        let direct: Vec<_> = native
            .iter()
            .filter(|native| native.id == video.id)
            .collect();
        let claims: Vec<_> = if direct.is_empty() {
            native
                .iter()
                .filter(|native| confirmed_episode_stream_ids(native).contains(&video.id))
                .collect()
        } else {
            direct
        };
        match claims.as_slice() {
            [] => {}
            [native] => {
                mapped.insert(video.id.clone(), native.id.clone());
            }
            _ => return Err(PendingReason::AmbiguousIdentity),
        }
    }
    // Different remote IDs must not collapse into one native episode.
    let distinct: std::collections::BTreeSet<_> = mapped.values().collect();
    if distinct.len() != mapped.len() {
        return Err(PendingReason::AmbiguousIdentity);
    }
    if mapped.is_empty() {
        return Err(PendingReason::EpisodeMappingUnavailable);
    }
    Ok(mapped)
}

/// Recognize the canonical shapes we can prove without guessing a source's
/// episode numbering convention. Arbitrary colon-separated IDs stay opaque.
fn episode_parent(id: &str) -> Option<&str> {
    let (parent, episode) = id.rsplit_once(':')?;
    episode.parse::<u32>().ok()?;
    if let Some((title, season)) = parent.rsplit_once(':')
        && season.parse::<u32>().is_ok()
        && matches!(
            nova_providers::ExternalId::parse(title),
            Some(nova_providers::ExternalId::Imdb(_))
        )
    {
        return Some(title);
    }
    let kitsu = parent.strip_prefix("kitsu:")?;
    (!kitsu.is_empty() && kitsu.bytes().all(|c| c.is_ascii_digit())).then_some(parent)
}

pub(super) fn outbound_identity(
    local: &LocalTitle,
    descriptors: &[Value],
) -> Result<(String, String), PendingReason> {
    if safe_segment(&local.id)
        && safe_segment(&local.media_type)
        && !nova_providers::private_provider_id(&local.id)
        && supported(descriptors, &local.media_type, &local.id)
    {
        return Ok((local.media_type.clone(), local.id.clone()));
    }
    let mut parents: Vec<_> = local
        .videos
        .iter()
        .flat_map(confirmed_episode_stream_ids)
        .filter_map(|alias| episode_parent(&alias).map(str::to_owned))
        .filter(|id| supported(descriptors, "series", id))
        .collect();
    parents.sort();
    parents.dedup();
    match parents.as_slice() {
        [parent] => Ok(("series".into(), parent.clone())),
        [] => Err(PendingReason::UnsupportedIdentity),
        _ => Err(PendingReason::AmbiguousIdentity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptors() -> Vec<Value> {
        vec![json!({
            "transportUrl":"https://metadata.example/manifest.json",
            "manifest": {"id":"example","name":"Metadata","version":"1.0.0",
                "resources":[{"name":"meta","types":["series","movie"],"idPrefixes":["tt"]}]}
        })]
    }

    fn episode(id: &str, aliases: &[&str]) -> Video {
        let mut video = Video {
            id: id.into(),
            ..Default::default()
        };
        video.extra.insert("novaStreamIds".into(), json!(aliases));
        video.extra.insert(
            "novaMetadataRevision".into(),
            json!(nova_providers::metadata_revision()),
        );
        video
    }

    #[test]
    fn canonical_import_reuses_proven_sources_but_keeps_existing_canonical_and_movies() {
        let remote = LibraryItem::new("tt100", "Same title", "series", 1);
        let native = LocalTitle {
            media_type: "anime".into(),
            id: "native-season".into(),
            videos: vec![episode("native-episode", &["tt100:2:1"])],
        };
        assert!(represented_by_native(
            &remote,
            std::slice::from_ref(&native),
            &descriptors()
        ));
        let canonical = LocalTitle {
            media_type: "series".into(),
            id: "tt100".into(),
            videos: vec![],
        };
        assert!(!represented_by_native(
            &remote,
            &[native.clone(), canonical],
            &descriptors()
        ));
        let conflicting = LocalTitle {
            videos: vec![episode("native", &["tt100:1:1", "tt200:1:1"])],
            ..native.clone()
        };
        assert!(!represented_by_native(
            &remote,
            &[conflicting],
            &descriptors()
        ));
        let unverified = LocalTitle {
            videos: vec![episode("native", &[])],
            ..native
        };
        assert!(!represented_by_native(
            &remote,
            &[unverified],
            &descriptors()
        ));
        assert!(!represented_by_native(
            &LibraryItem::new("tt100", "Same title", "movie", 1),
            &[],
            &descriptors()
        ));
    }

    #[test]
    fn canonical_import_checks_proven_aliases_even_when_native_id_is_supported() {
        let descriptors = vec![json!({
            "transportUrl":"https://metadata.example/manifest.json",
            "manifest":{"id":"example","name":"Metadata","version":"1.0.0",
                "resources":[{"name":"meta","types":["series"],"idPrefixes":["tt","kitsu:"]}]}
        })];
        let native = LocalTitle {
            media_type: "series".into(),
            id: "kitsu:10".into(),
            videos: vec![
                episode("kitsu:10:1", &["kitsu:10:1", "tt100:1:1"]),
                episode("kitsu:20:1", &["kitsu:20:1", "tt100:2:1"]),
            ],
        };
        let remote = LibraryItem::new("tt100", "Same title", "series", 1);
        assert_eq!(
            outbound_identity(&native, &descriptors).unwrap(),
            ("series".into(), "kitsu:10".into()),
            "the source keeps its existing export preference and playback IDs"
        );
        assert!(represented_by_native(
            &remote,
            std::slice::from_ref(&native),
            &descriptors
        ));
        for videos in [
            vec![episode("kitsu:10:1", &[])],
            vec![episode("kitsu:10:1", &["tt100:1:1", "tt200:1:1"])],
            vec![{
                let mut stale = episode("kitsu:10:1", &["tt100:1:1"]);
                stale.extra.insert(
                    "novaMetadataRevision".into(),
                    json!(nova_providers::metadata_revision().wrapping_add(1)),
                );
                stale
            }],
        ] {
            assert!(!represented_by_native(
                &remote,
                &[LocalTitle {
                    videos,
                    ..native.clone()
                }],
                &descriptors
            ));
        }
        assert!(!represented_by_native(
            &remote,
            &[LocalTitle {
                id: "tt200".into(),
                ..native
            }],
            &descriptors
        ));
    }

    #[test]
    fn descriptor_resource_type_and_prefix_are_respected() {
        let descriptors = descriptors();
        assert!(supported(&descriptors, "series", "tt100"));
        assert!(!supported(&descriptors, "anime", "tt100"));
        assert!(!supported(&descriptors, "series", "kitsu:100"));
        assert!(!supported(&[], "series", "tt100"));
    }

    #[test]
    fn confirmed_partial_season_keeps_full_remote_inventory() {
        let local = LocalTitle {
            media_type: "anime".into(),
            id: "native-season".into(),
            videos: vec![episode("native-episode", &["tt100:2:1"])],
        };
        let remote = LibraryItem::new("tt100", "Example", "series", 1);
        let body = serde_json::to_vec(
            &json!({"meta":{"id":"tt100","type":"series","name":"Example","videos":[
                {"id":"tt100:1:1","season":1,"episode":1},
                {"id":"tt100:2:1","season":2,"episode":1}
            ]}}),
        )
        .unwrap();
        let result = resolve(&remote, &[local], &descriptors(), &|_, _, _| {
            Ok(body.clone())
        })
        .unwrap();
        assert_eq!(result.id, "native-season");
        assert_eq!(result.videos.len(), 2);
        assert_eq!(
            result.episode_ids,
            [("tt100:2:1".into(), "native-episode".into())].into()
        );
    }

    #[test]
    fn competing_native_alias_claims_are_pending() {
        let remote = vec![episode("tt100:1:1", &[])];
        let native = vec![episode("a", &["tt100:1:1"]), episode("b", &["tt100:1:1"])];
        assert_eq!(
            mapped_episodes(&remote, &native),
            Err(PendingReason::AmbiguousIdentity)
        );
        let local = |id: &str| LocalTitle {
            media_type: "anime".into(),
            id: id.into(),
            videos: vec![episode("a", &["tt100:1:1"])],
        };
        assert!(matches!(
            local_match("series", "tt100", &[local("a"), local("b")]),
            Err(PendingReason::AmbiguousIdentity)
        ));
    }

    #[test]
    fn missing_metadata_cannot_project_progress_or_guess_from_numbers() {
        let item = LibraryItem::new("tt100", "Same name", "series", 1);
        assert!(matches!(
            resolve(&item, &[], &descriptors(), &|_, _, _| Err(())),
            Err(PendingReason::MetadataUnavailable)
        ));
        assert_eq!(
            mapped_episodes(
                &[episode("tt100:1:1", &[])],
                &[Video {
                    id: "native".into(),
                    season: Some(1),
                    episode: Some(1),
                    ..Default::default()
                }]
            ),
            Err(PendingReason::EpisodeMappingUnavailable)
        );
    }

    #[test]
    fn movies_resolve_without_addons_or_network() {
        let item = LibraryItem::new("tt100", "Movie", "movie", 1);
        let result = resolve(&item, &[], &[], &|_, _, _| {
            panic!("movie must not fetch metadata")
        })
        .unwrap();
        assert!(result.videos.is_empty());
        assert_eq!(
            result.episode_ids.get("tt100").map(String::as_str),
            Some("tt100")
        );
    }

    #[test]
    fn cold_cache_import_keeps_native_metadata_and_exact_episode_ids() {
        let item = LibraryItem::new("tt100", "Example", "series", 1);
        let body = serde_json::to_vec(&json!({"meta":{"id":"tt100","type":"series","name":"Example","videos":[{"id":"tt100:1:1","season":1,"episode":1}]}})).unwrap();
        let mut result = resolve(&item, &[], &descriptors(), &|_, _, _| Ok(body.clone())).unwrap();
        assert_eq!(result.native_metadata().unwrap().videos[0].id, "tt100:1:1");
        assert_eq!(
            result.episode_ids.get("tt100:1:1").map(String::as_str),
            Some("tt100:1:1")
        );
        // Keeping the parent for bitfields never licenses overwriting a
        // separately mapped native source's episode inventory.
        result.id = "native-season".into();
        result.media_type = "anime".into();
        assert!(result.native_metadata().is_none());
    }

    #[test]
    fn same_parent_maps_confirmed_episode_aliases_without_replacing_native_inventory() {
        let local = LocalTitle {
            media_type: "series".into(),
            id: "tt100".into(),
            videos: vec![episode("native:episode:one", &["tt100:1:1"])],
        };
        let item = LibraryItem::new("tt100", "Example", "series", 1);
        let body = serde_json::to_vec(&json!({"meta":{
            "id":"tt100","type":"series","name":"Example","videos":[
                {"id":"tt100:1:1","season":1,"episode":1},
                {"id":"tt100:1:2","season":1,"episode":2}
            ]
        }}))
        .unwrap();
        let result = resolve(&item, &[local], &descriptors(), &|_, _, _| Ok(body.clone())).unwrap();
        assert_eq!(
            result.videos.len(),
            2,
            "bitfields keep the full remote inventory"
        );
        assert_eq!(
            result.episode_ids,
            [("tt100:1:1".into(), "native:episode:one".into())].into()
        );
        assert!(result.native_metadata().is_none());
    }

    #[test]
    fn same_parent_does_not_guess_episode_identity_from_season_and_number() {
        let local = LocalTitle {
            media_type: "series".into(),
            id: "tt100".into(),
            videos: vec![Video {
                id: "unrelated:episode".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }],
        };
        let item = LibraryItem::new("tt100", "Example", "series", 1);
        let body = serde_json::to_vec(&json!({"meta":{
            "id":"tt100","type":"series","name":"Example",
            "videos":[{"id":"tt100:1:1","season":1,"episode":1}]
        }}))
        .unwrap();
        assert!(matches!(
            resolve(&item, &[local], &descriptors(), &|_, _, _| Ok(body.clone())),
            Err(PendingReason::EpisodeMappingUnavailable)
        ));
    }

    #[test]
    fn malformed_descriptors_and_path_segments_never_make_requests() {
        let item = LibraryItem::new("tt100", "Example", "series", 1);
        let malformed = vec![
            Value::Null,
            json!({"transportUrl":"https://"}),
            json!({"transportUrl":"file:///tmp/manifest.json","manifest":descriptors()[0]["manifest"]}),
            json!({"transportUrl":"https://example.com","manifest":{"resources":false}}),
        ];
        assert!(matches!(
            resolve(&item, &[], &malformed, &|_, _, _| panic!(
                "invalid descriptor fetched"
            )),
            Err(PendingReason::MetadataUnavailable)
        ));
        for (id, media_type) in [
            ("../secret", "series"),
            ("tt100", ".."),
            ("tt100?token=secret", "series"),
            ("tt100", "."),
        ] {
            let item = LibraryItem::new(id, "Example", media_type, 1);
            assert!(matches!(
                resolve(&item, &[], &descriptors(), &|_, _, _| panic!(
                    "invalid identity fetched"
                )),
                Err(PendingReason::InvalidIdentity)
            ));
        }
    }

    #[test]
    fn outbound_uses_only_unique_supported_confirmed_parents() {
        let mut local = LocalTitle {
            media_type: "anime".into(),
            id: "native-season".into(),
            videos: vec![episode("native", &["tt100:2:1"])],
        };
        assert_eq!(
            outbound_identity(&local, &descriptors()),
            Ok(("series".into(), "tt100".into()))
        );
        assert_eq!(
            outbound_identity(&local, &[]),
            Err(PendingReason::UnsupportedIdentity)
        );
        local.videos.push(episode("other", &["tt200:1:1"]));
        assert_eq!(
            outbound_identity(&local, &descriptors()),
            Err(PendingReason::AmbiguousIdentity)
        );
        local.videos[0].extra.insert(
            "novaMetadataRevision".into(),
            json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        local.videos.pop();
        assert_eq!(
            outbound_identity(&local, &descriptors()),
            Err(PendingReason::UnsupportedIdentity)
        );
    }

    #[test]
    fn metadata_must_match_requested_typed_identity() {
        let item = LibraryItem::new("tt100", "Example", "series", 1);
        let body = serde_json::to_vec(&json!({"meta":{"id":"tt200","type":"series","name":"Example","videos":[{"id":"tt200:1:1"}]}})).unwrap();
        assert!(matches!(
            resolve(&item, &[], &descriptors(), &|_, _, _| Ok(body.clone())),
            Err(PendingReason::MetadataUnavailable)
        ));
        assert!(!safe_segment("tt100/../../secret"));
        assert_eq!(episode_parent("unrecognized:source:1:1"), None);
    }

    fn native_source(url: &str) -> nova_providers::MetadataAddon {
        nova_providers::MetadataAddon {
            url: url.into(),
            manifest: serde_json::from_value(json!({
                "id":"native", "name":"Native", "version":"1",
                "resources":["meta"], "types":["series"], "idPrefixes":["kitsu:"]
            }))
            .unwrap(),
        }
    }

    fn cold_local() -> LocalTitle {
        LocalTitle {
            media_type: "series".into(),
            id: "kitsu:42".into(),
            videos: vec![],
        }
    }

    #[test]
    fn cold_native_inventory_hydrates_from_http_or_provider_sources() {
        let local = cold_local();
        let body = serde_json::to_vec(&json!({"meta":{
            "id":"kitsu:42","type":"series","videos":[{
                "id":"native:episode", "season":1,"episode":1,
                "novaStreamIds":["tt100:1:1"],
                "novaMetadataRevision":nova_providers::metadata_revision()
            }]
        }}))
        .unwrap();
        for base in [
            "https://native.example/manifest.json",
            "nova-provider://native/manifest.json",
        ] {
            let source = native_source(base);
            let expected = Addon::new(base).unwrap().meta_url("series", "kitsu:42");
            let item = hydrate_local(&local, &[source], &|url, timeout, limit| {
                assert_eq!(url, expected);
                assert!(!timeout.is_zero() && timeout <= REQUEST_BUDGET);
                assert_eq!(limit, BODY_LIMIT);
                Ok(body.clone())
            })
            .unwrap();
            assert_eq!(item.preview.id, local.id);
            assert_eq!(item.videos[0].id, "native:episode");
            let hydrated = LocalTitle {
                videos: item.videos,
                ..cold_local()
            };
            assert_eq!(
                outbound_identity(&hydrated, &descriptors()),
                Ok(("series".into(), "tt100".into()))
            );
        }
    }

    #[test]
    fn native_hydration_requires_exact_identity_and_source_capability() {
        let local = cold_local();
        let mut stream_only = native_source("https://streams.example");
        stream_only.manifest.resources = serde_json::from_value(json!(["stream"])).unwrap();
        let mut wrong_type = native_source("https://movies.example");
        wrong_type.manifest.types = vec!["movie".into()];
        let mut wrong_prefix = native_source("https://imdb.example");
        wrong_prefix.manifest.id_prefixes = vec!["tt".into()];
        let unsupported_scheme = native_source("file:///tmp/source");
        assert!(matches!(
            hydrate_local(
                &local,
                &[stream_only, wrong_type, wrong_prefix, unsupported_scheme],
                &|_, _, _| {
                    panic!("ineligible sources must not receive native metadata requests")
                }
            ),
            Err(PendingReason::MetadataUnavailable)
        ));
        let invalid = LocalTitle {
            id: "../outside".into(),
            ..cold_local()
        };
        assert!(matches!(
            hydrate_local(
                &invalid,
                &[native_source("https://native.example")],
                &|_, _, _| { panic!("invalid request identity must not be fetched") }
            ),
            Err(PendingReason::InvalidIdentity)
        ));
        for (id, media_type) in [("kitsu:99", "series"), ("kitsu:42", "movie")] {
            let body = serde_json::to_vec(
                &json!({"meta":{"id":id,"type":media_type,"videos":[{"id":"native:episode"}]}}),
            )
            .unwrap();
            assert!(matches!(
                hydrate_local(
                    &local,
                    &[native_source("https://native.example")],
                    &|_, _, _| Ok(body.clone())
                ),
                Err(PendingReason::MetadataUnavailable)
            ));
        }
    }

    #[test]
    fn native_hydration_normalizes_only_a_proven_legacy_parent_alias() {
        let body = serde_json::to_vec(&json!({"meta":{
            "id":"tt100", "imdb_id":"tt100", "kitsu_id":"42", "type":"series",
            "videos":[{"id":"kitsu:42:1", "season":1, "episode":1}]
        }}))
        .unwrap();
        let item = hydrate_local(
            &cold_local(),
            &[native_source("https://native.example")],
            &|_, _, _| Ok(body.clone()),
        )
        .unwrap();
        assert_eq!(item.preview.id, "kitsu:42");
        assert_eq!(item.videos[0].id, "kitsu:42:1");
    }

    #[test]
    fn native_hydration_rejects_oversized_or_ambiguous_inventory() {
        let local = cold_local();
        let source = native_source("https://native.example");
        let huge = vec![b' '; BODY_LIMIT + 1];
        assert!(matches!(
            hydrate_local(&local, std::slice::from_ref(&source), &|_, _, _| Ok(
                huge.clone()
            )),
            Err(PendingReason::MetadataUnavailable)
        ));
        for videos in [
            json!([]),
            json!([{"id":""}]),
            json!([{"id":"native:episode"},{"id":"native:episode"}]),
            json!([{"id":"native:\ncontrol"}]),
            serde_json::to_value(
                (0..=VIDEO_LIMIT)
                    .map(|n| json!({"id":format!("native:{n}")}))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        ] {
            let body = serde_json::to_vec(
                &json!({"meta":{"id":"kitsu:42","type":"series","videos":videos}}),
            )
            .unwrap();
            assert!(matches!(
                hydrate_local(&local, std::slice::from_ref(&source), &|_, _, _| Ok(
                    body.clone()
                )),
                Err(PendingReason::MetadataUnavailable)
            ));
        }
        let calls = std::cell::Cell::new(0);
        let sources = vec![source; MAX_ADDONS + 1];
        assert!(matches!(
            hydrate_local(&local, &sources, &|_, _, _| {
                calls.set(calls.get() + 1);
                Err(())
            }),
            Err(PendingReason::MetadataUnavailable)
        ));
        assert_eq!(calls.get(), MAX_ADDONS);
    }

    #[test]
    fn native_hydration_does_not_refresh_unproven_old_episode_aliases() {
        let body = serde_json::to_vec(&json!({"meta":{
            "id":"kitsu:42","type":"series","videos":[{
                "id":"native:episode","novaStreamIds":["tt100:1:1"],
                "novaMetadataRevision":nova_providers::metadata_revision().wrapping_add(1)
            }]
        }}))
        .unwrap();
        let item = hydrate_local(
            &cold_local(),
            &[native_source("https://native.example")],
            &|_, _, _| Ok(body.clone()),
        )
        .unwrap();
        let local = LocalTitle {
            videos: item.videos,
            ..cold_local()
        };
        assert_eq!(
            outbound_identity(&local, &descriptors()),
            Err(PendingReason::UnsupportedIdentity)
        );
    }

    #[test]
    fn bound_mapping_cannot_retarget_to_another_remote_parent() {
        let remote = LibraryItem::new("tt100", "Example", "series", 1);
        let local = LocalTitle {
            media_type: "series".into(),
            id: "native-season".into(),
            videos: vec![episode("native:episode", &["tt200:1:1"])],
        };
        assert!(matches!(
            resolve_bound(&remote, &local, &descriptors(), &|_, _, _| {
                panic!("a changed parent binding must fail before a metadata request")
            }),
            Err(PendingReason::AmbiguousIdentity)
        ));
        let local = LocalTitle {
            videos: vec![episode("native:episode", &["tt100:1:1"])],
            ..local
        };
        let body = serde_json::to_vec(
            &json!({"meta":{"id":"tt100","type":"series","videos":[{"id":"tt100:1:1"}]}}),
        )
        .unwrap();
        let resolved =
            resolve_bound(&remote, &local, &descriptors(), &|_, _, _| Ok(body.clone())).unwrap();
        assert_eq!(resolved.id, "native-season");
        assert_eq!(resolved.episode_ids["tt100:1:1"], "native:episode");
    }
}

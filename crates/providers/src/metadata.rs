//! Installed-addon metadata broker and bounded Cinemeta artwork repair.
mod cache;
mod cinemeta;
pub use cache::{MetadataCache, set_metadata_cache};
const CONFIRMED_CACHE_TTL: u64 = 30 * 24 * 3600;
const PARTIAL_CACHE_TTL: u64 = 5 * 60;
// Invalidate confirmed results when field selection/provenance changes, without
// changing the durable store format or any sync wire schema.
const ENRICHMENT_VERSION: u32 = 2;
use crate::{
    ExternalId, MediaItem, ProviderHost, ProviderHostError, SourceSequence, StremioProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataAddon {
    pub url: String,
    pub manifest: addons::Manifest,
}

#[derive(Clone, Default)]
struct Snapshot {
    revision: u64,
    fingerprint: String,
    addons: Vec<MetadataAddon>,
}
fn snapshot() -> &'static Mutex<Snapshot> {
    static SNAPSHOT: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    SNAPSHOT.get_or_init(Default::default)
}

fn inventory_fingerprint(addons: &[MetadataAddon]) -> String {
    // Manifests contain HashMaps. Their iteration order changes on restart;
    // canonical JSON keeps the revision stable for identical addon content.
    let mut value = serde_json::to_value(addons).unwrap_or(Value::Null);
    value.sort_all_objects();
    serde_json::to_string(&value).unwrap_or_default()
}

/// Called with already-cloned enabled/available addons, never while networking.
pub fn configure_metadata_addons(addons: Vec<MetadataAddon>) {
    let fingerprint = inventory_fingerprint(&addons);
    let mut current = snapshot().lock().unwrap();
    if current.fingerprint != fingerprint {
        // A content-derived revision also invalidates persisted episode aliases
        // after restart when the configured addon inventory has changed.
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, fingerprint.as_bytes());
        current.revision = u64::from_le_bytes(digest.as_ref()[..8].try_into().unwrap());
        current.fingerprint = fingerprint;
        current.addons = addons;
    }
}

pub(crate) fn has_metadata_addons(caller: &str) -> bool {
    snapshot().lock().unwrap().addons.iter().any(|a| {
        a.url != caller
            && (a.manifest.has_meta()
                || a.manifest.catalogs.iter().any(|c| {
                    a.manifest
                        .search_catalogs(&c.type_)
                        .any(|search| search.id == c.id)
                }))
    })
}

/// Provenance exposed to JS is opaque: configured addon URLs can contain keys.
pub fn addon_metadata_id(url: &str) -> String {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, url.as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn metadata_revision() -> u64 {
    snapshot().lock().unwrap().revision
}

/// Installed endpoints are owned by the host, not arbitrary JS URL arguments.
/// Transports must enforce the supplied remaining timeout and body limit.
pub trait AddonMetadataTransport: Send + Sync {
    fn fetch(
        &self,
        url: &str,
        timeout: Duration,
        response_limit: usize,
    ) -> Result<Vec<u8>, ProviderHostError>;
}
fn transport() -> &'static Mutex<Option<Arc<dyn AddonMetadataTransport>>> {
    static TRANSPORT: OnceLock<Mutex<Option<Arc<dyn AddonMetadataTransport>>>> = OnceLock::new();
    TRANSPORT.get_or_init(Default::default)
}
pub fn set_metadata_transport(value: Arc<dyn AddonMetadataTransport>) {
    *transport().lock().unwrap() = Some(value);
}

thread_local! { static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
pub(crate) fn nested() -> bool {
    DEPTH.with(|d| d.get() > 0)
}
struct NestedGuard;
impl NestedGuard {
    fn new() -> Self {
        DEPTH.with(|d| d.set(d.get() + 1));
        Self
    }
}
impl Drop for NestedGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get() - 1));
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderDetails {
    pub media: MediaItem,
    #[serde(default)]
    pub episodes: Vec<crate::Episode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichmentRequest {
    pub details: ProviderDetails,
    #[serde(default)]
    pub source_sequences: Vec<SourceSequence>,
    #[serde(default)]
    pub targets: Vec<String>,
    pub query: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataConnection {
    pub addon_id: String,
    pub media_id: String,
    pub ids: Vec<ExternalId>,
    pub basis: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeEnrichment {
    pub ids: Vec<String>,
    pub connections: Vec<MetadataConnection>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub title: Option<String>,
    pub released: Option<String>,
    pub thumbnail: Option<String>,
    pub overview: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichmentResult {
    #[serde(default)]
    pub inventory_revision: u64,
    pub status: String,
    pub details: ProviderDetails,
    pub connections: Vec<MetadataConnection>,
    pub episode_metadata: BTreeMap<String, EpisodeEnrichment>,
}

impl EnrichmentResult {
    fn native(details: ProviderDetails) -> Self {
        Self {
            inventory_revision: metadata_revision(),
            status: "missing".into(),
            details,
            connections: vec![],
            episode_metadata: BTreeMap::new(),
        }
    }
}

fn cache_key(revision: u64, caller: &str, request: &EnrichmentRequest) -> String {
    let input = serde_json::to_vec(&(
        revision,
        caller,
        request,
        cinemeta::ARTWORK_MATCH_VERSION,
        crate::sequence::MAPPING_VERSION,
        ENRICHMENT_VERSION,
    ))
    .unwrap_or_default();
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &input);
    format!(
        "metadata:v2:{}",
        digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn cached_result(state: &dyn ProviderHost, key: &str) -> Option<EnrichmentResult> {
    let raw = state.storage_get(key).or_else(|| cache::read(key))?;
    let (at, result): (u64, EnrichmentResult) = serde_json::from_str(&raw).ok()?;
    (now().saturating_sub(at) < cache_ttl(&result)).then_some(result)
}
fn cache_ttl(result: &EnrichmentResult) -> u64 {
    if result.status != "confirmed" {
        return 30;
    }
    // A confirmed identity can still have unpublished episode data. Keep the
    // durable mapping, but revisit sparse details on the next fetch instead
    // of freezing generic titles/missing images for thirty days.
    if result.details.episodes.iter().any(|episode| {
        result
            .episode_metadata
            .get(&episode.stable_id())
            .is_none_or(|mapping| {
                mapping
                    .title
                    .as_deref()
                    .is_none_or(|title| crate::sequence::episode_title(title).is_empty())
                    || mapping
                        .thumbnail
                        .as_deref()
                        .is_none_or(|url| url.trim().is_empty())
            })
    }) {
        PARTIAL_CACHE_TTL
    } else {
        CONFIRMED_CACHE_TTL
    }
}
fn cache_result(state: &dyn ProviderHost, key: &str, result: &EnrichmentResult) {
    if let Ok(raw) = serde_json::to_string(&(now(), result))
        && raw.len() <= 256 * 1024
    {
        let _ = state.storage_set(key, &raw);
        cache::write(key, result, &raw);
    }
}

struct Operation {
    start: Instant,
    budget: Duration,
    calls: usize,
    detail_calls: usize,
    visited: BTreeSet<String>,
    transport: Arc<dyn AddonMetadataTransport>,
}
impl Operation {
    fn fetch(&mut self, url: &str) -> Option<Vec<u8>> {
        let remaining = self.budget.checked_sub(self.start.elapsed())?;
        if self.calls >= 32 || !self.visited.insert(url.into()) {
            return None;
        }
        self.calls += 1;
        let bytes = self.transport.fetch(url, remaining, RESPONSE_LIMIT).ok()?;
        (bytes.len() <= RESPONSE_LIMIT).then_some(bytes)
    }
}

pub fn enrich_metadata(request: EnrichmentRequest, caller_url: &str) -> EnrichmentResult {
    enrich_metadata_with_budget(request, caller_url, Duration::from_secs(5))
}

pub(crate) fn enrich_metadata_with_budget(
    request: EnrichmentRequest,
    caller_url: &str,
    budget: Duration,
) -> EnrichmentResult {
    if nested() || budget.is_zero() {
        return EnrichmentResult::native(request.details);
    }
    let native_details = request.details.clone();
    let snapshot = snapshot().lock().unwrap().clone();
    let key = cache_key(snapshot.revision, caller_url, &request);
    let state = crate::host::session_state();
    if let Some(result) = cached_result(state.as_ref(), &key) {
        return result;
    }
    let fallback = state
        .storage_get(&key)
        .into_iter()
        .chain(cache::read(&key))
        .filter_map(|raw| serde_json::from_str::<(u64, EnrichmentResult)>(&raw).ok())
        .map(|(_, result)| result)
        .find(|result| result.status == "confirmed");
    let Some(transport) = transport().lock().unwrap().clone() else {
        return fallback.unwrap_or_else(|| EnrichmentResult::native(request.details));
    };
    let _guard = NestedGuard::new();
    let mut operation = Operation {
        start: Instant::now(),
        calls: 0,
        detail_calls: 0,
        budget: budget.min(Duration::from_secs(5)),
        visited: BTreeSet::new(),
        transport,
    };
    let mut result = enrich_with(request, caller_url, &snapshot.addons, &mut operation);
    if metadata_revision() != snapshot.revision {
        return EnrichmentResult::native(native_details);
    }
    if result.status == "missing"
        && let Some(cached) = fallback
    {
        result = cached;
    }
    result.inventory_revision = snapshot.revision;
    cache_result(state.as_ref(), &key, &result);
    result
}

fn enrich_with(
    request: EnrichmentRequest,
    caller_url: &str,
    addons: &[MetadataAddon],
    operation: &mut Operation,
) -> EnrichmentResult {
    let mut result = EnrichmentResult::native(request.details);
    let native = result.details.media.clone();
    let query = request
        .query
        .unwrap_or_else(|| crate::sequence::series_identity(&native.title).base);
    let mut sources = request.source_sequences;
    if sources.is_empty() {
        let mut seasons: BTreeMap<u32, Vec<crate::SequenceEpisode>> = BTreeMap::new();
        for e in &result.details.episodes {
            seasons
                .entry(e.season)
                .or_default()
                .push(crate::SequenceEpisode {
                    id: e.stable_id(),
                    number: e.number,
                    title: e.title.clone(),
                    released: e.released.clone(),
                });
        }
        let identity = crate::sequence::series_identity(&native.title);
        // Split entries often call their only native cour "season 1". Its
        // explicit title label describes the parent series' numbering instead.
        let titled_entry = seasons.len() == 1 && identity.labeled;
        sources = seasons
            .into_iter()
            .map(|(season, episodes)| SourceSequence {
                media_id: native.stable_id(),
                title: native.title.clone(),
                aliases: native.aliases.clone(),
                year: native.year.clone(),
                season: Some(if titled_entry && season == 1 {
                    identity.season
                } else {
                    season
                }),
                part: Some(if titled_entry { identity.part } else { 1 }),
                declared_count: None,
                episodes,
            })
            .collect();
    }
    let mut candidates: Vec<MetadataCandidate> = Vec::new();
    let mut mapped_videos = BTreeMap::new();
    let mut connected_media = BTreeMap::new();
    let mut searched = false;
    // Expand connections through explicit identifiers only. Each round may
    // discover IDs another enabled metadata addon accepts; never generate IDs.
    let mut completed = BTreeSet::new();
    loop {
        for installed in addons
            .iter()
            .filter(|a| a.url != caller_url && a.manifest.has_meta())
        {
            let Ok(addon) = addons::Addon::new(&installed.url) else {
                continue;
            };
            for id in external_request_ids(&result.details.media) {
                if installed.manifest.accepts("meta", &native.media_type, &id)
                    && !candidates
                        .iter()
                        .any(|c| c.addon.url == installed.url && c.preview.source_id == id)
                {
                    let mut item = result.details.media.clone();
                    item.source_id = id;
                    candidates.push(MetadataCandidate {
                        addon: installed.clone(),
                        preview: item,
                        rank: 3,
                        response: None,
                    });
                }
            }
            let _ = addon;
        }
        let Some(candidate) = candidates
            .iter()
            .find(|c| !completed.contains(&(c.addon.url.clone(), c.preview.source_id.clone())))
            .cloned()
        else {
            if !searched {
                searched = true;
                candidates.extend(search_candidates(
                    &native,
                    &query,
                    caller_url,
                    addons,
                    &result.connections,
                    &sources,
                    operation,
                ));
                continue;
            }
            break;
        };
        let MetadataCandidate {
            addon: installed,
            preview,
            rank,
            response,
        } = candidate;
        completed.insert((installed.url.clone(), preview.source_id.clone()));
        let Ok(addon) = addons::Addon::new(&installed.url) else {
            continue;
        };
        let adapter = StremioProvider::new(addon.clone(), installed.manifest.clone());
        let mut media = preview.clone();
        let mut videos = vec![];
        if installed
            .manifest
            .accepts("meta", &native.media_type, &preview.source_id)
        {
            let detail_url = addon.meta_url(&native.media_type, &preview.source_id);
            if let Some(mut response) =
                response.or_else(|| candidate_response(&installed, &preview, operation))
                && let Some(meta) = response.get_mut("meta")
            {
                // Candidate metadata uses the same image repair as direct
                // Cinemeta details, within this operation's remaining budget.
                cinemeta::repair_with(&detail_url, meta, |url| operation.fetch(url));
                let bytes = serde_json::to_vec(&response).unwrap_or_default();
                if let Ok(Some(detail)) = addons::Addon::parse_meta(&bytes)
                    && detail.preview.id == preview.source_id
                    && detail.preview.type_ == native.media_type
                {
                    media = adapter.convert_detail(detail.clone()).0;
                    videos = detail.videos;
                } else {
                    continue;
                }
            } else {
                continue;
            }
        }
        if crate::matching::ids_conflict(&native, &media) {
            continue;
        }
        let mappings = crate::sequence::map_episodes(&media, &videos, &sources);
        // Prefix/shortened titles require episode confirmation. Exact source
        // families can establish a show connection without inventing episodes.
        if rank == 1 && mappings.is_empty() {
            continue;
        }
        let connection = MetadataConnection {
            addon_id: addon_metadata_id(&installed.url),
            media_id: media.source_id.clone(),
            ids: all_ids(&media),
            basis: if rank == 3 {
                "identity-or-exact-title"
            } else {
                "series-family-and-episode-alignment"
            }
            .into(),
        };
        connected_media.insert(
            (connection.addon_id.clone(), connection.media_id.clone()),
            media.clone(),
        );
        merge_media(&mut result.details.media, &media);
        for (native_id, target_id) in mappings {
            // Only enrich this entry's available episodes, not related parts.
            if !result
                .details
                .episodes
                .iter()
                .any(|e| e.stable_id() == native_id)
            {
                continue;
            }
            let Some(video) = videos.iter().find(|v| v.id == target_id) else {
                continue;
            };
            if !valid_alias(&target_id) {
                continue;
            }
            let enrichment = result.episode_metadata.entry(native_id).or_default();
            if !enrichment
                .ids
                .iter()
                .zip(&enrichment.connections)
                .any(|(id, existing)| id == &target_id && existing.addon_id == connection.addon_id)
            {
                enrichment.ids.push(target_id.clone());
                enrichment.connections.push(connection.clone());
            }
            mapped_videos.insert((connection.addon_id.clone(), target_id), video.clone());
        }
        result.connections.push(connection);
    }
    // Independent catalog matches can disagree. Retain their claims for
    // inspection, but never pick whichever addon happened to answer first.
    for connection in &mut result.connections {
        if connection_conflicts(connection, &result.details.media) {
            connection.basis = "conflicting-identifiers".into();
        }
    }
    for mapping in result.episode_metadata.values_mut() {
        let accepted = mapping
            .ids
            .iter()
            .cloned()
            .zip(mapping.connections.iter().cloned())
            .filter(|(_, c)| !connection_conflicts(c, &result.details.media))
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        let distinct = accepted
            .iter()
            .filter(|(id, _)| seen.insert(id.clone()))
            .collect::<Vec<_>>();
        mapping.ids = distinct.iter().map(|(id, _)| id.clone()).collect();
        mapping.connections = distinct.into_iter().map(|(_, c)| c.clone()).collect();
        // Select fields only after conflicting connections have been removed.
        // Numbering comes from the first accepted mapping; later confirmed
        // sources can fill missing text/art instead of being blocked by it.
        for (id, connection) in accepted {
            let Some(video) = mapped_videos.get(&(connection.addon_id, id)) else {
                continue;
            };
            if mapping.season.is_none() {
                mapping.season = video.season;
                mapping.episode = video.episode_number();
            }
            let title = video.label();
            merge_optional_text(
                &mut mapping.title,
                &(!crate::sequence::episode_title(&title).is_empty()).then_some(title),
            );
            merge_optional_text(&mut mapping.released, &video.released);
            merge_optional_text(&mut mapping.thumbnail, &video.thumbnail);
            let mut overview = video.overview.clone();
            merge_optional_text(
                &mut overview,
                &video
                    .extra
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            merge_optional_text(&mut mapping.overview, &overview);
        }
    }
    result.episode_metadata.retain(|_, m| !m.ids.is_empty());
    if !result.connections.is_empty() {
        let ids = result.details.media.external_ids.clone();
        result.details.media = native;
        for connection in result
            .connections
            .iter()
            .filter(|connection| connection.basis != "conflicting-identifiers")
        {
            if let Some(media) =
                connected_media.get(&(connection.addon_id.clone(), connection.media_id.clone()))
            {
                merge_media(&mut result.details.media, media);
            }
        }
        result.details.media.external_ids = ids;
    }
    result.status = if result.connections.is_empty() {
        "missing"
    } else if result
        .connections
        .iter()
        .all(|c| c.basis == "conflicting-identifiers")
    {
        "ambiguous"
    } else {
        "confirmed"
    }
    .into();

    result
}

#[derive(Clone)]
struct MetadataCandidate {
    addon: MetadataAddon,
    preview: MediaItem,
    rank: u32,
    response: Option<Value>,
}

fn candidate_response(
    installed: &MetadataAddon,
    preview: &MediaItem,
    operation: &mut Operation,
) -> Option<Value> {
    if operation.detail_calls >= 6 {
        return None;
    }
    let addon = addons::Addon::new(&installed.url).ok()?;
    if !installed
        .manifest
        .accepts("meta", &preview.media_type, &preview.source_id)
    {
        return None;
    }
    operation.detail_calls += 1;
    let bytes = operation.fetch(&addon.meta_url(&preview.media_type, &preview.source_id))?;
    let response: Value = serde_json::from_slice(&bytes).ok()?;
    if let Some(detail) = addons::Addon::parse_meta(&bytes).ok()? {
        if detail.preview.id != preview.source_id || detail.preview.type_ != preview.media_type {
            return None;
        }
    } else {
        // Stremio's empty response is a completed lookup with no detail, not
        // a transport failure. Do not interpret error payloads as absence.
        let object = response.as_object()?;
        if !object.is_empty() && !(object.len() == 1 && object.get("meta") == Some(&Value::Null)) {
            return None;
        }
    }
    Some(response)
}

/// Prefer verified external IDs before spending the optional deadline on
/// title searches. Already connected addons need no speculative second match.
fn search_candidates(
    native: &MediaItem,
    query: &str,
    caller_url: &str,
    addons: &[MetadataAddon],
    known: &[MetadataConnection],
    sources: &[SourceSequence],
    operation: &mut Operation,
) -> Vec<MetadataCandidate> {
    let mut candidates = Vec::new();
    for installed in addons.iter().filter(|a| a.url != caller_url) {
        if known
            .iter()
            .any(|connection| connection.addon_id == addon_metadata_id(&installed.url))
        {
            continue;
        }
        let Ok(addon) = addons::Addon::new(&installed.url) else {
            continue;
        };
        let adapter = StremioProvider::new(addon.clone(), installed.manifest.clone());
        let mut found = Vec::new();
        for catalog in installed.manifest.search_catalogs(&native.media_type) {
            if catalog
                .extra
                .iter()
                .any(|extra| extra.is_required && extra.name != "search" && extra.name != "skip")
            {
                continue;
            }
            if operation.calls >= 26 {
                break;
            }
            let Some(bytes) = operation.fetch(&addon.catalog_url(
                &native.media_type,
                &catalog.id,
                &[("search", query)],
            )) else {
                continue;
            };
            for preview in addons::Addon::parse_catalog(&bytes)
                .unwrap_or_default()
                .into_iter()
                .take(100)
            {
                let mut preview = preview;
                if preview.type_.is_empty() {
                    preview.type_ = native.media_type.clone();
                }
                let item = adapter.convert_preview(preview);
                let rank = crate::sequence::candidate_rank(native, &item);
                if rank > 0 {
                    found.push((rank, item));
                }
            }
        }
        found.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
        let mut seen = BTreeSet::new();
        found.retain(|(_, item)| seen.insert(item.source_id.clone()));
        let Some((rank, _)) = found.first() else {
            continue;
        };
        let best = found
            .iter()
            .take_while(|(other, _)| other == rank)
            .collect::<Vec<_>>();
        if best.len() == 1 {
            candidates.push(MetadataCandidate {
                addon: installed.clone(),
                preview: best[0].1.clone(),
                rank: *rank,
                response: None,
            });
        } else if *rank == 1 && best.len() <= 6_usize.saturating_sub(operation.detail_calls) {
            // Short family titles can tie with picture dramas or spinoffs.
            // Evaluate every competitor before choosing: failed/malformed detail
            // responses leave the tie unresolved, regardless of catalog order.
            let mut proven = Vec::new();
            let mut complete = true;
            for (_, preview) in best {
                let Some(response) = candidate_response(installed, preview, operation) else {
                    complete = false;
                    break;
                };
                let bytes = serde_json::to_vec(&response).unwrap_or_default();
                let detail = match addons::Addon::parse_meta(&bytes) {
                    Ok(Some(detail)) => detail,
                    Ok(None) => continue,
                    Err(_) => {
                        complete = false;
                        break;
                    }
                };
                let media = adapter.convert_detail(detail.clone()).0;
                if !crate::matching::ids_conflict(native, &media)
                    && crate::sequence::map_episodes(&media, &detail.videos, sources).len() >= 2
                {
                    proven.push(MetadataCandidate {
                        addon: installed.clone(),
                        preview: preview.clone(),
                        rank: *rank,
                        response: Some(response),
                    });
                }
            }
            if complete && proven.len() == 1 {
                candidates.extend(proven);
            }
        }
    }
    candidates
}

fn connection_conflicts(connection: &MetadataConnection, media: &MediaItem) -> bool {
    connection.ids.iter().any(|id| {
        matches!(
            media.external_ids.resolve_id(id.namespace()),
            crate::IdResolution::Conflict(_)
        )
    })
}

fn all_ids(media: &MediaItem) -> Vec<ExternalId> {
    [
        crate::IdNamespace::Imdb,
        crate::IdNamespace::TmdbTv,
        crate::IdNamespace::TmdbMovie,
        crate::IdNamespace::MalAnime,
        crate::IdNamespace::AnilistAnime,
    ]
    .into_iter()
    .filter_map(|ns| match media.external_ids.resolve_id(ns) {
        crate::IdResolution::Unique(id) => Some(id),
        _ => None,
    })
    .collect()
}
fn external_request_ids(media: &MediaItem) -> Vec<String> {
    all_ids(media)
        .iter()
        .map(|id| match id {
            ExternalId::Imdb(id) => id.clone(),
            ExternalId::TmdbTv(id) => format!("tmdb:{id}"),
            ExternalId::TmdbMovie(id) => format!("tmdb:{id}"),
            ExternalId::MalAnime(id) => format!("mal:{id}"),
            ExternalId::AnilistAnime(id) => format!("anilist:{id}"),
            other => format!("{other:?}"),
        })
        .collect()
}

pub(crate) fn merge_media(media: &mut MediaItem, canonical: &MediaItem) {
    for id in all_ids(canonical) {
        if !media.external_ids.typed.contains(&id) {
            media.external_ids.typed.push(id);
        }
    }
    media.external_ids.imdb = media
        .external_ids
        .imdb
        .take()
        .or_else(|| canonical.external_ids.imdb.clone());
    media.external_ids.tmdb = media
        .external_ids
        .tmdb
        .take()
        .or_else(|| canonical.external_ids.tmdb.clone());
    media.external_ids.mal = media
        .external_ids
        .mal
        .take()
        .or_else(|| canonical.external_ids.mal.clone());
    media.external_ids.anilist = media
        .external_ids
        .anilist
        .take()
        .or_else(|| canonical.external_ids.anilist.clone());
    for (field, fallback) in [
        (&mut media.year, &canonical.year),
        (&mut media.poster, &canonical.poster),
        (&mut media.background, &canonical.background),
        (&mut media.description, &canonical.description),
    ] {
        merge_optional_text(field, fallback);
    }
    if media.genres.is_empty() {
        media.genres = canonical.genres.clone();
    }
    for ns in [
        crate::IdNamespace::Imdb,
        crate::IdNamespace::TmdbTv,
        crate::IdNamespace::TmdbMovie,
        crate::IdNamespace::MalAnime,
        crate::IdNamespace::AnilistAnime,
    ] {
        if matches!(
            media.external_ids.resolve_id(ns),
            crate::IdResolution::Conflict(_)
        ) {
            match ns {
                crate::IdNamespace::Imdb => media.external_ids.imdb = None,
                crate::IdNamespace::TmdbTv | crate::IdNamespace::TmdbMovie => {
                    media.external_ids.tmdb = None
                }
                crate::IdNamespace::MalAnime => media.external_ids.mal = None,
                _ => media.external_ids.anilist = None,
            }
        }
    }
}

fn merge_optional_text(value: &mut Option<String>, fallback: &Option<String>) {
    *value = value
        .take()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| fallback.clone().filter(|value| !value.trim().is_empty()));
}

pub(crate) fn valid_alias(id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}

/// Apply an enrichment result to the ordinary addon response. The native
/// media/video IDs remain unchanged, including for providers with private IDs.
pub fn apply_enrichment(meta: &mut Value, result: &EnrichmentResult) {
    let preview = crate::registry::media_preview(result.details.media.clone());
    if let (Some(meta), Some(preview)) = (meta.as_object_mut(), preview.as_object()) {
        for (key, value) in preview {
            if key != "id" && key != "type" {
                meta.insert(key.clone(), value.clone());
            }
        }
        meta.insert("novaConnections".into(), json!(result.connections));
        meta.insert(
            "novaMetadataRevision".into(),
            json!(result.inventory_revision),
        );
        if let Some(videos) = meta.get_mut("videos").and_then(Value::as_array_mut) {
            for video in videos {
                let id = video["id"].as_str().unwrap_or_default();
                if let Some(mapping) = result.episode_metadata.get(id) {
                    video["novaMetadataRevision"] = json!(result.inventory_revision);
                    video["novaStreamIds"] = json!(mapping.ids);
                    video["novaConnections"] = json!(mapping.connections);
                    if let Some(season) = mapping.season {
                        video["season"] = json!(season);
                    }
                    if let Some(number) = mapping.episode {
                        video["episode"] = json!(number);
                        video["number"] = json!(number);
                    }
                    for (field, value) in [
                        ("name", &mapping.title),
                        ("released", &mapping.released),
                        ("thumbnail", &mapping.thumbnail),
                        ("overview", &mapping.overview),
                    ] {
                        if let Some(value) = value {
                            video[field] = json!(value);
                        }
                    }
                }
            }
        }
    }
}

/// Enrich ordinary installed-addon details through the same broker as JS.
/// Candidate fetches carry the thread-local guard and cannot recursively enrich.
pub fn enrich_addon_response(raw_url: &str, bytes: Vec<u8>) -> Vec<u8> {
    if nested() {
        return bytes;
    }
    let Ok(mut response) = serde_json::from_slice::<Value>(&bytes) else {
        return bytes;
    };
    let Some(meta) = response.get_mut("meta").filter(|m| m.is_object()) else {
        return bytes;
    };
    if meta.get("novaConnections").is_some() {
        return bytes;
    }
    let image_transport = transport().lock().unwrap().clone();
    if let Some(transport) = image_transport {
        let _guard = NestedGuard::new();
        cinemeta::repair(raw_url, meta, transport.as_ref());
    }
    let bytes = serde_json::to_vec(&response).unwrap_or(bytes);
    let meta = &mut response["meta"];
    let Ok(Some(detail)) = addons::Addon::parse_meta(&bytes) else {
        return bytes;
    };
    let installed = snapshot()
        .lock()
        .unwrap()
        .addons
        .iter()
        .find(|a| {
            addons::Addon::new(&a.url).is_ok_and(|addon| {
                addon.meta_url(&detail.preview.type_, &detail.preview.id) == raw_url
            })
        })
        .cloned();
    let Some(installed) = installed else {
        return bytes;
    };
    let Ok(addon) = addons::Addon::new(&installed.url) else {
        return bytes;
    };
    let adapter = StremioProvider::new(addon, installed.manifest);
    let (mut media, mut episodes) = adapter.convert_detail(detail);
    media.provider_id.clear();
    for e in &mut episodes {
        e.provider_id.clear();
    }
    let result = enrich_metadata(
        EnrichmentRequest {
            details: ProviderDetails { media, episodes },
            source_sequences: vec![],
            targets: vec![],
            query: None,
        },
        &installed.url,
    );
    apply_enrichment(meta, &result);
    meta["novaSourceUrl"] = json!(installed.url);
    if let Some(videos) = meta["videos"].as_array_mut() {
        for video in videos {
            video["novaSourceUrl"] = json!(installed.url);
        }
    }
    serde_json::to_vec(&response).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Episode, ExternalIds, MemoryProviderHost, PluginManifest, PluginRuntime};

    #[test]
    fn inventory_revision_ignores_manifest_map_iteration_order() {
        let (mut first, _) = fixture();
        let mut second = first.clone();
        for i in 0..20 {
            first[0]
                .manifest
                .extra
                .insert(format!("field{i}"), json!(i));
        }
        for i in (0..20).rev() {
            second[0]
                .manifest
                .extra
                .insert(format!("field{i}"), json!(i));
        }
        assert_eq!(
            inventory_fingerprint(&first),
            inventory_fingerprint(&second)
        );
        second[0].manifest.version = "changed".into();
        assert_ne!(
            inventory_fingerprint(&first),
            inventory_fingerprint(&second)
        );
    }

    struct FixtureTransport {
        responses: BTreeMap<String, Value>,
        calls: Mutex<Vec<String>>,
        check_nested: bool,
    }
    impl AddonMetadataTransport for FixtureTransport {
        fn fetch(
            &self,
            url: &str,
            timeout: Duration,
            limit: usize,
        ) -> Result<Vec<u8>, ProviderHostError> {
            assert!(timeout <= Duration::from_secs(5));
            assert_eq!(limit, RESPONSE_LIMIT);
            if self.check_nested {
                assert!(nested());
                // Fetching addon candidate metadata cannot launch a second
                // enrichment operation, even with another source identity.
                let recursive = enrich_metadata(request(), "nova-provider://other");
                assert_eq!(recursive.status, "missing");
            }
            self.calls.lock().unwrap().push(url.into());
            self.responses
                .get(url)
                .map(|v| serde_json::to_vec(v).unwrap())
                .ok_or_else(|| ProviderHostError("fixture unavailable".into()))
        }
    }
    fn addon(url: &str, prefix: &str, search: bool) -> MetadataAddon {
        MetadataAddon {url:url.into(),manifest:serde_json::from_value(json!({"id":url,"name":"Fixture","version":"1","types":["series"],"idPrefixes":[prefix],"resources":["catalog","meta"],"catalogs":if search {json!([{"type":"series","id":"search","name":"Search","extra":[{"name":"search"}]}])} else {json!([])}})).unwrap()}
    }
    fn request() -> EnrichmentRequest {
        EnrichmentRequest {
            details: ProviderDetails {
                media: MediaItem {
                    provider_id: "fixture".into(),
                    source_id: "show".into(),
                    media_type: "series".into(),
                    title: "Show".into(),
                    year: Some("2020".into()),
                    external_ids: ExternalIds::default(),
                    ..Default::default()
                },
                episodes: vec![Episode {
                    provider_id: "fixture".into(),
                    source_id: "ep:1".into(),
                    parent_id: "show".into(),
                    number: 1,
                    season: 1,
                    title: "Opening".into(),
                    ..Default::default()
                }],
            },
            source_sequences: vec![SourceSequence {
                media_id: "fixture:show".into(),
                title: "Show".into(),
                year: Some("2020".into()),
                episodes: vec![crate::SequenceEpisode {
                    id: "fixture:ep:1".into(),
                    number: 1,
                    title: "Opening".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            targets: vec![
                "imdb".into(),
                "mal:anime".into(),
                "anilist:anime".into(),
                "tmdb:tv".into(),
            ],
            query: Some("Show".into()),
        }
    }
    fn fixture() -> (Vec<MetadataAddon>, Arc<FixtureTransport>) {
        let imdb = addon("https://imdb.example", "tt", true);
        let anime = addon("https://anime.example", "mal:", false);
        let imdb_url = addons::Addon::new(&imdb.url).unwrap();
        let anime_url = addons::Addon::new(&anime.url).unwrap();
        let canonical = json!({"id":"tt100","type":"series","name":"Show","year":"2020","mal_id":27,"description":"Canonical description","videos":[{"id":"tt100:1:1","name":"Opening","season":1,"episode":1,"released":"2020-01-01","thumbnail":"https://images.example/one.jpg"}]});
        let transport = Arc::new(FixtureTransport {
            responses: BTreeMap::from([
                (
                    imdb_url.catalog_url("series", "search", &[("search", "Show")]),
                    json!({"metas":[canonical.clone()]}),
                ),
                (
                    imdb_url.meta_url("series", "tt100"),
                    json!({"meta":canonical}),
                ),
                (
                    anime_url.meta_url("series", "mal:27"),
                    json!({"meta":{"id":"mal:27","type":"series","name":"Show","year":"2020","anilist_id":42,"tmdb_tv_id":123,"imdb_id":"tt100"}}),
                ),
            ]),
            calls: Mutex::new(vec![]),
            check_nested: true,
        });
        (vec![imdb, anime], transport)
    }
    fn run(
        request: EnrichmentRequest,
        addons: &[MetadataAddon],
        transport: Arc<dyn AddonMetadataTransport>,
    ) -> EnrichmentResult {
        let _guard = NestedGuard::new();
        enrich_with(
            request,
            "nova-provider://fixture",
            addons,
            &mut Operation {
                start: Instant::now(),
                calls: 0,
                detail_calls: 0,
                budget: Duration::from_secs(5),
                visited: BTreeSet::new(),
                transport,
            },
        )
    }

    #[test]
    fn weak_catalog_ties_require_one_proven_parent_and_complete_competitor_responses() {
        for reverse in [false, true] {
            for rival in [
                "unrelated",
                "matching",
                "unavailable",
                "wrong-id",
                "empty",
                "null",
                "error",
                "malformed",
            ] {
                let installed = addon("https://metadata.example", "tt", true);
                let endpoint = addons::Addon::new(&installed.url).unwrap();
                let mut request = request();
                request.details.media.title = "Code Geass: Lelouch of the Rebellion R2".into();
                request.details.media.aliases = vec!["Code Geass: Hangyaku no Lelouch R2".into()];
                request.details.media.year = Some("2008".into());
                request.query = Some("code geass lelouch of the rebellion".into());
                request.details.episodes = (1..=2)
                    .map(|n| Episode {
                        provider_id: "fixture".into(),
                        source_id: format!("ep:{n}"),
                        parent_id: "show".into(),
                        season: 1,
                        number: n,
                        title: format!("Distinct story {n}"),
                        ..Default::default()
                    })
                    .collect();
                request.source_sequences.clear();
                let mut previews = vec![
                    json!({"id":"tt100", "type":"series", "name":"Code Geass", "releaseInfo":"2006–2008"}),
                    json!({"id":"tt200", "type":"series", "name":"Code Geass: Hangyaku no Lelouch R2 Picture Drama", "releaseInfo":"2008"}),
                ];
                if reverse {
                    previews.reverse();
                }
                let videos = |id: &str, matching: bool| {
                    (1..=2).map(|n| json!({
                    "id":format!("{id}:2:{n}"), "season":2, "episode":n,
                    "title": if matching { format!("Distinct story {n}") } else { format!("Picture story {n}") },
                    "released":"2008-05-01", "thumbnail":format!("https://images.example/{n}.jpg")
                })).collect::<Vec<_>>()
                };
                let mut responses = BTreeMap::from([
                    (
                        endpoint.catalog_url(
                            "series",
                            "search",
                            &[("search", request.query.as_deref().unwrap())],
                        ),
                        json!({"metas":previews}),
                    ),
                    (
                        endpoint.meta_url("series", "tt100"),
                        json!({"meta":{
                            "id":"tt100", "type":"series", "name":"Code Geass", "releaseInfo":"2006–2008", "videos":videos("tt100", true)
                        }}),
                    ),
                ]);
                if ["empty", "null", "error", "malformed"].contains(&rival) {
                    responses.insert(
                        endpoint.meta_url("series", "tt200"),
                        match rival {
                            "empty" => json!({}),
                            "null" => json!({"meta":null}),
                            "error" => json!({"error":"temporary failure"}),
                            _ => json!({"meta":{"id":null,"type":"series"}}),
                        },
                    );
                } else if rival != "unavailable" {
                    responses.insert(endpoint.meta_url("series", "tt200"), json!({"meta":{
                        "id":if rival == "wrong-id" { "tt999" } else { "tt200" }, "type":"series",
                        "name":"Code Geass: Hangyaku no Lelouch R2 Picture Drama", "releaseInfo":"2008",
                        "videos":videos("tt200", rival == "matching")
                    }}));
                }
                let transport = Arc::new(FixtureTransport {
                    responses,
                    calls: Mutex::new(vec![]),
                    check_nested: false,
                });
                let result = run(request, &[installed], transport.clone());
                if ["unrelated", "empty", "null"].contains(&rival) {
                    assert_eq!(result.connections.len(), 1);
                    assert_eq!(result.connections[0].media_id, "tt100");
                    assert_eq!(result.episode_metadata.len(), 2);
                    assert_eq!(result.episode_metadata["fixture:ep:1"].season, Some(2));
                    assert!(
                        result
                            .episode_metadata
                            .values()
                            .all(|m| m.thumbnail.is_some())
                    );
                } else {
                    assert!(result.connections.is_empty(), "{reverse} {rival}");
                    assert!(result.episode_metadata.is_empty());
                }
                let parent_requests = transport
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|url| url.contains("/meta/series/tt100"))
                    .count();
                assert!(parent_requests <= 1);
                if ["unrelated", "empty", "null"].contains(&rival) {
                    assert_eq!(parent_requests, 1);
                }
            }
        }
    }

    #[test]
    fn weak_ties_larger_than_detail_budget_remain_unresolved() {
        let installed = addon("https://metadata.example", "tt", true);
        let endpoint = addons::Addon::new(&installed.url).unwrap();
        let mut request = request();
        request.details.media.title = "Show: Main Story Season 2".into();
        request.details.media.year = Some("2021".into());
        request.query = Some("show main story".into());
        let previews = (1..=7)
            .map(|n| {
                json!({"id":format!("tt{n}"),
            "type":"series", "name":format!("Show: Main Story Edition {n}"),
            "releaseInfo":"2020"})
            })
            .collect::<Vec<_>>();
        let transport = Arc::new(FixtureTransport {
            responses: BTreeMap::from([(
                endpoint.catalog_url("series", "search", &[("search", "show main story")]),
                json!({"metas":previews}),
            )]),
            calls: Mutex::new(vec![]),
            check_nested: false,
        });
        let result = run(request, &[installed], transport.clone());
        assert!(result.connections.is_empty());
        assert!(result.episode_metadata.is_empty());
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn ongoing_native_entry_receives_metadata_without_adding_unavailable_episodes() {
        let installed = addon("https://metadata.example", "tt", true);
        let endpoint = addons::Addon::new(&installed.url).unwrap();
        let mut request = request();
        request.details.media.title = "Example Season 3".into();
        request.details.media.year = Some("2026".into());
        request.query = Some("example".into());
        request.details.episodes = (1..=2)
            .map(|n| Episode {
                provider_id: "fixture".into(),
                source_id: format!("ep:{n}"),
                parent_id: "show".into(),
                season: 1,
                number: n,
                title: format!("Episode {n}: Episode {n}"),
                ..Default::default()
            })
            .collect();
        request.source_sequences.clear();
        let videos = (1..=12)
            .map(|n| {
                json!({"id":format!("tt100:3:{n}"),"season":3,"episode":n,
            "title":format!("Real title {n}"),"released":"2026-10-04",
            "thumbnail":format!("https://images.example/{n}.jpg"),"overview":"Episode description"})
            })
            .collect::<Vec<_>>();
        let transport = Arc::new(FixtureTransport {
            responses: BTreeMap::from([
                (
                    endpoint.catalog_url("series", "search", &[("search", "example")]),
                    json!({"metas":[{"id":"tt100","type":"series","name":"Example","releaseInfo":"2024–2026"}]}),
                ),
                (
                    endpoint.meta_url("series", "tt100"),
                    json!({"meta":{"id":"tt100","type":"series","name":"Example","releaseInfo":"2024–2026","videos":videos}}),
                ),
            ]),
            calls: Mutex::new(vec![]),
            check_nested: false,
        });
        let result = run(request, &[installed], transport);
        assert_eq!(result.episode_metadata.len(), 2);
        assert_eq!(result.details.episodes.len(), 2);
        let mut meta =
            json!({"id":"native-show","videos":[{"id":"fixture:ep:1"},{"id":"fixture:ep:2"}]});
        // Applying enrichment also leaves native playback identities untouched.
        apply_enrichment(&mut meta, &result);
        assert_eq!(meta["id"], "native-show");
        assert_eq!(meta["videos"].as_array().unwrap().len(), 2);
        assert_eq!(meta["videos"][0]["id"], "fixture:ep:1");
        assert_eq!(meta["videos"][0]["season"], 3);
        assert_eq!(meta["videos"][0]["name"], "Real title 1");
        assert_eq!(meta["videos"][0]["novaStreamIds"], json!(["tt100:3:1"]));
        assert!(meta["videos"][0]["thumbnail"].is_string());
        assert_eq!(result.episode_metadata["fixture:ep:1"].season, Some(3));
        assert_eq!(
            result.episode_metadata["fixture:ep:1"].title.as_deref(),
            Some("Real title 1")
        );
        assert_eq!(
            result.episode_metadata["fixture:ep:1"].overview.as_deref(),
            Some("Episode description")
        );
        assert!(
            result
                .episode_metadata
                .values()
                .all(|m| m.thumbnail.is_some())
        );
    }

    #[test]
    fn subtitled_season_maps_to_parent_in_split_and_combined_layouts() {
        for combined in [false, true] {
            let title = "Solo Leveling Season 2: Arise from the Shadow";
            let installed = addon("https://v3-cinemeta.strem.io", "tt", true);
            let endpoint = addons::Addon::new(&installed.url).unwrap();
            let fixture: Value =
                serde_json::from_str(include_str!("metadata/fixtures/cinemeta-numbering.json"))
                    .unwrap();
            let mut canonical = fixture[if combined { "live" } else { "native" }].clone();
            // The fixture contains just the second cour. Titles still prove
            // alignment when the source's siblings are unavailable.
            canonical["id"] = json!("tt21209876");
            canonical["type"] = json!("series");
            canonical["name"] = json!("Solo Leveling");
            canonical["year"] = json!("2024");
            let mut req = request();
            req.details.media.title = title.into();
            req.details.media.year = Some("2025".into());
            req.query = None;
            req.source_sequences.clear();
            let original_videos = fixture["native"]["videos"].as_array().unwrap();
            req.details.episodes = original_videos
                .iter()
                .enumerate()
                .map(|(i, v)| Episode {
                    provider_id: "fixture".into(),
                    source_id: format!("ep:{}", i + 1),
                    parent_id: "show".into(),
                    number: i as u32 + 1,
                    season: 1,
                    title: format!("Episode {}: {}", i + 1, v["name"].as_str().unwrap()),
                    released: v["released"].as_str().map(str::to_owned),
                    ..Default::default()
                })
                .collect();
            let transport = Arc::new(FixtureTransport {
                responses: BTreeMap::from([
                    (
                        endpoint.catalog_url("series", "search", &[("search", "solo leveling")]),
                        json!({"metas":[canonical.clone()]}),
                    ),
                    (
                        endpoint.meta_url("series", "tt21209876"),
                        json!({"meta":canonical}),
                    ),
                    (
                        "https://cinemeta-live.strem.io/meta/series/tt21209876.json".into(),
                        json!({"meta":fixture["live"].clone()}),
                    ),
                ]),
                calls: Mutex::new(vec![]),
                check_nested: false,
            });
            let result = run(req, &[installed], transport);
            assert_eq!(result.status, "confirmed");
            assert_eq!(result.episode_metadata.len(), 13, "combined={combined}");
            for i in 1..=13 {
                let native_id = format!("fixture:ep:{i}");
                let metadata = &result.episode_metadata[&native_id];
                let (season, number) = if combined { (1, i + 12) } else { (2, i) };
                assert_eq!(metadata.ids, vec![format!("tt21209876:{season}:{number}")]);
                assert_eq!(
                    metadata.thumbnail,
                    Some(format!(
                        "https://episodes.metahub.space/tt21209876/1/{}/w780.jpg",
                        i + 12
                    ))
                );
                assert_eq!(
                    (metadata.season, metadata.episode),
                    (Some(season), Some(number))
                );
                assert_eq!(
                    result.details.episodes[i as usize - 1].stable_id(),
                    native_id
                );
                assert_eq!(result.details.episodes[i as usize - 1].number, i);
            }
        }
    }

    #[test]
    fn single_native_season_uses_explicit_parent_season_even_without_title_anchors() {
        let (addons, transport) = fixture();
        let mut responses = transport.responses.clone();
        let installed = addons[0].clone();
        let endpoint = addons::Addon::new(&installed.url).unwrap();
        let mut canonical = responses[&endpoint.meta_url("series", "tt100")]["meta"].clone();
        canonical["videos"][0]["season"] = json!(2);
        canonical["videos"][0]["id"] = json!("tt100:2:1");
        canonical["videos"][0]["released"] = json!("2021-01-01");
        responses.insert(
            endpoint.meta_url("series", "tt100"),
            json!({"meta":canonical}),
        );
        responses.insert(
            endpoint.catalog_url("series", "search", &[("search", "show")]),
            json!({"metas":[{"id":"tt100","type":"series","name":"Show","year":"2020"}]}),
        );
        let mut req = request();
        req.query = None;
        req.source_sequences.clear();
        req.details.media.title = "Show Season 2: A New Arc".into();
        req.details.media.year = Some("2021".into());
        req.details.episodes[0].title = "Episode 1".into();
        let result = run(
            req,
            &[installed],
            Arc::new(FixtureTransport {
                responses,
                calls: Mutex::new(vec![]),
                check_nested: false,
            }),
        );
        assert_eq!(
            result.episode_metadata["fixture:ep:1"].ids,
            vec!["tt100:2:1"]
        );
        assert_eq!(result.details.episodes[0].season, 1);
    }

    #[test]
    fn normal_addon_search_expands_all_supported_ids_and_episode_aliases() {
        let (addons, transport) = fixture();
        let result = run(request(), &addons, transport.clone());
        assert_eq!(result.details.media.stable_id(), "fixture:show");
        assert_eq!(result.details.episodes[0].stable_id(), "fixture:ep:1");
        for ns in [
            crate::IdNamespace::Imdb,
            crate::IdNamespace::MalAnime,
            crate::IdNamespace::AnilistAnime,
            crate::IdNamespace::TmdbTv,
        ] {
            assert!(matches!(
                result.details.media.external_ids.resolve_id(ns),
                crate::IdResolution::Unique(_)
            ));
        }
        assert_eq!(
            result.episode_metadata["fixture:ep:1"].ids,
            vec!["tt100:1:1"]
        );
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().any(|u| u.contains("/catalog/series/search/")));
        assert!(calls.iter().any(|u| u.contains("mal:27")));
    }

    #[test]
    fn explicit_ids_are_resolved_before_optional_title_searches() {
        let (installed, transport) = fixture();
        let mut native = request();
        native.details.media.external_ids.mal = Some("27".into());
        let result = run(native, &installed, transport.clone());
        assert_eq!(result.status, "confirmed");
        assert_eq!(result.episode_metadata["fixture:ep:1"].ids, ["tt100:1:1"]);
        let calls = transport.calls.lock().unwrap();
        assert!(calls[0].contains("/meta/series/mal:27.json"));
        assert!(
            calls.iter().all(|url| !url.contains("/catalog/")),
            "known ID connections do not spend the deadline on redundant searches"
        );
    }

    #[test]
    fn later_confirmed_addons_fill_empty_artwork_and_descriptions() {
        let (installed, transport) = fixture();
        let first = addons::Addon::new(&installed[0].url).unwrap();
        let second = addons::Addon::new(&installed[1].url).unwrap();
        let mut responses = transport.responses.clone();
        let initial = &mut responses
            .get_mut(&first.meta_url("series", "tt100"))
            .unwrap()["meta"];
        initial["poster"] = json!(" ");
        initial["background"] = json!("");
        initial["videos"][0]["thumbnail"] = json!(" ");
        initial["videos"][0]["overview"] = json!("");
        let later = &mut responses
            .get_mut(&second.meta_url("series", "mal:27"))
            .unwrap()["meta"];
        later["poster"] = json!("https://images.example/later-poster.jpg");
        later["background"] = json!("https://images.example/later-background.jpg");
        later["videos"] = json!([{"id":"mal:27:1", "season":1, "episode":1, "name":"Opening", "thumbnail":"https://images.example/later-episode.jpg", "overview":"Later episode synopsis"}]);
        let mut request = request();
        request.details.media.poster = Some("\n".into());
        request.details.media.background = Some(String::new());
        request.details.media.description = Some("  ".into());
        let result = run(
            request,
            &installed,
            Arc::new(FixtureTransport {
                responses,
                calls: Mutex::new(vec![]),
                check_nested: false,
            }),
        );
        assert_eq!(result.status, "confirmed");
        assert_eq!(result.details.media.stable_id(), "fixture:show");
        assert_eq!(
            result.details.media.poster.as_deref(),
            Some("https://images.example/later-poster.jpg")
        );
        assert_eq!(
            result.details.media.background.as_deref(),
            Some("https://images.example/later-background.jpg")
        );
        assert_eq!(
            result.details.media.description.as_deref(),
            Some("Canonical description")
        );
        let mapping = &result.episode_metadata["fixture:ep:1"];
        assert_eq!(mapping.ids, ["tt100:1:1", "mal:27:1"]);
        assert_eq!((mapping.season, mapping.episode), (Some(1), Some(1)));
        assert_eq!(
            mapping.thumbnail.as_deref(),
            Some("https://images.example/later-episode.jpg")
        );
        assert_eq!(mapping.overview.as_deref(), Some("Later episode synopsis"));
    }

    #[test]
    fn conflicting_sources_cannot_supply_fields_or_hide_valid_duplicate_aliases() {
        let (mut installed, transport) = fixture();
        installed.truncate(1);
        let conflict = addon("https://conflict.example", "tt", true);
        let second = addons::Addon::new(&conflict.url).unwrap();
        let third = addon("https://clean.example", "tt", true);
        let endpoint = addons::Addon::new(&third.url).unwrap();
        let mut responses = transport.responses.clone();
        let conflicted = json!({"id":"tt100", "type":"series", "name":"Show", "year":"2020", "mal_id":28, "description":"Conflicting synopsis", "videos":[{"id":"tt100:1:1", "season":1, "episode":1, "name":"Opening", "thumbnail":"https://images.example/conflicting-episode.jpg"}]});
        responses.insert(
            second.catalog_url("series", "search", &[("search", "Show")]),
            json!({"metas":[conflicted.clone()]}),
        );
        responses.insert(
            second.meta_url("series", "tt100"),
            json!({"meta":conflicted}),
        );
        let clean = json!({"id":"tt100", "type":"series", "name":"Show", "year":"2020", "description":"Verified synopsis", "background":"https://images.example/verified-bg.jpg", "videos":[{"id":"tt100:1:1", "season":1, "episode":1, "name":"Opening", "thumbnail":"https://images.example/verified-episode.jpg"}]});
        responses.insert(
            endpoint.catalog_url("series", "search", &[("search", "Show")]),
            json!({"metas":[clean.clone()]}),
        );
        responses.insert(endpoint.meta_url("series", "tt100"), json!({"meta":clean}));
        installed.push(conflict);
        installed.push(third.clone());
        let result = run(
            request(),
            &installed,
            Arc::new(FixtureTransport {
                responses,
                calls: Mutex::new(vec![]),
                check_nested: false,
            }),
        );
        assert_eq!(result.status, "confirmed");
        assert!(
            result
                .connections
                .iter()
                .any(|connection| connection.basis == "conflicting-identifiers")
        );
        assert_eq!(
            result.details.media.description.as_deref(),
            Some("Verified synopsis")
        );
        assert_eq!(
            result.details.media.background.as_deref(),
            Some("https://images.example/verified-bg.jpg")
        );
        let mapping = &result.episode_metadata["fixture:ep:1"];
        assert_eq!(mapping.ids, ["tt100:1:1"]);
        assert_eq!(
            mapping.connections[0].addon_id,
            addon_metadata_id(&third.url)
        );
        assert_eq!(
            mapping.thumbnail.as_deref(),
            Some("https://images.example/verified-episode.jpg")
        );
    }

    #[test]
    fn duplicate_search_catalogs_do_not_create_false_ambiguity() {
        let (mut addons, transport) = fixture();
        let mut other = addons[0].manifest.catalogs[0].clone();
        other.id = "other-search".into();
        addons[0].manifest.catalogs.push(other);
        let addon = addons::Addon::new(&addons[0].url).unwrap();
        let mut responses = transport.responses.clone();
        let response =
            responses[&addon.catalog_url("series", "search", &[("search", "Show")])].clone();
        responses.insert(
            addon.catalog_url("series", "other-search", &[("search", "Show")]),
            response,
        );
        let result = run(
            request(),
            &addons,
            Arc::new(FixtureTransport {
                responses,
                calls: Mutex::new(vec![]),
                check_nested: false,
            }),
        );
        assert_eq!(result.status, "confirmed");
        assert_eq!(
            result.episode_metadata["fixture:ep:1"].ids,
            vec!["tt100:1:1"]
        );
    }

    #[test]
    fn conflicting_addons_keep_claims_and_remove_speculative_stream_aliases() {
        let (mut addons, transport) = fixture();
        let second = addon("https://second.example", "tt", true);
        let url = addons::Addon::new(&second.url).unwrap();
        let mut responses = transport.responses.clone();
        let other = json!({"id":"tt200","type":"series","name":"Show","year":"2020","videos":[{"id":"tt200:1:1","name":"Opening","season":1,"episode":1}]});
        responses.insert(
            url.catalog_url("series", "search", &[("search", "Show")]),
            json!({"metas":[other.clone()]}),
        );
        responses.insert(url.meta_url("series", "tt200"), json!({"meta":other}));
        addons.insert(1, second);
        let result = run(
            request(),
            &addons,
            Arc::new(FixtureTransport {
                responses,
                calls: Mutex::new(vec![]),
                check_nested: false,
            }),
        );
        assert!(matches!(
            result
                .details
                .media
                .external_ids
                .resolve_id(crate::IdNamespace::Imdb),
            crate::IdResolution::Conflict(_)
        ));
        assert!(result.details.media.external_ids.imdb.is_none());
        assert!(result.details.media.description.is_none());
        assert_eq!(result.status, "ambiguous");
        assert!(result.episode_metadata.is_empty());
        assert!(
            result
                .connections
                .iter()
                .any(|c| c.basis == "conflicting-identifiers")
        );
    }

    #[test]
    fn missing_addons_and_ambiguous_catalogs_leave_native_details() {
        let (addons, transport) = fixture();
        let result = run(request(), &[], transport.clone());
        assert!(result.connections.is_empty());
        assert_eq!(result.details.media.stable_id(), "fixture:show");
        let url = addons::Addon::new(&addons[0].url).unwrap();
        let transport = Arc::new(FixtureTransport {
            responses: BTreeMap::from([(
                url.catalog_url("series", "search", &[("search", "Show")]),
                json!({"metas":[{"id":"tt1","type":"series","name":"Show","year":"2020"},{"id":"tt2","type":"series","name":"Show","year":"2020"}]}),
            )]),
            calls: Mutex::new(vec![]),
            check_nested: false,
        });
        let result = run(request(), &addons, transport);
        assert!(result.connections.is_empty());
    }

    #[test]
    fn sparse_confirmed_metadata_expires_early_without_discarding_durable_aliases() {
        let state = MemoryProviderHost::new();
        let (addons, transport) = fixture();
        let complete = run(request(), &addons, transport);
        let key = "partial-expiration-test";
        for missing in ["title", "generic-title", "thumbnail", "mapping"] {
            let mut partial = complete.clone();
            match missing {
                "title" => {
                    partial
                        .episode_metadata
                        .get_mut("fixture:ep:1")
                        .unwrap()
                        .title = None
                }
                "generic-title" => {
                    partial
                        .episode_metadata
                        .get_mut("fixture:ep:1")
                        .unwrap()
                        .title = Some("Episode 1".into())
                }
                "thumbnail" => {
                    partial
                        .episode_metadata
                        .get_mut("fixture:ep:1")
                        .unwrap()
                        .thumbnail = Some(" ".into())
                }
                _ => partial.episode_metadata.clear(),
            }
            let raw = serde_json::to_string(&(now() - PARTIAL_CACHE_TTL - 1, &partial)).unwrap();
            state.storage_set(key, &raw).unwrap();
            assert!(cached_result(state.as_ref(), key).is_none(), "{missing}");
            assert_eq!(state.storage_get(key), Some(raw));
            assert_eq!(partial.connections[0].media_id, "tt100");
        }
        // Published episode fields return the same identity to the long-lived
        // cache; movies with no episodes also keep the existing lifetime.
        state
            .storage_set(
                key,
                &serde_json::to_string(&(now() - PARTIAL_CACHE_TTL - 1, &complete)).unwrap(),
            )
            .unwrap();
        assert_eq!(
            cached_result(state.as_ref(), key).unwrap().episode_metadata["fixture:ep:1"]
                .title
                .as_deref(),
            Some("Opening")
        );
        let mut movie = complete;
        movie.details.episodes.clear();
        assert_eq!(cache_ttl(&movie), CONFIRMED_CACHE_TTL);
    }

    #[test]
    fn session_cache_supports_large_entries_expiry_and_availability_fingerprints() {
        let state = MemoryProviderHost::new();
        let (addons, transport) = fixture();
        let mut result = run(request(), &addons, transport);
        result.status = "confirmed".into();
        result.details.media.description = Some("x".repeat(20 * 1024));
        let key = cache_key(1, "nova-provider://fixture", &request());
        cache_result(state.as_ref(), &key, &result);
        assert_eq!(
            cached_result(state.as_ref(), &key)
                .unwrap()
                .details
                .media
                .description
                .as_ref()
                .unwrap()
                .len(),
            20 * 1024
        );
        assert_ne!(key, cache_key(2, "nova-provider://fixture", &request()));
        let mut changed = request();
        changed.source_sequences[0].episodes[0].title = "Different".into();
        assert_ne!(key, cache_key(1, "nova-provider://fixture", &changed));
        state
            .storage_set(
                &key,
                &serde_json::to_string(&(now() - CONFIRMED_CACHE_TTL - 1, &result)).unwrap(),
            )
            .unwrap();
        assert!(cached_result(state.as_ref(), &key).is_none());
        result.status = "missing".into();
        state
            .storage_set(
                &key,
                &serde_json::to_string(&(now() - 31, &result)).unwrap(),
            )
            .unwrap();
        assert!(cached_result(state.as_ref(), &key).is_none());
        result.details.media.description = Some("x".repeat(300 * 1024));
        cache_result(state.as_ref(), "oversized", &result);
        assert!(state.storage_get("oversized").is_none());
    }

    struct JsFixtureHost {
        state: Arc<MemoryProviderHost>,
        addons: Vec<MetadataAddon>,
        transport: Arc<FixtureTransport>,
    }
    impl ProviderHost for JsFixtureHost {
        fn get(
            &self,
            _url: &str,
            _headers: &BTreeMap<String, String>,
        ) -> Result<crate::HttpResponse, ProviderHostError> {
            Err(ProviderHostError("no source networking needed".into()))
        }
        fn storage_get(&self, key: &str) -> Option<String> {
            self.state.storage_get(key)
        }
        fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
            self.state.storage_set(key, value)
        }
        fn log(&self, _message: &str) {}
        fn metadata_available(&self, _caller: &str) -> bool {
            true
        }
        fn metadata_enrich(
            &self,
            request: EnrichmentRequest,
            _caller: &str,
            _budget: Duration,
        ) -> EnrichmentResult {
            run(request, &self.addons, self.transport.clone())
        }
    }
    #[test]
    fn second_js_provider_uses_generic_rust_enrichment_with_permissions() {
        let (addons, transport) = fixture();
        let host = Arc::new(JsFixtureHost {
            state: MemoryProviderHost::new(),
            addons,
            transport,
        });
        let mut manifest:PluginManifest=serde_json::from_value(json!({"formatVersion":1,"id":"fixture","name":"Second provider","version":"1","entry":"index.js","permissions":{"addonMetadata":true}})).unwrap();
        let source = "globalThis.novaProvider = {handle(request) { return JSON.parse(nova.metadata.enrich(JSON.stringify(request))); }};";
        let request = serde_json::to_value(request()).unwrap();
        let result = PluginRuntime::new(manifest.clone(), source)
            .invoke(host.clone(), &request)
            .unwrap();
        assert_eq!(
            result["episodeMetadata"]["fixture:ep:1"]["ids"],
            json!(["tt100:1:1"])
        );
        manifest.permissions.addon_metadata = false;
        let denied = PluginRuntime::new(manifest, source)
            .invoke(host, &request)
            .unwrap();
        assert!(denied["error"].as_str().unwrap().contains("permission"));
    }

    #[test]
    fn operation_bounds_requests_response_sizes_and_duplicate_fetches() {
        let transport = Arc::new(FixtureTransport {
            responses: BTreeMap::from([
                ("https://example/one".into(), json!("ok")),
                (
                    "https://example/large".into(),
                    json!("x".repeat(RESPONSE_LIMIT + 1)),
                ),
            ]),
            calls: Mutex::new(vec![]),
            check_nested: false,
        });
        let mut operation = Operation {
            start: Instant::now(),
            calls: 0,
            detail_calls: 0,
            budget: Duration::from_secs(5),
            visited: BTreeSet::new(),
            transport: transport.clone(),
        };
        assert!(operation.fetch("https://example/one").is_some());
        assert!(operation.fetch("https://example/one").is_none());
        assert!(operation.fetch("https://example/large").is_none());
        operation.calls = 32;
        assert!(operation.fetch("https://example/other").is_none());
        operation.calls = 0;
        operation.start = Instant::now() - Duration::from_secs(6);
        assert!(operation.fetch("https://example/other").is_none());
        assert_eq!(transport.calls.lock().unwrap().len(), 2);
    }
}

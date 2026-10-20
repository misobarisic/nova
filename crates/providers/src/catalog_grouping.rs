//! Pure, reversible addon catalog projection. Group keys are display identities
//! only; every action must resolve to an original member and its exact owner.
mod cache;
pub use cache::{cached_evidence, remember_evidence, set_catalog_grouping_cache};

use addons::{MetaItem, MetaPreview};
use nova_config::CatalogGroupingMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Native IDs are never rewritten to the grouping parent, even for IMDb aliases.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIdentity {
    pub addon_url: String,
    pub media_type: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "namespace", content = "value", rename_all = "snake_case")]
pub enum ParentIdentity {
    ImdbSeries(String),
    TmdbTv(u32),
    TvdbSeries(u32),
}

impl ParentIdentity {
    /// Grouping has its own typed TVDB adapter; tracker contracts stay unchanged.
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 128 {
            return None;
        }
        let value = value.trim();
        let imdb = value.strip_prefix("imdb:").unwrap_or(value);
        if let Some(digits) = imdb.strip_prefix("tt") {
            return (!digits.is_empty()
                && digits.len() <= 12
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.bytes().any(|b| b != b'0'))
            .then(|| Self::ImdbSeries(imdb.into()));
        }
        let (namespace, id) = value.rsplit_once(':')?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let id = id.parse::<u32>().ok().filter(|id| *id > 0)?;
        match namespace {
            "tmdb:tv" => Some(Self::TmdbTv(id)),
            "tvdb" | "tvdb:series" => Some(Self::TvdbSeries(id)),
            _ => None,
        }
    }

    fn namespace(&self) -> u8 {
        match self {
            Self::ImdbSeries(_) => 0,
            Self::TmdbTv(_) => 1,
            Self::TvdbSeries(_) => 2,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::ImdbSeries(id) => Self::parse(id).as_ref() == Some(self),
            Self::TmdbTv(id) | Self::TvdbSeries(id) => *id > 0,
        }
    }
}

/// Unknown season is deliberately different from season 1. Multiple needs
/// complete canonical episode partitions before Season can split it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeasonMembership {
    Unknown,
    Season(u32),
    Multiple,
}

/// A canonical mapping is display/navigation evidence only. The source episode
/// ID remains the ID supplied by the original detail response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalEpisodeMapping {
    pub source_episode_id: String,
    pub parent: ParentIdentity,
    pub season: u32,
    pub episode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedSeasonPartition {
    pub canonical_season: u32,
    pub episodes: Vec<CanonicalEpisodeMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeasonSelection {
    pub source: SourceIdentity,
    pub canonical_season: u32,
    pub source_episode_ids: Vec<String>,
    pub source_inventory_fingerprint: String,
    pub evidence_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupingProof {
    /// Explicit, versioned parent relation, not a generic external-ID alias.
    ExplicitParent { provider_id: String },
    /// Canonical episode IDs proved the parent, not title/season text.
    CanonicalEpisodes {
        provider_id: String,
        episode_ids: Vec<String>,
    },
    /// The broker validated native-to-canonical episode alignment.
    BrokerAlignment {
        provider_id: String,
        episode_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingEvidence {
    pub source: SourceIdentity,
    pub parents: Vec<ParentIdentity>,
    pub season: SeasonMembership,
    pub proofs: Vec<GroupingProof>,
    /// Complete source coverage is required before emitting any partitions.
    #[serde(default)]
    pub season_partitions: Vec<ConfirmedSeasonPartition>,
    /// Binds a complete partition to the native episode inventory that proved it.
    #[serde(default)]
    pub source_inventory_fingerprint: Option<String>,
}

impl GroupingEvidence {
    pub fn verified(&self) -> bool {
        matches!(self.source.media_type.as_str(), "series" | "anime")
            && !self.source.id.is_empty()
            && !self.source.addon_url.is_empty()
            && !self.parents.is_empty()
            && !self.proofs.is_empty()
            && self.parents.iter().all(ParentIdentity::valid)
            && !has_conflict(&self.parents)
            && !matches!(self.season, SeasonMembership::Season(0))
            && self.valid_partitions()
    }

    pub fn fingerprint(&self) -> String {
        digest(self)
    }

    fn valid_partitions(&self) -> bool {
        if self.season_partitions.is_empty() {
            return true;
        }
        if self
            .source_inventory_fingerprint
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return false;
        }
        let mut seen_seasons = BTreeSet::new();
        let mut seen_episodes = BTreeSet::new();
        let mut seen_canonical = BTreeSet::new();
        if self.season_partitions.iter().any(|partition| {
            partition.canonical_season == 0
                || !seen_seasons.insert(partition.canonical_season)
                || partition.episodes.is_empty()
                || partition.episodes.iter().any(|mapping| {
                    mapping.source_episode_id.is_empty()
                        || mapping.season != partition.canonical_season
                        || mapping.episode == 0
                        || !self.parents.contains(&mapping.parent)
                        || !seen_episodes.insert(mapping.source_episode_id.clone())
                        || !seen_canonical.insert((
                            mapping.parent.clone(),
                            mapping.season,
                            mapping.episode,
                        ))
                })
        }) {
            return false;
        }
        match self.season {
            SeasonMembership::Unknown => false,
            SeasonMembership::Season(season) => {
                seen_seasons.len() == 1 && seen_seasons.contains(&season)
            }
            SeasonMembership::Multiple => seen_seasons.len() > 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OwnedCatalogEntry {
    pub source: SourceIdentity,
    pub preview: MetaPreview,
    pub evidence: Option<GroupingEvidence>,
}

impl OwnedCatalogEntry {
    pub fn new(addon_url: &str, preview: MetaPreview) -> Self {
        let source = SourceIdentity {
            addon_url: addon_url.into(),
            media_type: preview.type_.clone(),
            id: preview.id.clone(),
        };
        let evidence = evidence_from_preview(&source, &preview, addon_url);
        Self {
            source,
            preview,
            evidence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayGroupAction {
    OpenSource,
    MemberChooser,
}

#[derive(Clone, Debug)]
pub struct DisplayGroup {
    /// Never pass this key to storage, playback, bookmarks or addon endpoints.
    pub key: String,
    pub representative: OwnedCatalogEntry,
    pub members: Vec<OwnedCatalogEntry>,
    pub original_index: usize,
    pub proven_mapping: Vec<GroupingEvidence>,
    pub action: DisplayGroupAction,
    /// A virtual Season card retains the source ID and carries this navigation context.
    pub canonical_season: Option<u32>,
}

impl DisplayGroup {
    pub fn season_selection(&self, source: &SourceIdentity) -> Option<SeasonSelection> {
        let season = self.canonical_season?;
        let member = self
            .members
            .iter()
            .find(|member| &member.source == source)?;
        let partition = eligible(member)?
            .season_partitions
            .iter()
            .find(|partition| partition.canonical_season == season)?;
        Some(SeasonSelection {
            source: source.clone(),
            source_inventory_fingerprint: eligible(member)?.source_inventory_fingerprint.clone()?,
            evidence_fingerprint: eligible(member)?.fingerprint(),
            canonical_season: season,
            source_episode_ids: partition
                .episodes
                .iter()
                .map(|mapping| mapping.source_episode_id.clone())
                .collect(),
        })
    }
}

/// Stable native inventory identity; artwork and display text are deliberately
/// excluded. Ordering, additions, removals and native episode IDs all matter.
pub fn episode_inventory_fingerprint<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    digest(&(1_u32, ids.into_iter().collect::<Vec<_>>()))
}

fn digest(value: &impl Serialize) -> String {
    let raw = serde_json::to_vec(value).unwrap_or_default();
    let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &raw);
    hash.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn has_conflict(parents: &[ParentIdentity]) -> bool {
    let mut namespaces = BTreeMap::new();
    parents.iter().any(|parent| {
        namespaces
            .insert(parent.namespace(), parent)
            .is_some_and(|old| old != parent)
    })
}

fn eligible(entry: &OwnedCatalogEntry) -> Option<&GroupingEvidence> {
    entry.evidence.as_ref().filter(|evidence| {
        evidence.source == entry.source
            && entry.preview.id == entry.source.id
            && entry.preview.type_ == entry.source.media_type
            && evidence.verified()
    })
}

/// Projection never fetches or mutates raw entries. Earliest source order and
/// artwork win. A conflict anywhere in a transitive alias component keeps all
/// of its cards separate rather than accepting whichever alias arrived first.
pub fn project_catalog(
    entries: &[OwnedCatalogEntry],
    mode: CatalogGroupingMode,
) -> Vec<DisplayGroup> {
    let mut components: Vec<Vec<usize>> = Vec::new();
    if mode != CatalogGroupingMode::Original {
        for (index, entry) in entries.iter().enumerate() {
            let Some(evidence) = eligible(entry) else {
                continue;
            };
            let mut overlaps = Vec::new();
            for (component_index, component) in components.iter().enumerate() {
                if component.iter().any(|member| {
                    let other = &entries[*member];
                    // Different installed sources retain their own catalogs and
                    // policy boundaries, even when their public labels match.
                    other.source.addon_url == entry.source.addon_url
                        && eligible(other).is_some_and(|other| {
                            other.parents.iter().any(|id| evidence.parents.contains(id))
                        })
                }) {
                    overlaps.push(component_index);
                }
            }
            let mut component = vec![index];
            for old in overlaps.into_iter().rev() {
                component.extend(components.remove(old));
            }
            components.push(component);
        }
    }
    let mut buckets: Vec<(usize, Option<u32>, Vec<usize>)> = Vec::new();
    let mut assigned = BTreeSet::new();
    for mut component in components {
        component.sort_unstable();
        let parents: Vec<_> = component
            .iter()
            .filter_map(|i| eligible(&entries[*i]))
            .flat_map(|e| e.parents.clone())
            .collect();
        if has_conflict(&parents) {
            continue;
        }
        match mode {
            CatalogGroupingMode::Original => {}
            CatalogGroupingMode::Series => {
                buckets.push((component[0], None, component.clone()));
                assigned.extend(component);
            }
            CatalogGroupingMode::Season => {
                let mut seasons: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
                for index in component {
                    let Some(evidence) = eligible(&entries[index]) else {
                        continue;
                    };
                    match evidence.season {
                        SeasonMembership::Season(season) => {
                            seasons.entry(season).or_default().push(index);
                        }
                        SeasonMembership::Multiple => {
                            // Partitions exist only when every native episode
                            // has an unambiguous canonical mapping. Native
                            // relative season fields can never create them.
                            for partition in &evidence.season_partitions {
                                seasons
                                    .entry(partition.canonical_season)
                                    .or_default()
                                    .push(index);
                            }
                        }
                        SeasonMembership::Unknown => {}
                    }
                }
                for (season, members) in seasons {
                    buckets.push((members[0], Some(season), members.clone()));
                    assigned.extend(members);
                }
            }
        }
    }
    for index in 0..entries.len() {
        if !assigned.contains(&index) {
            buckets.push((index, None, vec![index]));
        }
    }
    buckets.sort_by_key(|(index, season, _)| (*index, *season));
    buckets
        .into_iter()
        .map(|(index, canonical_season, indices)| {
            let members: Vec<_> = indices.into_iter().map(|i| entries[i].clone()).collect();
            let proven_mapping = members
                .iter()
                .filter_map(|entry| eligible(entry).cloned())
                .collect::<Vec<_>>();
            let key = if members.len() > 1 || canonical_season.is_some() {
                let parents: BTreeSet<_> = proven_mapping
                    .iter()
                    .flat_map(|e| e.parents.iter())
                    .collect();
                format!(
                    "catalog-group:v1:{}",
                    digest(&(
                        mode,
                        &entries[index].source.addon_url,
                        parents.first(),
                        canonical_season
                    ))
                )
            } else {
                format!("catalog-source:v1:{}", digest(&entries[index].source))
            };
            DisplayGroup {
                key,
                representative: entries[index].clone(),
                original_index: index,
                action: if members.len() > 1 {
                    DisplayGroupAction::MemberChooser
                } else {
                    DisplayGroupAction::OpenSource
                },
                members,
                proven_mapping,
                canonical_season,
            }
        })
        .collect()
}

/// Version 1 compatible-addon extension. `parentIds` is an explicit whole-series
/// relation; ordinary imdbId/tmdbId/MAL aliases alone never establish a parent.
/// Specials and non-series content are intentionally unsupported.
pub fn evidence_from_preview(
    source: &SourceIdentity,
    preview: &MetaPreview,
    provider_url: &str,
) -> Option<GroupingEvidence> {
    if source.id != preview.id
        || source.media_type != preview.type_
        || !matches!(source.media_type.as_str(), "series" | "anime")
    {
        return None;
    }
    let value = preview.extra.get("novaCatalogGrouping")?;
    if value.get("version")?.as_u64()? != 1 || value.get("kind")?.as_str()? != "series_member" {
        return None;
    }
    let values = value.get("parentIds")?.as_array()?;
    if values.is_empty() || values.len() > 8 {
        return None;
    }
    let mut parents = values
        .iter()
        .map(|id| ParentIdentity::parse(id.as_str()?))
        .collect::<Option<Vec<_>>>()?;
    parents.sort();
    parents.dedup();
    let season = match value.get("season") {
        None | Some(serde_json::Value::Null) => SeasonMembership::Unknown,
        Some(value) => {
            SeasonMembership::Season(u32::try_from(value.as_u64()?).ok().filter(|n| *n > 0)?)
        }
    };
    let evidence = GroupingEvidence {
        source: source.clone(),
        parents,
        season,
        season_partitions: vec![],
        source_inventory_fingerprint: None,
        proofs: vec![GroupingProof::ExplicitParent {
            provider_id: crate::addon_metadata_id(provider_url),
        }],
    };
    evidence.verified().then_some(evidence)
}

/// Canonical episode IDs bind parent and numbering together. Relative `season`
/// fields on native MAL/Kitsu/etc. episodes do not carry this proof.
pub fn evidence_from_detail(
    source: &SourceIdentity,
    detail: &MetaItem,
    provider_url: &str,
) -> Option<GroupingEvidence> {
    if detail.preview.id != source.id
        || detail.preview.type_ != source.media_type
        || !matches!(source.media_type.as_str(), "series" | "anime")
    {
        return None;
    }
    let explicit = evidence_from_preview(source, &detail.preview, provider_url);
    if detail.videos.is_empty() {
        return explicit;
    }
    let mut parent = None;
    let mut seasons = BTreeSet::new();
    let mut ids = Vec::new();
    let mut partitions: BTreeMap<u32, Vec<CanonicalEpisodeMapping>> = BTreeMap::new();
    let mut complete = true;
    for video in &detail.videos {
        if video.season == Some(0) {
            return None;
        }
        let Some((p, season, episode)) = video_canonical_episode(video) else {
            if video.extra.contains_key("imdbSeason") || video.extra.contains_key("imdbEpisode") {
                return None;
            }
            complete = false;
            continue;
        };
        // A special anywhere makes the relationship unsafe for this projection.
        // Native season/episode fields may retain relative anime numbering.
        // Canonical IDs (and optional explicit canonical fields) carry the
        // mapped numbering; ordinary native fields are never used as proof.
        if video
            .extra
            .get("imdbSeason")
            .is_some_and(|s| s.as_u64() != Some(u64::from(season)))
            || video
                .extra
                .get("imdbEpisode")
                .is_some_and(|e| e.as_u64() != Some(u64::from(episode)))
            || parent.as_ref().is_some_and(|old| old != &p)
        {
            return None;
        }
        partitions
            .entry(season)
            .or_default()
            .push(CanonicalEpisodeMapping {
                source_episode_id: video.id.clone(),
                parent: p.clone(),
                season,
                episode,
            });
        parent = Some(p);
        seasons.insert(season);
        if ids.len() < 128 {
            ids.push(video.id.clone());
        }
    }
    if let Some(explicit) = &explicit {
        let combined: Vec<_> = explicit
            .parents
            .iter()
            .cloned()
            .chain(parent.iter().cloned())
            .collect();
        if has_conflict(&combined)
            || matches!(explicit.season, SeasonMembership::Season(season) if seasons.iter().any(|mapped| *mapped != season))
        {
            return None;
        }
    }
    if !complete {
        return explicit;
    }
    let mut evidence = GroupingEvidence {
        source: source.clone(),
        parents: vec![parent?],
        source_inventory_fingerprint: Some(episode_inventory_fingerprint(
            detail.videos.iter().map(|video| video.id.as_str()),
        )),
        season_partitions: partitions
            .into_iter()
            .map(|(canonical_season, episodes)| ConfirmedSeasonPartition {
                canonical_season,
                episodes,
            })
            .collect(),
        season: if seasons.len() == 1 {
            SeasonMembership::Season(*seasons.first()?)
        } else {
            SeasonMembership::Multiple
        },
        proofs: vec![GroupingProof::CanonicalEpisodes {
            provider_id: crate::addon_metadata_id(provider_url),
            episode_ids: ids,
        }],
    };
    if let Some(explicit) = explicit {
        evidence.parents.extend(explicit.parents);
        evidence.parents.sort();
        evidence.parents.dedup();
        evidence.proofs.extend(explicit.proofs);
    }
    evidence.verified().then_some(evidence)
}

/// Normalize only explicit requested-ID aliases using the shared Stremio guard,
/// then extract parent proof. Merely returning a different canonical title ID
/// never licenses replacing the native catalog identity.
pub fn evidence_from_response(
    source: &SourceIdentity,
    bytes: &[u8],
    provider_url: &str,
) -> Option<GroupingEvidence> {
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    let addon = addons::Addon::new(provider_url).ok()?;
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if let Some(meta) = value.get_mut("meta") {
        crate::stremio::normalize_detail_request_identity(
            &addon.meta_url(&source.media_type, &source.id),
            meta,
        );
    }
    let detail = addons::Addon::parse_meta(&serde_json::to_vec(&value).ok()?).ok()??;
    evidence_from_detail(source, &detail, provider_url)
}

/// Explicit canonical episode triples are an ordinary compatible-addon field
/// contract (including AIOMetadata's imdb_id/imdbSeason/imdbEpisode output).
/// They describe mapped episodes, unlike whole-title external-ID aliases.
fn video_canonical_episode(video: &addons::Video) -> Option<(ParentIdentity, u32, u32)> {
    let canonical = canonical_episode(&video.id);
    let mapped =
        if video.extra.contains_key("imdbSeason") || video.extra.contains_key("imdbEpisode") {
            let parent = video
                .extra
                .get("imdb_id")
                .or_else(|| video.extra.get("imdbId"))?
                .as_str()?;
            let parent = ParentIdentity::parse(parent)?;
            if !matches!(parent, ParentIdentity::ImdbSeries(_)) {
                return None;
            }
            let season = u32::try_from(video.extra.get("imdbSeason")?.as_u64()?)
                .ok()
                .filter(|n| *n > 0)?;
            let episode = u32::try_from(video.extra.get("imdbEpisode")?.as_u64()?)
                .ok()
                .filter(|n| *n > 0)?;
            Some((parent, season, episode))
        } else {
            None
        };
    match (canonical, mapped) {
        (Some(canonical), Some(mapped)) if canonical != mapped => None,
        (Some(canonical), _) => Some(canonical),
        (_, mapped) => mapped,
    }
}

pub(crate) fn canonical_episode(id: &str) -> Option<(ParentIdentity, u32, u32)> {
    let (parent_season, episode) = id.rsplit_once(':')?;
    let (parent, season) = parent_season.rsplit_once(':')?;
    let season = season.parse::<u32>().ok().filter(|n| *n > 0)?;
    let episode = episode.parse::<u32>().ok().filter(|n| *n > 0)?;
    Some((ParentIdentity::parse(parent)?, season, episode))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupingLookupStatus {
    Original,
    Confirmed,
    Unconfirmed,
    Deferred,
    Stale,
}

#[derive(Clone, Debug)]
pub struct GroupingLookup {
    pub evidence: Option<GroupingEvidence>,
    pub status: GroupingLookupStatus,
}

/// Bounded identity-only metadata lookup. No title search, source discovery or
/// full enrichment is performed. The caller schedules visible cards and rejects
/// completions for obsolete profile/catalog/mode epochs before publication.
pub fn resolve_catalog_grouping_evidence(
    source: &SourceIdentity,
    mode: CatalogGroupingMode,
    revision: u64,
) -> GroupingLookup {
    crate::metadata::resolve_catalog_grouping_evidence(source, mode, revision)
}

/// Consume only broker-validated canonical episode connections. A generic
/// confirmed header/title match is insufficient evidence of parenthood.
pub fn evidence_from_broker(
    source: &SourceIdentity,
    result: &crate::EnrichmentResult,
) -> Option<GroupingEvidence> {
    if result.status != "confirmed"
        || result.details.media.source_id != source.id
        || result.details.media.media_type != source.media_type
        || !matches!(source.media_type.as_str(), "series" | "anime")
    {
        return None;
    }
    let mut parents = BTreeSet::new();
    let mut seasons = BTreeSet::new();
    let mut proofs = Vec::new();
    let mut covered = 0;
    let mut partitions: BTreeMap<u32, Vec<CanonicalEpisodeMapping>> = BTreeMap::new();
    for episode in &result.details.episodes {
        let Some(mapping) = result.episode_metadata.get(&episode.stable_id()) else {
            continue;
        };
        let mut episode_proven = false;
        let mut episode_mapping = None;
        for (id, connection) in mapping.ids.iter().zip(&mapping.connections) {
            if connection.basis == "conflicting-identifiers"
                || !result.connections.iter().any(|c| {
                    c.addon_id == connection.addon_id
                        && c.media_id == connection.media_id
                        && c.basis != "conflicting-identifiers"
                })
            {
                continue;
            }
            let Some((parent, season, number)) = canonical_episode(id) else {
                continue;
            };
            // The mapping must point inside this connection's own TV parent.
            if ParentIdentity::parse(&connection.media_id).as_ref() != Some(&parent)
                || mapping.season != Some(season)
                || mapping.episode != Some(number)
            {
                continue;
            }
            let mapped = CanonicalEpisodeMapping {
                source_episode_id: episode.source_id.clone(),
                parent: parent.clone(),
                season,
                episode: number,
            };
            if episode_mapping.as_ref().is_some_and(|old| old != &mapped) {
                return None;
            }
            episode_mapping = Some(mapped);
            parents.insert(parent);
            seasons.insert(season);
            episode_proven = true;
            if proofs.len() < 128 {
                proofs.push(GroupingProof::BrokerAlignment {
                    provider_id: connection.addon_id.clone(),
                    episode_ids: vec![id.clone()],
                });
            }
        }
        if episode_proven {
            covered += 1;
            if let Some(mapping) = episode_mapping {
                partitions.entry(mapping.season).or_default().push(mapping);
            }
        }
    }
    if covered == 0 {
        return None;
    }
    let parents: Vec<_> = parents.into_iter().collect();
    let evidence = GroupingEvidence {
        source: source.clone(),
        parents,
        source_inventory_fingerprint: (covered == result.details.episodes.len()).then(|| {
            episode_inventory_fingerprint(
                result
                    .details
                    .episodes
                    .iter()
                    .map(|episode| episode.source_id.as_str()),
            )
        }),
        season_partitions: if covered == result.details.episodes.len() {
            partitions
                .into_iter()
                .map(|(canonical_season, episodes)| ConfirmedSeasonPartition {
                    canonical_season,
                    episodes,
                })
                .collect()
        } else {
            vec![]
        },
        // Partial episode coverage confirms a parent, never the season extent.
        season: if covered != result.details.episodes.len() {
            SeasonMembership::Unknown
        } else if seasons.len() == 1 {
            SeasonMembership::Season(*seasons.first()?)
        } else {
            SeasonMembership::Multiple
        },
        proofs,
    };
    evidence.verified().then_some(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_config::CatalogGroupingMode::{Original, Season, Series};
    use serde_json::json;

    fn entry(id: &str, parents: &[&str], season: Option<u32>) -> OwnedCatalogEntry {
        let mut value = json!({"id":id,"type":"series","name":id,"poster":format!("https://images.example/{id}"),"novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":parents}});
        if let Some(season) = season {
            value["novaCatalogGrouping"]["season"] = json!(season);
        }
        OwnedCatalogEntry::new(
            "https://owner.example/key/manifest.json",
            serde_json::from_value(value).unwrap(),
        )
    }

    #[test]
    fn real_compatible_episode_contract_collapses_and_restores_native_anime_sources() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../test-fixtures/catalog-grouping/aiometadata-imdb-anime.json"
        ))
        .unwrap();
        let catalog = serde_json::to_vec(&fixture["catalog"]).unwrap();
        let raw = addons::Addon::parse_catalog(&catalog).unwrap();
        let mut entries = raw
            .into_iter()
            .map(|preview| {
                OwnedCatalogEntry::new("https://metadata.example/manifest.json", preview)
            })
            .collect::<Vec<_>>();
        assert_eq!(project_catalog(&entries, Series).len(), 2);
        for (entry, value) in entries
            .iter_mut()
            .zip(fixture["details"].as_array().unwrap())
        {
            let detail = addons::Addon::parse_meta(&serde_json::to_vec(value).unwrap())
                .unwrap()
                .unwrap();
            entry.evidence = evidence_from_detail(&entry.source, &detail, &entry.source.addon_url);
        }
        assert_eq!(project_catalog(&entries, Season).len(), 2);
        let grouped = project_catalog(&entries, Series);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].action, DisplayGroupAction::MemberChooser);
        assert_eq!(
            grouped[0].representative.preview.poster,
            entries[0].preview.poster
        );
        assert_eq!(
            grouped[0]
                .members
                .iter()
                .map(|e| e.source.id.as_str())
                .collect::<Vec<_>>(),
            ["kitsu:111", "mal:222"]
        );
        assert!(
            grouped[0]
                .members
                .iter()
                .all(|e| e.source.media_type == "anime")
        );
        let original = project_catalog(&entries, Original);
        assert_eq!(original.len(), 2);
        assert!(
            original
                .iter()
                .all(|g| g.action == DisplayGroupAction::OpenSource)
        );
        assert_eq!(original[1].representative.preview.id, "mal:222");
    }

    #[test]
    fn proven_aliases_combine_cours_only_inside_the_same_canonical_season() {
        let entries = [
            entry("mal:1", &["imdb:tt1", "tmdb:tv:8"], Some(1)),
            entry("anilist:2", &["tmdb:tv:8", "tvdb:9"], Some(1)),
            entry("kitsu:3", &["tvdb:9"], Some(2)),
        ];
        let seasons = project_catalog(&entries, Season);
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].members.len(), 2);
        assert_eq!(project_catalog(&entries, Series).len(), 1);
        assert_eq!(project_catalog(&entries, Original).len(), 3);
    }

    #[test]
    fn parent_without_season_groups_only_in_series_and_multiseason_is_never_one_season() {
        let mut entries = [
            entry("mal:1", &["tt1"], None),
            entry("mal:2", &["tt1"], None),
        ];
        assert_eq!(project_catalog(&entries, Season).len(), 2);
        assert_eq!(project_catalog(&entries, Series).len(), 1);
        entries[0].evidence.as_mut().unwrap().season = SeasonMembership::Multiple;
        entries[1].evidence.as_mut().unwrap().season = SeasonMembership::Season(1);
        assert_eq!(project_catalog(&entries, Season).len(), 2);
        assert_eq!(project_catalog(&entries, Series).len(), 1);
    }

    #[test]
    fn conflicting_transitive_aliases_remakes_spinoffs_and_opaque_owners_stay_separate() {
        let entries = [
            entry("mal:1", &["tt1"], Some(1)),
            entry("mal:2", &["tt1", "tmdb:tv:8"], Some(1)),
            entry("mal:3", &["tt2", "tmdb:tv:8"], Some(1)),
        ];
        assert_eq!(project_catalog(&entries, Series).len(), 3);
        let mut different_owner = entry("mal:1", &["tt1"], Some(1));
        different_owner.source.addon_url = "https://other.example/secret".into();
        different_owner.evidence.as_mut().unwrap().source = different_owner.source.clone();
        assert_eq!(
            project_catalog(&[entries[0].clone(), different_owner], Series).len(),
            2
        );
        assert_eq!(
            project_catalog(
                &[
                    entry("opaque:one", &["tt1"], Some(1)),
                    entry("opaque:two", &["tt2"], Some(1))
                ],
                Series
            )
            .len(),
            2
        );
    }

    #[test]
    fn arbitrary_aliases_missing_metadata_movies_and_specials_never_supply_proof() {
        for value in [
            json!({"id":"mal:1","type":"series","name":"Series Season 2","imdb_id":"tt1","season":2}),
            json!({"id":"mal:2","type":"movie","novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tt1"],"season":1}}),
            json!({"id":"mal:3","type":"series","novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tt1"],"season":0}}),
        ] {
            let e = OwnedCatalogEntry::new(
                "https://owner.example",
                serde_json::from_value(value).unwrap(),
            );
            assert!(e.evidence.is_none());
            assert_eq!(project_catalog(&[e], Series).len(), 1);
        }
        for value in [
            json!({"meta":{"id":"mal:1","type":"series","videos":[{"id":"mal:1:1","season":1,"episode":1}]}}),
            json!({"meta":{"id":"mal:1","type":"series","videos":[{"id":"tt1:0:1","season":0,"episode":1}]}}),
            json!({"meta":{"id":"mal:1","type":"series","videos":[{"id":"tt1:1:1"},{"id":"tt2:1:2"}]}}),
        ] {
            let d = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .unwrap();
            let source = SourceIdentity {
                addon_url: "https://owner.example".into(),
                media_type: "series".into(),
                id: "mal:1".into(),
            };
            assert!(evidence_from_detail(&source, &d, &source.addon_url).is_none());
        }
    }

    #[test]
    fn canonical_episode_mapping_can_preserve_native_episode_ids_and_reject_conflicts() {
        let value = json!({"meta":{"id":"mal:1","type":"series","videos":[{"id":"mal:1:1","season":1,"episode":1,"imdb_id":"tt1","imdbSeason":2,"imdbEpisode":14}]}});
        let mut d = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "series".into(),
            id: "mal:1".into(),
        };
        assert_eq!(
            evidence_from_detail(&source, &d, &source.addon_url)
                .unwrap()
                .season,
            SeasonMembership::Season(2)
        );
        assert_eq!(d.videos[0].id, "mal:1:1");
        d.videos[0].id = "tt2:2:14".into();
        assert!(evidence_from_detail(&source, &d, &source.addon_url).is_none());
    }

    #[test]
    fn season_mode_splits_a_complete_multiseason_source_and_reverses_without_changing_ids() {
        let value = json!({"meta":{"id":"kitsu:77","type":"anime","videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1},
            {"id":"kitsu:77:3","season":1,"episode":3,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":2}
        ]}});
        let detail = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        let mut source = OwnedCatalogEntry::new("https://owner.example", detail.preview.clone());
        source.evidence = evidence_from_detail(&source.source, &detail, &source.source.addon_url);
        assert_eq!(
            source.evidence.as_ref().unwrap().season,
            SeasonMembership::Multiple
        );
        let seasons = project_catalog(&[source.clone()], Season);
        assert_eq!(seasons.len(), 2);
        assert_ne!(seasons[0].key, seasons[1].key);
        assert_eq!(seasons[0].canonical_season, Some(1));
        assert_eq!(seasons[1].canonical_season, Some(2));
        assert!(
            seasons
                .iter()
                .all(|group| group.representative.preview.id == "kitsu:77"
                    && group.members[0].source.id == "kitsu:77")
        );
        assert_eq!(
            seasons[1]
                .season_selection(&source.source)
                .unwrap()
                .source_episode_ids,
            ["kitsu:77:2", "kitsu:77:3"]
        );
        assert_eq!(project_catalog(&[source.clone()], Original).len(), 1);
        assert_eq!(project_catalog(&[source.clone()], Series).len(), 1);
        // One unmapped native episode makes the extent unproven: no virtual split.
        let mut partial = detail;
        partial.videos.push(addons::Video {
            id: "kitsu:77:4".into(),
            season: Some(1),
            episode: Some(4),
            ..Default::default()
        });
        source.evidence = evidence_from_detail(&source.source, &partial, &source.source.addon_url);
        assert!(source.evidence.is_none());
        assert_eq!(project_catalog(&[source], Season).len(), 1);
    }

    #[test]
    fn canonical_response_alias_is_validated_before_parent_evidence_is_accepted() {
        let source = SourceIdentity {
            addon_url: "https://catalog.example/key".into(),
            media_type: "series".into(),
            id: "kitsu:77".into(),
        };
        let mut response = json!({"meta":{"id":"tt7","type":"series","_kitsuId":"77","videos":[{"id":"tt7:2:1","season":1,"episode":1}]}});
        let evidence = evidence_from_response(
            &source,
            &serde_json::to_vec(&response).unwrap(),
            "https://metadata.example/manifest.json",
        )
        .unwrap();
        assert_eq!(evidence.source.id, "kitsu:77");
        assert_eq!(evidence.season, SeasonMembership::Season(2));
        response["meta"]["_kitsuId"] = json!("78");
        assert!(
            evidence_from_response(
                &source,
                &serde_json::to_vec(&response).unwrap(),
                "https://metadata.example/manifest.json"
            )
            .is_none()
        );
        response["meta"]["_kitsuId"] = json!("77");
        response["meta"]["type"] = json!("movie");
        assert!(
            evidence_from_response(
                &source,
                &serde_json::to_vec(&response).unwrap(),
                "https://metadata.example/manifest.json"
            )
            .is_none()
        );
    }

    #[test]
    fn explicit_parent_accepts_richer_partitions_but_rejects_conflicting_season_or_duplicate_coordinates()
     {
        let owner = "https://owner.example";
        let mut value = json!({"meta":{"id":"kitsu:77","type":"anime","novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tt7"]},"videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}
        ]}});
        let make = |value: &serde_json::Value| {
            let detail = addons::Addon::parse_meta(&serde_json::to_vec(value).unwrap())
                .unwrap()
                .unwrap();
            let mut entry = OwnedCatalogEntry::new(owner, detail.preview.clone());
            entry.evidence = evidence_from_detail(&entry.source, &detail, owner);
            entry
        };
        let combined = make(&value);
        let evidence = combined.evidence.as_ref().unwrap();
        assert_eq!(evidence.proofs.len(), 2);
        assert_eq!(
            project_catalog(std::slice::from_ref(&combined), Season).len(),
            2
        );
        let expected = episode_inventory_fingerprint(["kitsu:77:1", "kitsu:77:2"]);
        assert_eq!(
            evidence.source_inventory_fingerprint.as_deref(),
            Some(expected.as_str())
        );
        assert_ne!(episode_inventory_fingerprint(["kitsu:77:1"]), expected);
        let standalone = make(
            &json!({"meta":{"id":"mal:88","type":"anime","videos":[{"id":"mal:88:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":2}]}}),
        );
        let grouped = project_catalog(&[combined.clone(), standalone.clone()], Season);
        let second = grouped
            .iter()
            .find(|group| group.canonical_season == Some(2))
            .unwrap();
        assert_eq!(second.action, DisplayGroupAction::MemberChooser);
        let whole_scope = second.season_selection(&combined.source).unwrap();
        let separate_scope = second.season_selection(&standalone.source).unwrap();
        assert_eq!(whole_scope.source.id, "kitsu:77");
        assert_eq!(whole_scope.source_episode_ids, ["kitsu:77:2"]);
        assert_eq!(separate_scope.source.id, "mal:88");
        assert_eq!(separate_scope.source_episode_ids, ["mal:88:1"]);
        assert_eq!(whole_scope.evidence_fingerprint, evidence.fingerprint());
        value["meta"]["novaCatalogGrouping"]["season"] = json!(1);
        assert!(make(&value).evidence.is_none());
        value["meta"]["novaCatalogGrouping"]
            .as_object_mut()
            .unwrap()
            .remove("season");
        value["meta"]["videos"][1]["imdbSeason"] = json!(1);
        assert!(make(&value).evidence.is_none());
    }

    #[test]
    fn original_never_starts_resolution_even_for_an_unavailable_inventory() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "series".into(),
            id: "mal:1".into(),
        };
        let lookup = resolve_catalog_grouping_evidence(&source, Original, u64::MAX);
        assert_eq!(lookup.status, GroupingLookupStatus::Original);
        assert!(lookup.evidence.is_none());
    }

    #[test]
    fn typed_parent_parser_does_not_repurpose_native_anime_or_movie_ids() {
        for id in [
            "mal:1",
            "anilist:2",
            "kitsu:3",
            "tmdb:movie:4",
            "tmdb:5",
            "5",
            "tvdb:0",
            "tt0",
            "tt1:2:3",
        ] {
            assert!(ParentIdentity::parse(id).is_none(), "{id}");
        }
        assert_eq!(
            ParentIdentity::parse("tmdb:tv:5"),
            Some(ParentIdentity::TmdbTv(5))
        );
        assert_eq!(
            ParentIdentity::parse("tvdb:series:5"),
            Some(ParentIdentity::TvdbSeries(5))
        );
    }
}

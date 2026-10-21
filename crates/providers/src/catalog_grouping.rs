//! Pure, reversible addon catalog projection. Group keys are display identities
//! only; every action must resolve to an original member and its exact owner.
mod cache;
pub(crate) use cache::remember_inferred_evidence;
pub use cache::{
    cached_evidence, cached_evidence_with_inference, remember_evidence, set_catalog_grouping_cache,
};

use addons::{MetaItem, MetaPreview};
use nova_config::CatalogGroupingMode;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
    pub mapping_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredParentPreview {
    pub source: SourceIdentity,
    pub name: String,
    pub poster: Option<String>,
    pub background: Option<String>,
    pub logo: Option<String>,
    pub release_info: Option<serde_json::Value>,
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
    /// Optional display-only inference. The source explicitly points to the
    /// preceding named season and its native episodes occur in this TV parent.
    /// This is never evidence that any source episode has canonical numbering.
    InferredSeasonChain {
        provider_id: String,
        related_release_id: String,
        source_season: u32,
        related_season: u32,
        parent_preview: InferredParentPreview,
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
    pub fn is_inferred(&self) -> bool {
        self.proofs
            .iter()
            .any(|proof| matches!(proof, GroupingProof::InferredSeasonChain { .. }))
    }

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
            && (!self.is_inferred() || self.valid_inference())
    }

    fn valid_inference(&self) -> bool {
        self.season == SeasonMembership::Unknown
            && self.season_partitions.is_empty()
            && self.proofs.len() == 1
            && self.parents.len() == 1
            && self
                .source_inventory_fingerprint
                .as_ref()
                .is_some_and(|value| {
                    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
                })
            && self.proofs.iter().all(|proof| match proof {
                GroupingProof::InferredSeasonChain {
                    provider_id,
                    related_release_id,
                    source_season,
                    related_season,
                    parent_preview,
                } => {
                    *provider_id == crate::addon_metadata_id(&self.source.addon_url)
                        && anime_release_id(related_release_id)
                        && *related_release_id != self.source.id
                        && *related_season > 0
                        && *source_season <= 100
                        && related_season.checked_add(1) == Some(*source_season)
                        && parent_preview.source.addon_url == self.source.addon_url
                        && parent_preview.source.media_type == "series"
                        && ParentIdentity::parse(&parent_preview.source.id)
                            .is_some_and(|parent| self.parents.contains(&parent))
                        && !parent_preview.name.is_empty()
                        && parent_preview.name.len() <= 256
                        && !parent_preview.name.chars().any(char::is_control)
                        && [
                            &parent_preview.poster,
                            &parent_preview.background,
                            &parent_preview.logo,
                        ]
                        .iter()
                        .all(|value| {
                            value
                                .as_ref()
                                .is_none_or(|value| bounded_art(Some(value.clone())).is_some())
                        })
                        && parent_preview.release_info.as_ref().is_none_or(|value| {
                            value.as_str().is_some_and(|value| value.len() <= 64)
                        })
                }
                _ => false,
            })
    }

    pub fn fingerprint(&self) -> String {
        // Provenance and partition membership are sets. Catalog/detail/cache
        // paths may encounter identical claims in a different order; that must
        // not invalidate an otherwise identical source-owned Detail opening.
        // Native episode order remains bound by source_inventory_fingerprint.
        let mut canonical = self.clone();
        canonical.parents.sort();
        canonical.parents.dedup();
        for proof in &mut canonical.proofs {
            match proof {
                GroupingProof::ExplicitParent { .. }
                | GroupingProof::InferredSeasonChain { .. } => {}
                GroupingProof::CanonicalEpisodes { episode_ids, .. }
                | GroupingProof::BrokerAlignment { episode_ids, .. } => {
                    episode_ids.sort();
                    episode_ids.dedup();
                }
            }
        }
        canonical
            .proofs
            .sort_by_cached_key(|proof| serde_json::to_string(proof).unwrap_or_default());
        canonical.proofs.dedup();
        for partition in &mut canonical.season_partitions {
            partition.episodes.sort_by(|first, second| {
                first
                    .parent
                    .cmp(&second.parent)
                    .then(first.season.cmp(&second.season))
                    .then(first.episode.cmp(&second.episode))
                    .then(first.source_episode_id.cmp(&second.source_episode_id))
            });
            partition.episodes.dedup();
        }
        canonical
            .season_partitions
            .sort_by_key(|partition| partition.canonical_season);
        digest(&(2_u32, canonical))
    }

    /// Semantic navigation identity, independent of which providers supplied
    /// corroborating proof. Each canonical episode mapping already binds its
    /// actual parent; redundant whole-title aliases must not invalidate the
    /// same native season when a fresh owner response omits those aliases.
    pub fn mapping_fingerprint(&self) -> String {
        let mut partitions = self
            .season_partitions
            .iter()
            .map(|partition| {
                let mut episodes = partition
                    .episodes
                    .iter()
                    .map(|mapping| {
                        (
                            mapping.source_episode_id.as_str(),
                            &mapping.parent,
                            mapping.season,
                            mapping.episode,
                        )
                    })
                    .collect::<Vec<_>>();
                episodes.sort();
                episodes.dedup();
                (partition.canonical_season, episodes)
            })
            .collect::<Vec<_>>();
        partitions.sort();
        partitions.dedup();
        let mut parents = if partitions.is_empty() {
            self.parents.iter().collect::<Vec<_>>()
        } else {
            vec![]
        };
        parents.sort();
        parents.dedup();
        digest(&(
            "catalog-mapping:v1",
            &self.source,
            &self.season,
            &self.source_inventory_fingerprint,
            partitions,
            parents,
        ))
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

/// Refine catalog parent proof with independently validated detail/cache proof.
/// Rich partitions win only when both claims agree; conflicts never regress to
/// a weaker preview relation that would silently hide a contradictory detail.
pub fn merge_grouping_evidence(
    explicit: Option<GroupingEvidence>,
    cached: Option<GroupingEvidence>,
) -> Option<GroupingEvidence> {
    match (explicit, cached) {
        (None, None) => None,
        (Some(evidence), None) | (None, Some(evidence)) => evidence.verified().then_some(evidence),
        (Some(explicit), Some(cached)) => {
            if !explicit.verified() || !cached.verified() || explicit.source != cached.source {
                return None;
            }
            // Independently confirmed parent/episode evidence supersedes an
            // optional suggestion; inference must never taint its provenance.
            if explicit.is_inferred() != cached.is_inferred() {
                return Some(if explicit.is_inferred() {
                    cached
                } else {
                    explicit
                });
            }
            let mut parents = explicit.parents.clone();
            parents.extend(cached.parents.clone());
            parents.sort();
            parents.dedup();
            if has_conflict(&parents) {
                return None;
            }
            if explicit.is_inferred()
                && explicit.source_inventory_fingerprint != cached.source_inventory_fingerprint
            {
                return None;
            }
            let season = match (&explicit.season, &cached.season) {
                (SeasonMembership::Unknown, season) | (season, SeasonMembership::Unknown) => {
                    season.clone()
                }
                (first, second) if first == second => first.clone(),
                _ => return None,
            };
            if !explicit.season_partitions.is_empty()
                && !cached.season_partitions.is_empty()
                && (explicit.source_inventory_fingerprint != cached.source_inventory_fingerprint
                    || explicit.season_partitions != cached.season_partitions)
            {
                return None;
            }
            let mut merged = if cached.season_partitions.is_empty() {
                explicit.clone()
            } else {
                cached.clone()
            };
            merged.parents = parents;
            merged.season = season;
            merged.proofs = explicit.proofs;
            for proof in cached.proofs {
                if !merged.proofs.contains(&proof) {
                    merged.proofs.push(proof);
                }
            }
            merged.verified().then_some(merged)
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
    pub representative: Arc<OwnedCatalogEntry>,
    pub members: Vec<Arc<OwnedCatalogEntry>>,
    pub original_index: usize,
    pub proven_mapping: Vec<Arc<GroupingEvidence>>,
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
        let evidence = eligible(member)?;
        let partition = evidence
            .season_partitions
            .iter()
            .find(|partition| partition.canonical_season == season)?;
        Some(SeasonSelection {
            source: source.clone(),
            source_inventory_fingerprint: evidence.source_inventory_fingerprint.clone()?,
            mapping_fingerprint: evidence.mapping_fingerprint(),
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

/// Opaque stable digest for local display/context identities, never a media ID.
pub fn projection_fingerprint(value: &impl Serialize) -> String {
    digest(value)
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
    // One retained allocation per source, shared by every virtual season card.
    let retained: Vec<_> = entries.iter().cloned().map(Arc::new).collect();
    let evidence: Vec<_> = entries
        .iter()
        .map(|entry| eligible(entry).cloned().map(Arc::new))
        .collect();
    let mut components: Vec<Vec<usize>> = Vec::new();
    if mode != CatalogGroupingMode::Original {
        for (index, entry) in entries.iter().enumerate() {
            let Some(current_evidence) = evidence[index].as_ref() else {
                continue;
            };
            let mut overlaps = Vec::new();
            for (component_index, component) in components.iter().enumerate() {
                if component.iter().any(|member| {
                    let other = &entries[*member];
                    // Different installed sources retain their own catalogs and
                    // policy boundaries, even when their public labels match.
                    other.source.addon_url == entry.source.addon_url
                        && evidence[*member].as_ref().is_some_and(|other| {
                            other
                                .parents
                                .iter()
                                .any(|id| current_evidence.parents.contains(id))
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
            .filter_map(|i| evidence[*i].as_ref())
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
                    let Some(evidence) = evidence[index].as_ref() else {
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
            let proven_mapping = indices
                .iter()
                .filter_map(|index| evidence[*index].clone())
                .collect::<Vec<_>>();
            let members: Vec<_> = indices.into_iter().map(|i| retained[i].clone()).collect();
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
                representative: retained[index].clone(),
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

fn episode_has_canonical_claim(video: &addons::Video) -> bool {
    if ["imdb_id", "imdbId", "imdbSeason", "imdbEpisode"]
        .iter()
        .any(|field| video.extra.contains_key(*field))
    {
        return true;
    }
    // Also recognize malformed canonical coordinates as an attempted claim,
    // rather than silently treating them as a bare native ID for cache reuse.
    video
        .id
        .rsplit_once(':')
        .and_then(|(parent_season, _)| parent_season.rsplit_once(':'))
        .is_some_and(|(parent, _)| ParentIdentity::parse(parent).is_some())
}

fn header_agrees_with_mapping(detail: &MetaItem, evidence: &GroupingEvidence) -> bool {
    for fields in [&detail.preview.extra, &detail.extra] {
        let ids = crate::normalize_external_ids(fields, &detail.preview.id, &detail.preview.type_);
        for id in ids.typed {
            let claim = match id {
                crate::ExternalId::Imdb(id) => ParentIdentity::ImdbSeries(id),
                crate::ExternalId::TmdbTv(id) => ParentIdentity::TmdbTv(id.get()),
                _ => continue,
            };
            if evidence
                .parents
                .iter()
                .any(|parent| parent.namespace() == claim.namespace() && parent != &claim)
            {
                return false;
            }
        }
        for field in ["imdb_id", "imdbId", "_imdbId", "imdb"] {
            let Some(value) = fields
                .get(field)
                .filter(|value| !value.is_null() && value.as_str() != Some(""))
            else {
                continue;
            };
            let Some(claim @ ParentIdentity::ImdbSeries(_)) =
                value.as_str().and_then(ParentIdentity::parse)
            else {
                return false;
            };
            if evidence
                .parents
                .iter()
                .any(|parent| parent.namespace() == claim.namespace() && parent != &claim)
            {
                return false;
            }
        }
        for field in ["tvdb_id", "tvdbId", "_tvdbId", "tvdb"] {
            let Some(value) = fields.get(field).filter(|value| !value.is_null()) else {
                continue;
            };
            let raw = value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|id| id.to_string()));
            let Some(raw) = raw else {
                return false;
            };
            let parsed = ParentIdentity::parse(&raw)
                .or_else(|| ParentIdentity::parse(&format!("tvdb:{raw}")));
            let Some(claim @ ParentIdentity::TvdbSeries(_)) = parsed else {
                return false;
            };
            if evidence
                .parents
                .iter()
                .any(|parent| parent.namespace() == claim.namespace() && parent != &claim)
            {
                return false;
            }
        }
    }
    true
}

/// A fresh owner response is authoritative for its current native inventory.
/// Bare metadata may retain a complete mapping established by an external
/// broker, but never mask disappearing owner proof or partial/conflicting
/// canonical fields. A changed valid mapping replaces stale cache proof so an
/// opening bound to the older mapping fingerprint is safely rejected.
pub fn evidence_for_refreshed_detail(
    source: &SourceIdentity,
    detail: &MetaItem,
    owner_url: &str,
    cached: Option<GroupingEvidence>,
) -> Option<GroupingEvidence> {
    if source.id != detail.preview.id
        || source.media_type != detail.preview.type_
        || source.addon_url != owner_url
        || !matches!(source.media_type.as_str(), "series" | "anime")
    {
        return None;
    }
    let explicit_present = detail.preview.extra.contains_key("novaCatalogGrouping");
    if explicit_present && evidence_from_preview(source, &detail.preview, owner_url).is_none() {
        return None;
    }
    if detail.videos.iter().any(|video| video.season == Some(0)) {
        // A special prevents a complete regular-season partition, but cannot
        // erase a separately broker-verified parent for this exact inventory.
        // Never infer this relationship from untrusted novaStreamIds alone.
        let inventory =
            episode_inventory_fingerprint(detail.videos.iter().map(|video| video.id.as_str()));
        let owner = crate::addon_metadata_id(owner_url);
        let mut cached = cached.filter(|cached| {
            cached.verified()
                && cached.source == *source
                && cached.source_inventory_fingerprint.as_ref() == Some(&inventory)
                && cached.proofs.iter().any(|proof| {
                    matches!(proof,
                    GroupingProof::BrokerAlignment { provider_id, .. }
                        if !provider_id.is_empty() && provider_id != &owner)
                })
                && header_agrees_with_mapping(detail, cached)
                && detail.videos.iter().all(|video| {
                    !episode_has_canonical_claim(video)
                        || video_canonical_episode_including_specials(video)
                            .is_some_and(|(parent, _, _)| cached.parents.contains(&parent))
                })
        })?;
        if explicit_present {
            cached = merge_grouping_evidence(
                Some(cached),
                evidence_from_preview(source, &detail.preview, owner_url),
            )?;
        }
        // Keep the entire native source reachable in Season mode. Emitting
        // only its regular seasons would silently lose the special.
        cached.season = SeasonMembership::Unknown;
        cached.season_partitions.clear();
        return cached.verified().then_some(cached);
    }
    let claims = detail.videos.iter().any(episode_has_canonical_claim);
    let fresh = evidence_from_detail(source, detail, owner_url);
    if claims
        && fresh
            .as_ref()
            .is_none_or(|fresh| fresh.season_partitions.is_empty())
    {
        return None;
    }
    if let Some(fresh) = fresh
        .as_ref()
        .filter(|fresh| !fresh.season_partitions.is_empty())
    {
        if !header_agrees_with_mapping(detail, fresh) {
            return None;
        }
        return match cached.filter(|cached| {
            cached.verified()
                && cached.source == *source
                && cached.mapping_fingerprint() == fresh.mapping_fingerprint()
        }) {
            Some(cached) => merge_grouping_evidence(Some(cached), Some(fresh.clone()))
                .or_else(|| Some(fresh.clone())),
            None => Some(fresh.clone()),
        };
    }
    // Explicit parent/season relations are valid current evidence even when
    // the owner never supplied canonical episode partitions. Keep that weaker
    // relation while dropping stale complete partitions; old virtual openings
    // then fail their mapping/partition checks instead of clearing Series proof.
    if fresh
        .as_ref()
        .is_some_and(|fresh| !header_agrees_with_mapping(detail, fresh))
    {
        return None;
    }
    if detail.videos.is_empty() {
        return fresh;
    }
    let inventory =
        episode_inventory_fingerprint(detail.videos.iter().map(|video| video.id.as_str()));
    let Some(cached) = cached.filter(|cached| {
        cached.verified()
            && cached.source == *source
            && !cached.season_partitions.is_empty()
            && cached.source_inventory_fingerprint.as_ref() == Some(&inventory)
    }) else {
        return fresh;
    };
    let owner = crate::addon_metadata_id(owner_url);
    let owner_lost_proof = cached.proofs.iter().any(|proof| matches!(proof, GroupingProof::CanonicalEpisodes {provider_id,..} if provider_id == &owner));
    let external_broker = cached.proofs.iter().any(|proof| matches!(proof, GroupingProof::BrokerAlignment {provider_id,..} | GroupingProof::CanonicalEpisodes {provider_id,..} if !provider_id.is_empty() && provider_id != &owner));
    if owner_lost_proof
        || !external_broker
        || claims
        || !header_agrees_with_mapping(detail, &cached)
    {
        return fresh;
    }
    merge_grouping_evidence(fresh.clone(), Some(cached)).or(fresh)
}

/// Normalize only explicit requested-ID aliases using the shared Stremio guard,
/// then extract parent proof. Merely returning a different canonical title ID
/// never licenses replacing the native catalog identity.
pub fn evidence_from_response(
    source: &SourceIdentity,
    bytes: &[u8],
    provider_url: &str,
) -> Option<GroupingEvidence> {
    let detail = detail_from_response(source, bytes, provider_url)?;
    let evidence = evidence_from_detail(source, &detail, provider_url)?;
    header_agrees_with_mapping(&detail, &evidence).then_some(evidence)
}

pub(crate) fn detail_from_response(
    source: &SourceIdentity,
    bytes: &[u8],
    provider_url: &str,
) -> Option<MetaItem> {
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
    (detail.preview.id == source.id && detail.preview.type_ == source.media_type).then_some(detail)
}

/// Explicit canonical episode triples are an ordinary compatible-addon field
/// contract (including AIOMetadata's imdb_id/imdbSeason/imdbEpisode output).
/// They describe mapped episodes, unlike whole-title external-ID aliases.
fn video_canonical_episode(video: &addons::Video) -> Option<(ParentIdentity, u32, u32)> {
    video_canonical_episode_including_specials(video).filter(|(_, season, _)| *season > 0)
}

fn video_canonical_episode_including_specials(
    video: &addons::Video,
) -> Option<(ParentIdentity, u32, u32)> {
    let canonical = canonical_episode_including_specials(&video.id);
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
            let season = u32::try_from(video.extra.get("imdbSeason")?.as_u64()?).ok()?;
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
    canonical_episode_including_specials(id).filter(|(_, season, _)| *season > 0)
}

fn canonical_episode_including_specials(id: &str) -> Option<(ParentIdentity, u32, u32)> {
    let (parent_season, episode) = id.rsplit_once(':')?;
    let (parent, season) = parent_season.rsplit_once(':')?;
    let season = season.parse::<u32>().ok()?;
    let episode = episode.parse::<u32>().ok().filter(|n| *n > 0)?;
    Some((ParentIdentity::parse(parent)?, season, episode))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupingLookupStatus {
    Original,
    Confirmed,
    Inferred,
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
    resolve_catalog_grouping_evidence_with_inference(source, mode, revision, false)
}

pub fn resolve_catalog_grouping_evidence_with_inference(
    source: &SourceIdentity,
    mode: CatalogGroupingMode,
    revision: u64,
    allow_inferred: bool,
) -> GroupingLookup {
    crate::metadata::resolve_catalog_grouping_evidence_with_inference(
        source,
        mode,
        revision,
        allow_inferred,
    )
}

/// Retained canonical metadata, supplied by the exact owner's relation
/// response. It is a navigation option alongside the original native release.
pub fn inferred_parent_entry(evidence: &GroupingEvidence) -> Option<OwnedCatalogEntry> {
    if !evidence.is_inferred() || !evidence.verified() {
        return None;
    }
    let mut parents = evidence.proofs.iter().filter_map(|proof| match proof {
        GroupingProof::InferredSeasonChain { parent_preview, .. } => Some(parent_preview),
        _ => None,
    });
    let parent = parents.next()?;
    if parents.any(|other| other.source != parent.source) {
        return None;
    }
    Some(OwnedCatalogEntry {
        source: parent.source.clone(),
        preview: MetaPreview {
            id: parent.source.id.clone(),
            type_: parent.source.media_type.clone(),
            name: parent.name.clone(),
            poster: parent.poster.clone(),
            background: parent.background.clone(),
            logo: parent.logo.clone(),
            release_info: parent.release_info.clone(),
            ..Default::default()
        },
        evidence: None,
    })
}

pub(crate) struct SeasonRelation {
    pub source: SourceIdentity,
    pub season: u32,
}

fn anime_release_id(id: &str) -> bool {
    matches!(
        crate::ExternalId::parse(id),
        Some(crate::ExternalId::MalAnime(_) | crate::ExternalId::AnilistAnime(_))
    ) && !id.contains("://")
        || id.strip_prefix("kitsu:").is_some_and(|id| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && id
                    .parse::<u32>()
                    .is_ok_and(|id| id > 0 && id <= i32::MAX as u32)
        })
}

fn bounded_art(value: Option<String>) -> Option<String> {
    value.filter(|value| {
        value.len() <= 8192
            && url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
    })
}

fn excluded_season_title(title: &str) -> bool {
    let title = crate::sequence::normalized_title(title);
    [
        "remake",
        "reboot",
        "spinoff",
        "spin off",
        "side story",
        "alternate",
        "alternative",
        "recap",
        "movie",
        "film",
    ]
    .iter()
    .any(|marker| format!(" {title} ").contains(&format!(" {marker} ")))
}

fn season_family(title: &str) -> String {
    crate::sequence::series_identity(title)
        .base
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn family_matches(detail: &MetaItem, family: &str) -> bool {
    std::iter::once(detail.preview.title())
        .chain(
            [&detail.preview.extra, &detail.extra]
                .into_iter()
                .flat_map(|extra| {
                    ["aliases", "aka", "akaNames"]
                        .into_iter()
                        .flat_map(move |field| {
                            extra
                                .get(field)
                                .and_then(serde_json::Value::as_array)
                                .into_iter()
                                .flatten()
                                .take(16)
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                }),
        )
        .any(|title| !excluded_season_title(&title) && season_family(&title) == family)
}

fn release_aliases(detail: &MetaItem) -> BTreeSet<String> {
    let mut aliases = BTreeSet::from([detail.preview.id.clone()]);
    for fields in [&detail.preview.extra, &detail.extra] {
        for id in
            crate::normalize_external_ids(fields, &detail.preview.id, &detail.preview.type_).typed
        {
            match id {
                crate::ExternalId::MalAnime(id) => {
                    aliases.insert(format!("mal:{id}"));
                }
                crate::ExternalId::AnilistAnime(id) => {
                    aliases.insert(format!("anilist:{id}"));
                }
                _ => {}
            }
        }
        for field in ["kitsu_id", "kitsuId", "_kitsuId", "kitsu"] {
            if let Some(value) = fields.get(field).and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|v| v.to_string()))
            }) {
                let value = value.strip_prefix("kitsu:").unwrap_or(&value);
                if value.parse::<u32>().is_ok_and(|id| id > 0) {
                    aliases.insert(format!("kitsu:{value}"));
                }
            }
        }
    }
    aliases
}

/// Parse only an explicit, unique addon-native prequel link. Titles constrain
/// the relationship; they never discover or prove it on their own.
pub(crate) fn season_relation(
    source: &SourceIdentity,
    detail: &MetaItem,
) -> Option<SeasonRelation> {
    if detail.preview.id != source.id
        || detail.preview.type_ != source.media_type
        || !matches!(source.media_type.as_str(), "series" | "anime")
        || excluded_season_title(&detail.preview.title())
        || detail.preview.extra.contains_key("novaCatalogGrouping")
    {
        return None;
    }
    let current = crate::sequence::series_identity(&detail.preview.title());
    if !current.labeled || current.season < 2 || current.part != 1 {
        return None;
    }
    let family = season_family(&detail.preview.title());
    let mut relations = BTreeMap::new();
    for fields in [&detail.preview.extra, &detail.extra] {
        if fields
            .get("links")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|links| links.len() > 64)
        {
            return None;
        }
        for link in fields
            .get("links")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .take(64)
        {
            if !link
                .get("category")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("prequel"))
            {
                continue;
            }
            let url = url::Url::parse(link.get("url")?.as_str()?).ok()?;
            if url.scheme() != "stremio"
                || url.host_str().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            let parts = url.path_segments()?.collect::<Vec<_>>();
            if parts.len() != 3 || parts[0] != "detail" || !matches!(parts[1], "series" | "anime") {
                return None;
            }
            let id = percent_encoding::percent_decode_str(parts[2])
                .decode_utf8()
                .ok()?
                .into_owned();
            if !anime_release_id(&id) || id == source.id {
                return None;
            }
            // The native title can carry MAL/Kitsu aliases; a prequel pointing
            // back to one of those aliases is a cycle, not a preceding season.
            if release_aliases(detail).contains(&id) {
                return None;
            }
            let title = link.get("name")?.as_str()?;
            let previous = crate::sequence::series_identity(title);
            if excluded_season_title(title)
                || (!previous.labeled && current.season != 2)
                || previous.part != 1
                || previous.season.checked_add(1) != Some(current.season)
                || season_family(title) != family
            {
                return None;
            }
            relations.insert(
                id.clone(),
                SeasonRelation {
                    source: SourceIdentity {
                        addon_url: source.addon_url.clone(),
                        media_type: parts[1].into(),
                        id,
                    },
                    season: previous.season,
                },
            );
        }
    }
    if relations.len() != 1 {
        return None;
    }
    relations.into_values().next()
}

/// The target request may return its whole TV series instead of an alias of
/// that standalone release. Keep that original canonical identity and prove
/// the relationship through native episode membership, never title rewriting.
pub(crate) fn infer_season_parent(
    source: &SourceIdentity,
    detail: &MetaItem,
    relation: &SeasonRelation,
    parent_detail: &MetaItem,
    provider_url: &str,
) -> Option<GroupingEvidence> {
    if relation.source.addon_url != source.addon_url
        || provider_url != source.addon_url
        || parent_detail.preview.type_ != "series"
        || excluded_season_title(&parent_detail.preview.title())
        || !family_matches(parent_detail, &season_family(&detail.preview.title()))
        || parent_detail.videos.is_empty()
        || parent_detail.videos.len() > 10_000
    {
        return None;
    }
    let source_aliases = release_aliases(detail);
    for fields in [&parent_detail.preview.extra, &parent_detail.extra] {
        if fields
            .get("links")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|links| links.len() > 64)
        {
            return None;
        }
        if fields
            .get("links")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .take(64)
            .any(|link| {
                link.get("category")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("prequel"))
                    && link
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|url| url.rsplit('/').next())
                        .is_some_and(|target| source_aliases.contains(target))
            })
        {
            return None;
        }
    }
    let parent = ParentIdentity::parse(&parent_detail.preview.id)?;
    // Native anime numbering may stay local S1 even when an explicit mapping
    // says canonical S3. Only validate attempted claims and parent agreement;
    // inference never installs or changes any episode correspondence.
    if detail.videos.iter().any(|video| {
        episode_has_canonical_claim(video)
            && video_canonical_episode_including_specials(video)
                .is_none_or(|(claimed, _, _)| claimed != parent)
    }) {
        return None;
    }
    let mut native = BTreeSet::new();
    let mut positions = BTreeSet::new();
    let prefix = format!("{}:", relation.source.id);
    let mut matched = 0;
    for video in &parent_detail.videos {
        if video.id.is_empty() || !native.insert(&video.id) {
            return None;
        }
        if let (Some(season), Some(episode)) = (video.season, video.episode_number())
            && !positions.insert((season, episode))
        {
            return None;
        }
        if episode_has_canonical_claim(video) {
            let (claimed, season, episode) = video_canonical_episode_including_specials(video)?;
            if claimed != parent
                || video.season.is_some_and(|value| value != season)
                || video.episode_number().is_some_and(|value| value != episode)
            {
                return None;
            }
        }
        if let Some(number) = video.id.strip_prefix(&prefix) {
            if !number.parse::<u32>().is_ok_and(|number| number > 0)
                || video.season != Some(relation.season)
                || video.episode_number().is_none_or(|n| n == 0)
            {
                return None;
            }
            matched += 1;
        }
    }
    if matched == 0 {
        return None;
    }
    let current = crate::sequence::series_identity(&detail.preview.title());
    let evidence = GroupingEvidence {
        source: source.clone(),
        parents: vec![parent],
        season: SeasonMembership::Unknown,
        proofs: vec![GroupingProof::InferredSeasonChain {
            provider_id: crate::addon_metadata_id(provider_url),
            related_release_id: relation.source.id.clone(),
            source_season: current.season,
            related_season: relation.season,
            parent_preview: InferredParentPreview {
                source: SourceIdentity {
                    addon_url: source.addon_url.clone(),
                    media_type: parent_detail.preview.type_.clone(),
                    id: parent_detail.preview.id.clone(),
                },
                name: parent_detail.preview.title(),
                poster: bounded_art(parent_detail.preview.poster.clone()),
                background: bounded_art(parent_detail.preview.background.clone()),
                logo: bounded_art(parent_detail.preview.logo.clone()),
                release_info: parent_detail
                    .preview
                    .year_str()
                    .filter(|value| value.len() <= 64)
                    .map(serde_json::Value::String),
            },
        }],
        season_partitions: vec![],
        source_inventory_fingerprint: Some(episode_inventory_fingerprint(
            detail.videos.iter().map(|video| video.id.as_str()),
        )),
    };
    (evidence.verified()
        && header_agrees_with_mapping(detail, &evidence)
        && header_agrees_with_mapping(parent_detail, &evidence))
    .then_some(evidence)
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
        // Parent-only proof is still bound to the exact native inventory.
        // In particular, a broker may map every regular episode while leaving
        // specials untouched; Detail must be able to retain that parent proof.
        source_inventory_fingerprint: Some(episode_inventory_fingerprint(
            result
                .details
                .episodes
                .iter()
                .map(|episode| episode.source_id.as_str()),
        )),
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
        assert!(Arc::ptr_eq(
            &seasons[0].representative,
            &seasons[1].representative
        ));
        assert!(Arc::ptr_eq(&seasons[0].members[0], &seasons[1].members[0]));
        assert!(Arc::ptr_eq(
            &seasons[0].proven_mapping[0],
            &seasons[1].proven_mapping[0]
        ));
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
        assert_eq!(
            whole_scope.mapping_fingerprint,
            evidence.mapping_fingerprint()
        );
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
    fn merging_preview_and_cache_refines_partitions_and_never_hides_conflicts() {
        let explicit = entry("mal:1", &["tt1"], None).evidence.unwrap();
        let mut cached = explicit.clone();
        cached.season = SeasonMembership::Season(2);
        cached.source_inventory_fingerprint = Some(episode_inventory_fingerprint(["mal:1:1"]));
        cached.season_partitions = vec![ConfirmedSeasonPartition {
            canonical_season: 2,
            episodes: vec![CanonicalEpisodeMapping {
                source_episode_id: "mal:1:1".into(),
                parent: ParentIdentity::ImdbSeries("tt1".into()),
                season: 2,
                episode: 1,
            }],
        }];
        let merged = merge_grouping_evidence(Some(explicit.clone()), Some(cached.clone())).unwrap();
        assert_eq!(merged.season_partitions, cached.season_partitions);
        assert_eq!(merged.season, SeasonMembership::Season(2));
        let mut conflicting = explicit.clone();
        conflicting.season = SeasonMembership::Season(1);
        assert!(merge_grouping_evidence(Some(conflicting), Some(cached.clone())).is_none());
        let mut conflicting = explicit.clone();
        conflicting.parents = vec![ParentIdentity::ImdbSeries("tt2".into())];
        assert!(merge_grouping_evidence(Some(conflicting), Some(cached)).is_none());
    }

    #[test]
    fn merged_preview_and_identical_fresh_detail_have_the_same_evidence_fingerprint() {
        let owner = "https://owner.example";
        let value = json!({"meta":{"id":"kitsu:77","type":"anime","novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tvdb:7","tt7"]},"videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}
        ]}});
        let detail = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        let entry = OwnedCatalogEntry::new(owner, detail.preview.clone());
        let fresh = evidence_from_detail(&entry.source, &detail, owner).unwrap();
        let merged = merge_grouping_evidence(entry.evidence, Some(fresh.clone())).unwrap();
        assert_eq!(merged.fingerprint(), fresh.fingerprint());
        let mut reordered = fresh.clone();
        reordered.parents.reverse();
        reordered.proofs.reverse();
        reordered.season_partitions.reverse();
        for proof in &mut reordered.proofs {
            if let GroupingProof::CanonicalEpisodes { episode_ids, .. } = proof {
                episode_ids.reverse();
            }
        }
        assert_eq!(reordered.fingerprint(), fresh.fingerprint());
        reordered.source_inventory_fingerprint = Some("changed-native-inventory".into());
        assert_ne!(reordered.fingerprint(), fresh.fingerprint());
    }

    #[test]
    fn mapping_fingerprint_accepts_corroborating_proof_subsets_but_binds_actual_mapping() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "anime".into(),
            id: "kitsu:77".into(),
        };
        let value = json!({"meta":{"id":"kitsu:77","type":"anime","videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}
        ]}});
        let detail = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        let owner = evidence_from_detail(&source, &detail, &source.addon_url).unwrap();
        let mut enriched = owner.clone();
        enriched.parents.push(ParentIdentity::TvdbSeries(9));
        enriched.proofs.extend([
            GroupingProof::ExplicitParent {
                provider_id: "catalog-provider".into(),
            },
            GroupingProof::BrokerAlignment {
                provider_id: "another-proof-provider".into(),
                episode_ids: vec!["tt7:1:1".into(), "tt7:2:1".into()],
            },
        ]);
        assert!(enriched.verified());
        assert_ne!(owner.fingerprint(), enriched.fingerprint());
        assert_eq!(owner.mapping_fingerprint(), enriched.mapping_fingerprint());
        enriched.parents.reverse();
        enriched.proofs.reverse();
        enriched.season_partitions.reverse();
        assert_eq!(owner.mapping_fingerprint(), enriched.mapping_fingerprint());
        let mut changed = enriched.clone();
        changed.source.addon_url = "https://other.example".into();
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = enriched.clone();
        changed.source.id = "mal:88".into();
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = enriched.clone();
        changed.source_inventory_fingerprint = Some("changed-inventory".into());
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = owner.clone();
        changed.parents = vec![ParentIdentity::ImdbSeries("tt8".into())];
        for partition in &mut changed.season_partitions {
            for mapping in &mut partition.episodes {
                mapping.parent = ParentIdentity::ImdbSeries("tt8".into());
            }
        }
        assert!(changed.verified());
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = owner.clone();
        changed.season_partitions[1].canonical_season = 3;
        changed.season_partitions[1].episodes[0].season = 3;
        assert!(changed.verified());
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = owner.clone();
        changed.season_partitions[1].episodes[0].episode = 2;
        assert!(changed.verified());
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
        let mut changed = owner.clone();
        changed.season = SeasonMembership::Season(1);
        assert_ne!(owner.mapping_fingerprint(), changed.mapping_fingerprint());
    }

    #[test]
    fn agreeing_parent_and_multiple_membership_cannot_hide_conflicting_episode_partitions() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "anime".into(),
            id: "kitsu:77".into(),
        };
        let value = json!({"meta":{"id":"kitsu:77","type":"anime","videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}
        ]}});
        let detail = addons::Addon::parse_meta(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        let first = evidence_from_detail(&source, &detail, &source.addon_url).unwrap();
        let mut second = first.clone();
        second.season_partitions[0].episodes[0].source_episode_id = "kitsu:77:2".into();
        second.season_partitions[1].episodes[0].source_episode_id = "kitsu:77:1".into();
        assert!(first.verified() && second.verified());
        assert_eq!(first.parents, second.parents);
        assert_eq!(first.season, second.season);
        assert!(merge_grouping_evidence(Some(first.clone()), Some(second)).is_none());
        let explicit = GroupingEvidence {
            source,
            parents: first.parents.clone(),
            season: SeasonMembership::Unknown,
            proofs: vec![GroupingProof::ExplicitParent {
                provider_id: "catalog-provider".into(),
            }],
            season_partitions: vec![],
            source_inventory_fingerprint: None,
        };
        let refined = merge_grouping_evidence(Some(explicit), Some(first.clone())).unwrap();
        assert_eq!(refined.season_partitions, first.season_partitions);
        assert_eq!(refined.mapping_fingerprint(), first.mapping_fingerprint());
    }

    #[test]
    fn refreshed_owner_metadata_retains_only_compatible_complete_external_proof() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "anime".into(),
            id: "kitsu:77".into(),
        };
        let canonical = json!({"meta":{"id":"kitsu:77","type":"anime","videos":[
            {"id":"kitsu:77:1","season":1,"episode":1,"imdb_id":"tt7","imdbSeason":1,"imdbEpisode":1},
            {"id":"kitsu:77:2","season":1,"episode":2,"imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}
        ]}});
        let parse = |value: &serde_json::Value| {
            addons::Addon::parse_meta(&serde_json::to_vec(value).unwrap())
                .unwrap()
                .unwrap()
        };
        let owner_proof =
            evidence_from_detail(&source, &parse(&canonical), &source.addon_url).unwrap();
        let mut external = owner_proof.clone();
        external.proofs = vec![GroupingProof::BrokerAlignment {
            provider_id: crate::addon_metadata_id("https://external.example"),
            episode_ids: vec!["tt7:1:1".into(), "tt7:2:1".into()],
        }];
        let bare = json!({"meta":{"id":"kitsu:77","type":"anime","videos":[{"id":"kitsu:77:1","season":1,"episode":1},{"id":"kitsu:77:2","season":1,"episode":2}]}});
        let retained = evidence_for_refreshed_detail(
            &source,
            &parse(&bare),
            &source.addon_url,
            Some(external.clone()),
        )
        .unwrap();
        assert_eq!(
            retained.mapping_fingerprint(),
            external.mapping_fingerprint()
        );
        assert!(
            evidence_for_refreshed_detail(
                &source,
                &parse(&bare),
                &source.addon_url,
                Some(owner_proof.clone())
            )
            .is_none()
        );
        let mut second_addon = external.clone();
        second_addon.proofs = vec![GroupingProof::CanonicalEpisodes {
            provider_id: crate::addon_metadata_id("https://external.example"),
            episode_ids: vec!["tt7:1:1".into(), "tt7:2:1".into()],
        }];
        assert!(
            evidence_for_refreshed_detail(
                &source,
                &parse(&bare),
                &source.addon_url,
                Some(second_addon)
            )
            .is_some()
        );
        for mut bad in [bare.clone(), bare.clone(), bare.clone(), bare.clone()]
            .into_iter()
            .enumerate()
        {
            match bad.0 {
                0 => {
                    bad.1["meta"]["videos"][0]["imdbSeason"] = json!(1);
                }
                1 => {
                    bad.1["meta"]["imdb_id"] = json!("tt8");
                }
                2 => {
                    bad.1["meta"]["videos"][0]["id"] = json!("changed-native-episode");
                }
                _ => {
                    bad.1["meta"]["novaCatalogGrouping"] =
                        json!({"version":1,"kind":"series_member","parentIds":["tt7","tt8"]});
                }
            }
            assert!(
                evidence_for_refreshed_detail(
                    &source,
                    &parse(&bad.1),
                    &source.addon_url,
                    Some(external.clone())
                )
                .is_none()
            );
        }
        let mut partial = canonical.clone();
        partial["meta"]["videos"][1]
            .as_object_mut()
            .unwrap()
            .remove("imdbSeason");
        assert!(
            evidence_for_refreshed_detail(
                &source,
                &parse(&partial),
                &source.addon_url,
                Some(external.clone())
            )
            .is_none()
        );
        let mut changed = canonical;
        changed["meta"]["videos"][1]["imdbSeason"] = json!(3);
        let fresh = evidence_for_refreshed_detail(
            &source,
            &parse(&changed),
            &source.addon_url,
            Some(owner_proof.clone()),
        )
        .unwrap();
        assert_eq!(fresh.season_partitions[1].canonical_season, 3);
        assert_ne!(
            fresh.mapping_fingerprint(),
            owner_proof.mapping_fingerprint()
        );
    }

    #[test]
    fn fresh_explicit_only_relations_survive_refresh_without_inventing_or_retaining_partitions() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "anime".into(),
            id: "kitsu:77".into(),
        };
        let parse = |value: &serde_json::Value| {
            addons::Addon::parse_meta(&serde_json::to_vec(value).unwrap())
                .unwrap()
                .unwrap()
        };
        let mut value = json!({"meta":{"id":"kitsu:77","type":"anime","novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["tt7"]},"videos":[
            {"id":"kitsu:77:1","season":1,"episode":1},
            {"id":"kitsu:77:2","season":1,"episode":2}
        ]}});
        let relation =
            evidence_for_refreshed_detail(&source, &parse(&value), &source.addon_url, None)
                .unwrap();
        assert_eq!(relation.season, SeasonMembership::Unknown);
        assert!(relation.season_partitions.is_empty());
        assert!(relation.source_inventory_fingerprint.is_none());
        assert_eq!(
            evidence_for_refreshed_detail(
                &source,
                &parse(&value),
                &source.addon_url,
                Some(relation.clone())
            )
            .unwrap()
            .fingerprint(),
            relation.fingerprint()
        );
        let mut canonical = value.clone();
        for (index, video) in canonical["meta"]["videos"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            video["imdb_id"] = json!("tt7");
            video["imdbSeason"] = json!(index + 1);
            video["imdbEpisode"] = json!(1);
        }
        let owner_complete =
            evidence_from_detail(&source, &parse(&canonical), &source.addon_url).unwrap();
        let weaker = evidence_for_refreshed_detail(
            &source,
            &parse(&value),
            &source.addon_url,
            Some(owner_complete.clone()),
        )
        .unwrap();
        assert!(weaker.season_partitions.is_empty());
        assert_ne!(
            weaker.mapping_fingerprint(),
            owner_complete.mapping_fingerprint()
        );
        let mut external = owner_complete.clone();
        external.proofs = vec![GroupingProof::BrokerAlignment {
            provider_id: crate::addon_metadata_id("https://external.example"),
            episode_ids: vec!["tt7:1:1".into(), "tt7:2:1".into()],
        }];
        let refined = evidence_for_refreshed_detail(
            &source,
            &parse(&value),
            &source.addon_url,
            Some(external),
        )
        .unwrap();
        assert_eq!(refined.season_partitions, owner_complete.season_partitions);
        value["meta"]["novaCatalogGrouping"]["season"] = json!(2);
        let season_only = evidence_for_refreshed_detail(
            &source,
            &parse(&value),
            &source.addon_url,
            Some(owner_complete.clone()),
        )
        .unwrap();
        assert_eq!(season_only.season, SeasonMembership::Season(2));
        assert!(season_only.season_partitions.is_empty());
        assert_ne!(
            season_only.mapping_fingerprint(),
            owner_complete.mapping_fingerprint()
        );
        value["meta"]["videos"] = json!([]);
        assert!(
            evidence_for_refreshed_detail(&source, &parse(&value), &source.addon_url, None)
                .unwrap()
                .season_partitions
                .is_empty()
        );
        value["meta"]["videos"] =
            json!([{"id":"kitsu:77:1","season":1,"episode":1,"imdbSeason":2}]);
        assert!(
            evidence_for_refreshed_detail(
                &source,
                &parse(&value),
                &source.addon_url,
                Some(owner_complete)
            )
            .is_none(),
            "a weaker relation never masks partial canonical claims"
        );
    }

    #[test]
    fn broker_parent_survives_a_combined_native_inventory_with_a_special() {
        let owner = "https://owner.example/manifest.json";
        let external = crate::addon_metadata_id("https://metadata.example/manifest.json");
        let source = SourceIdentity {
            addon_url: owner.into(),
            media_type: "series".into(),
            id: "mal:61990".into(),
        };
        let connection = crate::MetadataConnection {
            addon_id: external,
            media_id: "tt12590266".into(),
            ids: vec![crate::ExternalId::parse("tt12590266").unwrap()],
            basis: "identity-or-exact-title".into(),
        };
        let mut videos = vec![json!({
            "id":"tt12590266:0:1", "season":0, "episode":1,
            "title":"Ending Theme | Let You Down"
        })];
        let mut result = crate::EnrichmentResult {
            inventory_revision: 1,
            status: "confirmed".into(),
            details: crate::ProviderDetails {
                media: crate::MediaItem {
                    source_id: source.id.clone(),
                    media_type: source.media_type.clone(),
                    title: "Cyberpunk: Edgerunners".into(),
                    ..Default::default()
                },
                episodes: vec![crate::Episode {
                    source_id: "tt12590266:0:1".into(),
                    season: 0,
                    number: 1,
                    ..Default::default()
                }],
            },
            connections: vec![connection.clone()],
            episode_metadata: BTreeMap::new(),
            supplemental_seasons: vec![],
        };
        for (season, kitsu) in [(1, 43248), (2, 50016)] {
            for episode in 1..=10 {
                let native = format!("kitsu:{kitsu}:{episode}");
                let canonical = format!("tt12590266:{season}:{episode}");
                videos.push(json!({"id":native, "season":season, "episode":episode,
                    "novaStreamIds":[canonical]}));
                result.details.episodes.push(crate::Episode {
                    source_id: native.clone(),
                    season,
                    number: episode,
                    ..Default::default()
                });
                result.episode_metadata.insert(
                    native,
                    crate::EpisodeEnrichment {
                        ids: vec![canonical],
                        connections: vec![connection.clone()],
                        season: Some(season),
                        episode: Some(episode),
                        ..Default::default()
                    },
                );
            }
        }
        let value = json!({"meta":{"id":source.id,"type":"series",
            "name":"Cyberpunk: Edgerunners 2", "imdb_id":"tt12590266", "videos":videos}});
        let parse = |value: &serde_json::Value| {
            addons::Addon::parse_meta(&serde_json::to_vec(value).unwrap())
                .unwrap()
                .unwrap()
        };
        let detail = parse(&value);
        let cached = evidence_from_broker(&source, &result).unwrap();
        assert_eq!(cached.season, SeasonMembership::Unknown);
        assert!(cached.season_partitions.is_empty());
        assert_eq!(
            cached.source_inventory_fingerprint,
            Some(episode_inventory_fingerprint(
                detail.videos.iter().map(|video| video.id.as_str())
            ))
        );
        assert!(
            evidence_for_refreshed_detail(&source, &detail, owner, None).is_none(),
            "unverified novaStreamIds cannot establish parenthood"
        );
        let retained =
            evidence_for_refreshed_detail(&source, &detail, owner, Some(cached.clone())).unwrap();
        assert_eq!(
            retained.parents,
            [ParentIdentity::ImdbSeries("tt12590266".into())]
        );
        assert!(retained.season_partitions.is_empty());
        assert_eq!(retained.season, SeasonMembership::Unknown);
        let mut native = OwnedCatalogEntry::new(owner, detail.preview.clone());
        native.evidence = Some(retained);
        let mut parent = native.clone();
        parent.source.id = "tt12590266".into();
        parent.preview.id = parent.source.id.clone();
        parent.evidence.as_mut().unwrap().source = parent.source.clone();
        let entries = [native, parent];
        let grouped = project_catalog(&entries, Series);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].members.len(), 2);
        assert_eq!(grouped[0].members[0].source.id, "mal:61990");
        assert_eq!(grouped[0].members[1].source.id, "tt12590266");
        let seasons = project_catalog(&entries, Season);
        assert_eq!(seasons.len(), 2);
        assert!(
            seasons.iter().all(|group| group.canonical_season.is_none()),
            "the original sources keep the special reachable; no partial virtual seasons"
        );
        assert_eq!(detail.videos.len(), 21);
        assert_eq!(detail.videos[0].id, "tt12590266:0:1");
        assert_eq!(detail.videos[1].id, "kitsu:43248:1");
        assert_eq!(detail.videos[11].id, "kitsu:50016:1");
        for index in 0..4 {
            let mut changed = value.clone();
            match index {
                0 => changed["meta"]["videos"][1]["id"] = json!("changed-native-id"),
                1 => changed["meta"]["videos"][0]["id"] = json!("tt999:0:1"),
                2 => changed["meta"]["videos"][1]["imdbSeason"] = json!(1),
                _ => changed["meta"]["imdb_id"] = json!("tt999"),
            }
            assert!(
                evidence_for_refreshed_detail(
                    &source,
                    &parse(&changed),
                    owner,
                    Some(cached.clone())
                )
                .is_none()
            );
        }
        let mut unverified = result;
        unverified.connections.clear();
        assert!(evidence_from_broker(&source, &unverified).is_none());
    }

    #[test]
    fn catalog_response_rejects_a_header_that_contradicts_episode_parent_proof() {
        let source = SourceIdentity {
            addon_url: "https://owner.example".into(),
            media_type: "series".into(),
            id: "mal:7".into(),
        };
        let mut value = json!({"meta":{"id":"mal:7","type":"series","imdb_id":"tt7",
            "videos":[{"id":"mal:7:1","season":1,"episode":1,
                "imdb_id":"tt7","imdbSeason":2,"imdbEpisode":1}]}});
        assert!(
            evidence_from_response(
                &source,
                &serde_json::to_vec(&value).unwrap(),
                &source.addon_url
            )
            .is_some()
        );
        value["meta"]["imdb_id"] = json!("tt8");
        assert!(
            evidence_from_response(
                &source,
                &serde_json::to_vec(&value).unwrap(),
                &source.addon_url
            )
            .is_none()
        );
    }

    #[test]
    fn optional_season_chain_uses_an_explicit_relation_and_canonical_native_membership() {
        let owner = "https://owner.example/manifest.json";
        let source = SourceIdentity {
            addon_url: owner.into(),
            media_type: "series".into(),
            id: "mal:62516".into(),
        };
        let value = json!({"meta":{"id":"mal:62516","type":"series","name":"DAN DA DAN Season 3",
            "_kitsuId":"50163","imdb_id":null,"links":[{"category":"prequel","name":"DAN DA DAN Season 2","url":"stremio:///detail/series/kitsu:49425"}],
            "videos":(1..=27).map(|n|json!({"id":format!("kitsu:50163:{n}"),"season":1,"episode":n,"title":format!("Episode {n}")})).collect::<Vec<_>>()}});
        let parent_value = json!({"meta":{"id":"tt30217403","type":"series","name":"DAN DA DAN","imdb_id":"tt30217403","poster":"https://images.example/poster.jpg","releaseInfo":"2024–",
            "videos":[{"id":"tt30217403:0:1","season":0,"episode":1},
                {"id":"kitsu:48269:1","season":1,"episode":1},
                {"id":"kitsu:49425:1","season":2,"episode":1},
                {"id":"tt30217403:3:1","season":3,"episode":1}]}});
        let parse = |v: &serde_json::Value| {
            addons::Addon::parse_meta(&serde_json::to_vec(v).unwrap())
                .unwrap()
                .unwrap()
        };
        let detail = parse(&value);
        let parent = parse(&parent_value);
        let original_videos = serde_json::to_value(&detail.videos).unwrap();
        let relation = season_relation(&source, &detail).unwrap();
        let inferred = infer_season_parent(&source, &detail, &relation, &parent, owner).unwrap();
        assert!(inferred.is_inferred() && inferred.verified());
        assert_eq!(
            inferred.parents,
            [ParentIdentity::ImdbSeries("tt30217403".into())]
        );
        assert_eq!(inferred.season, SeasonMembership::Unknown);
        assert!(inferred.season_partitions.is_empty());
        assert_eq!(inferred.fingerprint(), inferred.clone().fingerprint());
        let navigation = inferred_parent_entry(&inferred).unwrap();
        assert_eq!(navigation.source.id, "tt30217403");
        assert_eq!(navigation.source.addon_url, owner);
        assert_eq!(navigation.preview.name, "DAN DA DAN");
        assert!(navigation.evidence.is_none());
        let mut native = OwnedCatalogEntry::new(owner, detail.preview.clone());
        native.evidence = Some(inferred.clone());
        assert_eq!(
            project_catalog(&[native.clone()], Season)[0].canonical_season,
            None
        );
        assert_eq!(native.preview.id, "mal:62516");
        assert_eq!(
            serde_json::to_value(&detail.videos).unwrap(),
            original_videos,
            "display inference must not alter local season/episode numbering or progress aliases"
        );
        assert_eq!(detail.videos.len(), 27);
        assert_eq!(detail.videos[0].season, Some(1));
        for index in 0..6 {
            let mut bad = value.clone();
            match index {
                0=>bad["meta"].as_object_mut().unwrap().remove("links").map(|_|()).unwrap(),
                1=>bad["meta"]["links"].as_array_mut().unwrap().push(json!({"category":"prequel","name":"DAN DA DAN Season 2","url":"stremio:///detail/series/kitsu:999"})),
                2=>bad["meta"]["links"][0]["url"]=json!("stremio:///detail/series/kitsu:50163"),
                3=>bad["meta"]["links"][0]["name"]=json!("DAN DA DAN Season 1"),
                4=>bad["meta"]["name"]=json!("DAN DA DAN Remake Season 3"),
                _=>bad["meta"]["type"]=json!("movie"),
            }
            assert!(
                season_relation(&source, &parse(&bad)).is_none(),
                "relation rejection {index}"
            );
        }
        // An unlabeled first release is permitted only for explicit Season 2;
        // the parent's native inventory, not the descriptor's default number,
        // must independently place that preceding release in canonical S1.
        let mut second_season = value.clone();
        second_season["meta"]["name"] = json!("DAN DA DAN Season 2");
        second_season["meta"]["links"][0]["name"] = json!("DAN DA DAN");
        second_season["meta"]["links"][0]["url"] = json!("stremio:///detail/series/kitsu:48269");
        let second_detail = parse(&second_season);
        let first_relation = season_relation(&source, &second_detail).unwrap();
        assert!(
            infer_season_parent(&source, &second_detail, &first_relation, &parent, owner)
                .unwrap()
                .is_inferred()
        );
        let mut wrong_first_anchor = parent_value.clone();
        wrong_first_anchor["meta"]["videos"] =
            json!([{"id":"kitsu:48269:1","season":2,"episode":1}]);
        assert!(
            infer_season_parent(
                &source,
                &second_detail,
                &first_relation,
                &parse(&wrong_first_anchor),
                owner
            )
            .is_none()
        );
        let mut long_links = value.clone();
        long_links["meta"]["links"] = json!(vec![value["meta"]["links"][0].clone(); 65]);
        assert!(season_relation(&source, &parse(&long_links)).is_none());
        let mut long_parent = parent_value.clone();
        long_parent["meta"]["links"] = json!(vec![json!({"category":"genre","name":"Genre"}); 65]);
        assert!(
            infer_season_parent(&source, &detail, &relation, &parse(&long_parent), owner).is_none()
        );
        let mut conflicting_source = value.clone();
        conflicting_source["meta"]["imdb_id"] = json!("tt999");
        let conflicting_detail = parse(&conflicting_source);
        let valid_relation = season_relation(&source, &conflicting_detail).unwrap();
        assert!(
            infer_season_parent(
                &source,
                &conflicting_detail,
                &valid_relation,
                &parent,
                owner
            )
            .is_none()
        );
        for malformed in [false, true] {
            let mut bad_source = value.clone();
            bad_source["meta"]["videos"][0]["imdb_id"] = json!("tt999");
            bad_source["meta"]["videos"][0]["imdbSeason"] = json!(3);
            if !malformed {
                bad_source["meta"]["videos"][0]["imdbEpisode"] = json!(1);
            }
            assert!(
                infer_season_parent(&source, &parse(&bad_source), &relation, &parent, owner)
                    .is_none()
            );
        }
        let mut local_numbering = value.clone();
        local_numbering["meta"]["videos"][0]["imdb_id"] = json!("tt30217403");
        local_numbering["meta"]["videos"][0]["imdbSeason"] = json!(3);
        local_numbering["meta"]["videos"][0]["imdbEpisode"] = json!(1);
        assert!(
            infer_season_parent(&source, &parse(&local_numbering), &relation, &parent, owner)
                .unwrap()
                .is_inferred(),
            "valid canonical S3 may coexist with unchanged native local S1"
        );
        let mut native_parent = parent_value.clone();
        native_parent["meta"]["videos"] = json!([{"id":"kitsu:49425:1","season":2,"episode":1}]);
        native_parent["meta"]["imdb_id"] = json!("tt999");
        assert!(
            infer_season_parent(&source, &detail, &relation, &parse(&native_parent), owner)
                .is_none(),
            "native episode IDs cannot mask a contradictory parent header"
        );
        for index in 0..8 {
            let mut bad = parent_value.clone();
            match index {
                0 => bad["meta"]["type"] = json!("movie"),
                1 => bad["meta"]["imdb_id"] = json!("tt999"),
                2 => bad["meta"]["name"] = json!("Unrelated Story"),
                3 => bad["meta"]["videos"][2]["id"] = json!("kitsu:999:1"),
                4 => bad["meta"]["videos"][2]["season"] = json!(1),
                5 => bad["meta"]["videos"][0]["id"] = json!("tt999:0:1"),
                6 => {
                    bad["meta"]["links"] =
                        json!([{"category":"prequel","url":"stremio:///detail/series/kitsu:50163"}])
                }
                _ => bad["meta"]["videos"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"kitsu:49425:2","season":2,"episode":1})),
            }
            assert!(
                infer_season_parent(&source, &detail, &relation, &parse(&bad), owner).is_none(),
                "anchor rejection {index}"
            );
        }
        for index in 0..6 {
            let mut bad = inferred.clone();
            let GroupingProof::InferredSeasonChain {
                provider_id,
                parent_preview,
                ..
            } = &mut bad.proofs[0]
            else {
                unreachable!()
            };
            match index {
                0 => *provider_id = "untrusted".into(),
                1 => parent_preview.source.id = "tt999".into(),
                2 => parent_preview.source.addon_url = "https://other.example".into(),
                3 => parent_preview.source.media_type = "movie".into(),
                4 => parent_preview.name = "x".repeat(257),
                _ => parent_preview.release_info = Some(json!({"invalid":"shape"})),
            }
            assert!(!bad.verified());
            assert!(inferred_parent_entry(&bad).is_none());
        }
        let mut confirmed = entry("mal:62516", &["tt999"], None).evidence.unwrap();
        confirmed.source = source;
        let preferred = merge_grouping_evidence(Some(inferred), Some(confirmed.clone())).unwrap();
        assert_eq!(preferred, confirmed);
        assert!(!preferred.is_inferred());
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

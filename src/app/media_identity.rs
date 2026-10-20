//! Cache-proven episode identity, independent of saved source/playback IDs.
//! Shared episode coverage can join history without equating a season-specific
//! release with the complete series. Nothing here persists or rewrites an ID.
use super::{Video, confirmed_episode_stream_ids};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type TitleKey = (String, String);
pub(super) type EpisodeKey = (String, String);

pub(super) struct TitleInventory {
    pub media_type: String,
    pub series_id: String,
    pub videos: Vec<Video>,
}

#[derive(Default)]
pub(super) struct IdentityIndex {
    /// Each group lists existing native IDs and their proven aliases under
    /// each source parent. A caller resolves these through sync action clocks;
    /// projecting a group must never create a fresh watched/unwatched action.
    pub progress_groups: Vec<Vec<EpisodeKey>>,
    coverage: BTreeMap<TitleKey, BTreeSet<usize>>,
    incomplete: BTreeSet<TitleKey>,
}

struct Node {
    title: TitleKey,
    native: String,
    aliases: BTreeSet<String>,
    inventory: bool,
}

impl IdentityIndex {
    /// Inventories are consumed one at a time; metadata need not stay resident
    /// after its compact identities have been indexed. Movies are excluded:
    /// their playback IDs do not have this episode-identity contract.
    pub fn new(inventories: impl IntoIterator<Item = TitleInventory>) -> Self {
        Self::with_progress(inventories, std::iter::empty())
    }

    /// Received history can retain a different parent even without a saved
    /// title/cache on this device. Its explicit global episode IDs may join a
    /// proven inventory episode, but history alone never establishes complete
    /// title coverage or makes a removed title visible again.
    pub fn with_progress(
        inventories: impl IntoIterator<Item = TitleInventory>,
        progress: impl IntoIterator<Item = EpisodeKey>,
    ) -> Self {
        let mut result = Self::default();
        let mut nodes = Vec::new();
        for inventory in inventories {
            if !matches!(inventory.media_type.as_str(), "series" | "anime")
                || inventory.series_id.is_empty()
            {
                continue;
            }
            let title = (inventory.media_type, inventory.series_id);
            let mut natives = BTreeSet::new();
            for video in inventory.videos {
                if video.id.is_empty() || !natives.insert(video.id.clone()) {
                    result.incomplete.insert(title.clone());
                    continue;
                }
                let mut aliases: BTreeSet<_> =
                    confirmed_episode_stream_ids(&video).into_iter().collect();
                aliases.insert(video.id.clone());
                // Contradictory aliases cannot prove which episode was meant.
                // Keep the native ID itself, but discard that namespace's ties.
                let mut namespaces: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
                for alias in &aliases {
                    if let Some(namespace) = canonical_episode_namespace(alias) {
                        namespaces.entry(namespace).or_default().insert(alias);
                    }
                }
                let conflicting: BTreeSet<_> = namespaces
                    .into_iter()
                    .filter_map(|(namespace, ids)| (ids.len() > 1).then_some(namespace))
                    .collect();
                aliases.retain(|id| {
                    *id == video.id
                        || canonical_episode_namespace(id)
                            .is_none_or(|namespace| !conflicting.contains(namespace))
                });
                nodes.push(Node {
                    title: title.clone(),
                    native: video.id,
                    aliases,
                    inventory: true,
                });
            }
        }
        let mut existing: BTreeSet<_> = nodes
            .iter()
            .map(|node| (node.title.1.clone(), node.native.clone()))
            .collect();
        for (series_id, episode_id) in progress {
            if series_id.is_empty()
                || series_id == episode_id
                || canonical_episode_namespace(&episode_id).is_none()
                || !existing.insert((series_id.clone(), episode_id.clone()))
            {
                continue;
            }
            nodes.push(Node {
                title: ("history".into(), series_id),
                native: episode_id.clone(),
                aliases: BTreeSet::from([episode_id]),
                inventory: false,
            });
        }
        // A provider returning the same alias for two native episodes has not
        // proved either correspondence, even if another source knows the ID.
        let mut claims: BTreeMap<(&TitleKey, &str), BTreeSet<usize>> = BTreeMap::new();
        for (index, node) in nodes.iter().enumerate() {
            for alias in &node.aliases {
                claims
                    .entry((&node.title, alias))
                    .or_default()
                    .insert(index);
            }
        }
        let valid: Vec<BTreeSet<String>> = nodes
            .iter()
            .map(|node| {
                node.aliases
                    .iter()
                    .filter(|alias| claims[&(&node.title, alias.as_str())].len() == 1)
                    .cloned()
                    .collect()
            })
            .collect();
        let mut parents: Vec<_> = (0..nodes.len()).collect();
        let mut global: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, aliases) in valid.iter().enumerate() {
            for alias in aliases {
                if canonical_episode_namespace(alias).is_none() {
                    continue;
                }
                if let Some(&previous) = global.get(alias.as_str()) {
                    let previous = root(&parents, previous);
                    let current = root(&parents, index);
                    parents[current] = previous;
                } else {
                    global.insert(alias, index);
                }
            }
        }
        let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for index in 0..nodes.len() {
            components
                .entry(root(&parents, index))
                .or_default()
                .push(index);
        }
        let mut component_id = 0;
        for members in components.values() {
            let mut namespaces: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
            let mut source_claims: BTreeMap<&TitleKey, usize> = BTreeMap::new();
            for &index in members {
                *source_claims.entry(&nodes[index].title).or_default() += 1;
                for alias in &valid[index] {
                    if let Some(namespace) = canonical_episode_namespace(alias) {
                        namespaces.entry(namespace).or_default().insert(alias);
                    }
                }
            }
            if namespaces.values().any(|ids| ids.len() > 1)
                || source_claims.values().any(|count| *count > 1)
            {
                // A transitive bridge must not join contradictory catalogs or
                // merge two episodes of one source through a third provider.
                for &index in members {
                    if nodes[index].inventory {
                        result
                            .coverage
                            .entry(nodes[index].title.clone())
                            .or_default()
                            .insert(component_id);
                    }
                    component_id += 1;
                }
                continue;
            }
            let mut keys = BTreeSet::new();
            for &index in members {
                let node = &nodes[index];
                if node.inventory {
                    result
                        .coverage
                        .entry(node.title.clone())
                        .or_default()
                        .insert(component_id);
                }
                // A multiply-claimed native ID cannot accept projected aliases.
                if !valid[index].contains(&node.native) {
                    continue;
                }
                for alias in &valid[index] {
                    keys.insert((node.title.1.clone(), alias.clone()));
                }
            }
            if keys.len() > 1 {
                result.progress_groups.push(keys.into_iter().collect());
            }
            component_id += 1;
        }
        result
    }

    /// Only complete one-to-one inventories represent the same saved title.
    /// Overlapping seasons share episode progress but remain separate titles.
    pub fn equivalent_titles(&self, left: &TitleKey, right: &TitleKey) -> bool {
        if self.incomplete.contains(left) || self.incomplete.contains(right) {
            return false;
        }
        self.coverage.get(left).is_some_and(|left_coverage| {
            !left_coverage.is_empty() && self.coverage.get(right) == Some(left_coverage)
        })
    }
}

fn root(parents: &[usize], mut index: usize) -> usize {
    while parents[index] != index {
        index = parents[index];
    }
    index
}

/// These catalog formats are globally scoped. Opaque source IDs can be local
/// aliases, but matching their spelling across titles proves nothing.
fn canonical_episode_namespace(id: &str) -> Option<&'static str> {
    let (parent, episode) = id.rsplit_once(':')?;
    episode.parse::<u32>().ok()?;
    if let Some((title, season)) = parent.rsplit_once(':')
        && season.parse::<u32>().is_ok()
        && matches!(
            nova_providers::ExternalId::parse(title),
            Some(nova_providers::ExternalId::Imdb(_))
        )
    {
        return Some("imdb");
    }
    parent
        .strip_prefix("kitsu:")
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|_| "kitsu")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn video(id: &str, aliases: &[&str]) -> Video {
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

    fn inventory(id: &str, videos: Vec<Video>) -> TitleInventory {
        TitleInventory {
            media_type: "series".into(),
            series_id: id.into(),
            videos,
        }
    }

    fn title(id: &str) -> TitleKey {
        ("series".into(), id.into())
    }

    #[test]
    fn complete_sources_share_title_and_progress_without_rewriting_native_ids() {
        let index = IdentityIndex::new([
            inventory(
                "kitsu:10",
                vec![
                    video("kitsu:10:1", &["tt100:1:1"]),
                    video("kitsu:20:1", &["tt100:2:1"]),
                ],
            ),
            inventory(
                "tt100",
                vec![video("tt100:1:1", &[]), video("tt100:2:1", &[])],
            ),
        ]);
        assert!(index.equivalent_titles(&title("kitsu:10"), &title("tt100")));
        assert_eq!(index.progress_groups.len(), 2);
        assert!(index.progress_groups[0].contains(&("kitsu:10".into(), "kitsu:10:1".into())));
        assert!(index.progress_groups[0].contains(&("tt100".into(), "tt100:1:1".into())));
    }

    #[test]
    fn identical_global_native_episode_ids_share_history_across_saved_parents() {
        // The reported Slime/Tanya duplicates had the same Kitsu episode IDs
        // in both inventories, but watched flags under only the Kitsu parent.
        // Their exact global identity does not require a refreshed alias link.
        let mut native = video("kitsu:10:1", &[]);
        native.extra.clear();
        let index = IdentityIndex::new([
            inventory("kitsu:10", vec![native.clone()]),
            inventory("tt100", vec![native]),
        ]);
        assert!(index.equivalent_titles(&title("kitsu:10"), &title("tt100")));
        assert_eq!(
            index.progress_groups,
            vec![vec![
                ("kitsu:10".into(), "kitsu:10:1".into()),
                ("tt100".into(), "kitsu:10:1".into()),
            ]]
        );
    }

    #[test]
    fn partial_season_shares_only_proven_episodes_and_remains_a_distinct_title() {
        let index = IdentityIndex::new([
            inventory("season", vec![video("native", &["tt100:2:1"])]),
            inventory(
                "tt100",
                vec![video("tt100:1:1", &[]), video("tt100:2:1", &[])],
            ),
        ]);
        assert!(!index.equivalent_titles(&title("season"), &title("tt100")));
        assert_eq!(index.progress_groups.len(), 1);
        assert!(!index.progress_groups[0].contains(&("tt100".into(), "tt100:1:1".into())));
    }

    #[test]
    fn received_global_history_links_without_inventing_complete_title_coverage() {
        let index = IdentityIndex::with_progress(
            [inventory(
                "kitsu:10",
                vec![video("kitsu:10:1", &["tt100:1:1"])],
            )],
            [
                ("tt100".into(), "tt100:1:1".into()),
                ("unrelated".into(), "opaque".into()),
                ("movie".into(), "movie".into()),
            ],
        );
        assert_eq!(index.progress_groups.len(), 1);
        assert!(index.progress_groups[0].contains(&("tt100".into(), "tt100:1:1".into())));
        assert!(index.progress_groups[0].contains(&("kitsu:10".into(), "kitsu:10:1".into())));
        assert!(!index.equivalent_titles(&title("kitsu:10"), &title("tt100")));
    }

    #[test]
    fn stale_ambiguous_opaque_and_movie_ids_do_not_join_sources() {
        let mut stale = video("native", &["tt100:1:1"]);
        stale.extra.insert(
            "novaMetadataRevision".into(),
            json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        for videos in [
            vec![stale],
            vec![video("a", &["tt100:1:1"]), video("b", &["tt100:1:1"])],
            vec![video("episode-1", &[])],
            vec![video("a", &["tt100:1:1", "tt200:1:1"])],
        ] {
            let index = IdentityIndex::new([
                inventory("native", videos),
                inventory("tt100", vec![video("tt100:1:1", &[])]),
            ]);
            assert!(!index.equivalent_titles(&title("native"), &title("tt100")));
            assert!(index.progress_groups.is_empty());
        }
        let index = IdentityIndex::new([
            inventory("one", vec![video("episode-1", &[])]),
            inventory("two", vec![video("episode-1", &[])]),
        ]);
        assert!(!index.equivalent_titles(&title("one"), &title("two")));
        let mut movie = inventory("movie", vec![video("tt100:1:1", &[])]);
        movie.media_type = "movie".into();
        assert!(IdentityIndex::new([movie]).progress_groups.is_empty());
    }

    #[test]
    fn transitive_conflicting_catalogs_cannot_bridge_sources() {
        let index = IdentityIndex::new([
            inventory("one", vec![video("kitsu:10:1", &["tt100:1:1"])]),
            inventory("two", vec![video("kitsu:10:1", &["tt200:1:1"])]),
        ]);
        assert!(!index.equivalent_titles(&title("one"), &title("two")));
        assert!(index.progress_groups.is_empty());
    }
}

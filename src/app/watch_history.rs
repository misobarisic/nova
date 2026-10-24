//! Home's bounded, playback-backed history, independent of saved Library items.
use super::*;

const WATCH_HISTORY_MAX: usize = 100;
pub(super) const MEANINGFUL_PLAYBACK_SECS: f64 = 30.0;
pub(super) const HIDDEN_KEY: &str = "watch_history_hidden:v1";

// JSON tuple identity cannot collide when addon IDs contain separators.
pub(super) fn title_key(type_: &str, id: &str) -> String {
    serde_json::to_string(&(type_, id)).expect("history title identity")
}

#[derive(Default)]
pub(super) struct HistoryIdentities {
    stamp: library::DisplayProofStamp,
    episodes: HashMap<media_identity::EpisodeKey, String>,
    global: HashMap<String, String>,
}

#[derive(Hash, PartialEq, Eq)]
enum HistoryEpisode {
    Proven(String),
    Native(String, String, String),
}

impl HistoryIdentities {
    fn prepare(
        rows: &[HistoryEntry],
        continuing: &[ContinueEntry],
        mut episodes: impl FnMut(&str, &str) -> Vec<Video>,
    ) -> Self {
        let titles = rows
            .iter()
            .map(|row| (row.type_.clone(), row.series_id.clone()))
            .chain(
                continuing
                    .iter()
                    .map(|row| (row.type_.clone(), row.series_id.clone())),
            )
            .filter(|(type_, _)| matches!(type_.as_str(), "series" | "anime"))
            .collect::<HashSet<_>>();
        let relevant = rows
            .iter()
            .filter(|row| matches!(row.type_.as_str(), "series" | "anime"))
            .map(|row| (row.series_id.clone(), row.episode_id.clone()))
            .chain(
                continuing
                    .iter()
                    .filter(|row| matches!(row.type_.as_str(), "series" | "anime"))
                    .map(|row| (row.series_id.clone(), row.episode_id.clone())),
            )
            .collect::<HashSet<_>>();
        let index = media_identity::IdentityIndex::with_progress(
            titles
                .into_iter()
                .map(|(media_type, series_id)| media_identity::TitleInventory {
                    videos: episodes(&media_type, &series_id),
                    media_type,
                    series_id,
                }),
            relevant.iter().cloned(),
        );
        let mut result = Self {
            stamp: library::display_proof_stamp(),
            ..Default::default()
        };
        let mut ambiguous = HashSet::new();
        // Keep only the compact proof needed by current cards. Metadata is
        // decoded on the projection worker, never again for a Hide gesture.
        for group in index.progress_groups {
            if !group.iter().any(|key| relevant.contains(key)) {
                continue;
            }
            let Some(identity) = group
                .iter()
                .map(|(_, episode)| episode)
                .filter(|episode| media_identity::canonical_episode_namespace(episode).is_some())
                .min()
                .cloned()
            else {
                continue;
            };
            for key in group {
                if media_identity::canonical_episode_namespace(&key.1).is_some() {
                    result.global.insert(key.1.clone(), identity.clone());
                }
                if ambiguous.contains(&key) {
                    continue;
                }
                if result
                    .episodes
                    .get(&key)
                    .is_some_and(|old| old != &identity)
                {
                    // Progress keys omit the media type. Two typed inventories
                    // can reuse an opaque ID for different episodes; neither
                    // mapping may win merely because its HashSet iterated last.
                    result.episodes.remove(&key);
                    ambiguous.insert(key);
                } else {
                    result.episodes.insert(key, identity.clone());
                }
            }
        }
        result
    }

    fn episode(&self, type_: &str, parent: &str, episode: &str) -> HistoryEpisode {
        if matches!(type_, "series" | "anime") {
            if self.stamp == library::display_proof_stamp()
                && let Some(identity) = self
                    .episodes
                    .get(&(parent.into(), episode.into()))
                    .or_else(|| self.global.get(episode))
            {
                return HistoryEpisode::Proven(identity.clone());
            }
            // A globally scoped episode ID proves itself even before the
            // first worker result. Opaque IDs and movie IDs never do.
            if media_identity::canonical_episode_namespace(episode).is_some() {
                return HistoryEpisode::Proven(episode.into());
            }
        }
        HistoryEpisode::Native(type_.into(), parent.into(), episode.into())
    }
}

fn filter_history(
    mut rows: Vec<HistoryEntry>,
    visible: &[ContinueEntry],
    hidden: &HashMap<String, u64>,
    identities: &HistoryIdentities,
    resume_only: bool,
) -> Vec<HistoryEntry> {
    let visible_episodes = visible
        .iter()
        .map(|row| identities.episode(&row.type_, &row.series_id, &row.episode_id))
        .collect::<HashSet<_>>();
    let visible_titles = visible
        .iter()
        .map(|row| (&row.type_, &row.series_id))
        .collect::<HashSet<_>>();
    let mut hidden_episodes = HashMap::<HistoryEpisode, u64>::new();
    for row in &rows {
        if let Some(at) = hidden.get(&title_key(&row.type_, &row.series_id)) {
            hidden_episodes
                .entry(identities.episode(&row.type_, &row.series_id, &row.episode_id))
                .and_modify(|old| *old = (*old).max(*at))
                .or_insert(*at);
        }
    }
    let mut seen = HashSet::new();
    // Choose the newest alias before checking eligibility: a completed or
    // brief latest run must not resurrect an older resumable projection.
    rows.retain(|row| seen.insert(identities.episode(&row.type_, &row.series_id, &row.episode_id)));
    rows.retain(|row| {
        let episode = identities.episode(&row.type_, &row.series_id, &row.episode_id);
        row.meaningful
            && (!resume_only || row.can_resume())
            && (!resume_only
                || (!visible_titles.contains(&(&row.type_, &row.series_id))
                    && !visible_episodes.contains(&episode)))
            && hidden
                .get(&title_key(&row.type_, &row.series_id))
                .into_iter()
                .chain(hidden_episodes.get(&episode))
                .all(|at| row.played_at_secs > *at)
    });
    rows
}

pub(super) fn hidden_title_keys(state: &Shared, selected: &HistoryEntry) -> HashSet<String> {
    let identity = state.history_identities.episode(
        &selected.type_,
        &selected.series_id,
        &selected.episode_id,
    );
    history_candidates(&state.progress, &state.entries, false)
        .iter()
        .filter(|row| {
            state
                .history_identities
                .episode(&row.type_, &row.series_id, &row.episode_id)
                == identity
        })
        .map(|row| title_key(&row.type_, &row.series_id))
        .chain(std::iter::once(title_key(
            &selected.type_,
            &selected.series_id,
        )))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct HistoryEntry {
    pub(super) series_id: String,
    pub(super) type_: String,
    pub(super) episode_id: String,
    pub(super) title: String,
    pub(super) poster_url: String,
    pub(super) episode_label: String,
    pub(super) season: Option<u32>,
    pub(super) episode: Option<u32>,
    pub(super) played_at_secs: u64,
    pub(super) position_secs: f64,
    pub(super) duration_secs: f64,
    pub(super) resumable: bool,
    pub(super) meaningful: bool,
    pub(super) rewatch: bool,
}

impl HistoryEntry {
    pub(super) fn can_resume(&self) -> bool {
        self.resumable
    }

    pub(super) fn action_label(&self) -> &'static str {
        text::tr(if !self.can_resume() {
            "Play again"
        } else if self.rewatch {
            "Rewatch"
        } else {
            "Resume"
        })
    }

    pub(super) fn caption(&self) -> String {
        if self.type_ == "movie" {
            return String::new();
        }
        let identifier = match (self.season, self.episode) {
            (Some(season), Some(episode)) => format!("S{season} E{episode}"),
            (Some(season), None) => format!("S{season}"),
            (None, Some(episode)) => format!("E{episode}"),
            (None, None) => String::new(),
        };
        if identifier.is_empty() && self.episode_label.trim().is_empty() {
            text::tr("Episode").to_string()
        } else {
            text::stream_episode_context(&identifier, &self.episode_label)
        }
    }
}

#[cfg(test)]
fn project_watch_history(
    progress: &HashMap<String, EpisodeProgress>,
    library: &[LibraryEntry],
    visible_continue: &HashSet<(String, String)>,
) -> Vec<HistoryEntry> {
    let mut rows = project_history(progress, library, visible_continue, true);
    rows.truncate(WATCH_HISTORY_MAX);
    rows
}

#[cfg(test)]
fn project_history(
    progress: &HashMap<String, EpisodeProgress>,
    library: &[LibraryEntry],
    visible_continue: &HashSet<(String, String)>,
    resume_only: bool,
) -> Vec<HistoryEntry> {
    let visible = visible_continue
        .iter()
        .map(|(type_, series_id)| ContinueEntry {
            type_: type_.clone(),
            series_id: series_id.clone(),
            episode_id: String::new(),
            updated_at_secs: 0,
        })
        .collect::<Vec<_>>();
    filter_history(
        history_candidates(progress, library, resume_only),
        &visible,
        &HashMap::new(),
        &HistoryIdentities::default(),
        resume_only,
    )
}

fn history_candidates(
    progress: &HashMap<String, EpisodeProgress>,
    library: &[LibraryEntry],
    resume_only: bool,
) -> Vec<HistoryEntry> {
    let mut rows = Vec::new();
    for record in progress.values().filter(|record| {
        record.play_count > 0 && record.position_secs.is_finite() && record.position_secs > 0.0
    }) {
        let snapshot = record.history.as_ref();
        let legacy = || {
            let mut matches = library.iter().filter(|entry| entry.id == record.series_id);
            let first = matches.next()?;
            // Old progress has no media type. Never infer it from the title or
            // arbitrarily pick one of two distinct typed Library identities.
            matches.next().is_none().then_some(first)
        };
        let type_ = snapshot
            .map(|snapshot| snapshot.type_.as_str())
            .filter(|type_| !type_.is_empty())
            .or_else(|| legacy().map(|entry| entry.type_.as_str()));
        let Some(type_) = type_ else { continue };
        let saved = library
            .iter()
            .find(|entry| entry.type_ == type_ && entry.id == record.series_id);
        let title = snapshot
            .map(|snapshot| snapshot.title.as_str())
            .filter(|title| !title.trim().is_empty())
            .or_else(|| saved.map(|entry| entry.name.as_str()))
            .unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let poster_url = snapshot
            .map(|snapshot| snapshot.poster_url.as_str())
            .filter(|url| !url.trim().is_empty())
            .or_else(|| saved.map(|entry| entry.poster_url.as_str()))
            .unwrap_or_default();
        rows.push(HistoryEntry {
            series_id: record.series_id.clone(),
            type_: type_.to_owned(),
            episode_id: record.episode_id.clone(),
            title: title.to_owned(),
            poster_url: poster_url.to_owned(),
            episode_label: snapshot
                .map(|snapshot| snapshot.episode_label.clone())
                .unwrap_or_default(),
            season: snapshot.and_then(|snapshot| snapshot.season),
            episode: snapshot.and_then(|snapshot| snapshot.episode),
            // Metadata edits and mark-watched operations update the progress
            // clock; only actual playback updates the dedicated history clock.
            played_at_secs: snapshot
                .map_or(record.updated_at_secs, |snapshot| snapshot.played_at_secs),
            position_secs: record.position_secs,
            duration_secs: record.duration_secs,
            // A sticky watched flag is resumable only for an actual rewatch.
            // Legacy records lack measured play time; keep their valid resumes.
            resumable: playback_can_resume(record)
                && (!resume_only
                    || saved.is_none_or(|entry| entry.watch_status == WatchStatus::Auto)),
            rewatch: snapshot.is_some_and(|history| history.rewatch),
            // A brief new rewatch must not erase the archive entry earned by
            // an earlier completion. Home still requires this run to qualify.
            meaningful: (!resume_only && record.watched)
                || snapshot
                    .and_then(|history| history.engaged_secs)
                    .is_none_or(|seconds| {
                        seconds.is_finite() && seconds >= MEANINGFUL_PLAYBACK_SECS
                    }),
        });
    }
    rows.sort_by(|a, b| {
        b.played_at_secs
            .cmp(&a.played_at_secs)
            .then_with(|| a.type_.cmp(&b.type_))
            .then_with(|| a.series_id.cmp(&b.series_id))
            .then_with(|| a.episode_id.cmp(&b.episode_id))
    });
    let mut seen = HashSet::new();
    // Select the latest episode before rejecting completed or brief openings:
    // finishing E4 must never resurrect a stale unfinished E2.
    rows.retain(|row| seen.insert((row.type_.clone(), row.series_id.clone())));
    rows
}

fn visible_continue_entries(state: &Shared, visible_ids: &HashSet<String>) -> Vec<ContinueEntry> {
    if !state
        .cache_settings
        .home_row_enabled(&HomeRowSource::ContinueWatching)
    {
        return Vec::new();
    }
    state
        .continue_list
        .iter()
        .filter(|entry| visible_ids.contains(&entry.series_id))
        .filter(|entry| {
            state
                .entries
                .iter()
                .any(|saved| saved.id == entry.series_id && saved.type_ == entry.type_)
        })
        .cloned()
        .collect()
}

impl Bridge {
    pub(super) fn current_watch_history_entries(&self) -> Vec<HistoryEntry> {
        self.current_history_entries(true)
    }

    fn current_history_entries(&self, resume_only: bool) -> Vec<HistoryEntry> {
        let visible_ids: HashSet<String> = self
            .app()
            .map(|app| {
                app.get_home_continue()
                    .iter()
                    .map(|row| row.id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let state = self.shared.lock().unwrap();
        if resume_only
            && !state
                .cache_settings
                .home_row_enabled(&HomeRowSource::WatchHistory)
        {
            return Vec::new();
        }
        let visible_continue = if resume_only {
            visible_continue_entries(&state, &visible_ids)
        } else {
            Vec::new()
        };
        let mut rows = history_candidates(&state.progress, &state.entries, resume_only);
        let identities = state.history_identities.clone();
        drop(state);
        let hidden: HashMap<String, u64> = read_json(HIDDEN_KEY).unwrap_or_default();
        rows.retain(|row| {
            profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &row.type_,
                &row.series_id,
            )
        });
        rows = filter_history(rows, &visible_continue, &hidden, &identities, resume_only);
        rows.truncate(WATCH_HISTORY_MAX);
        // Legacy playback can borrow existing episode labels offline. This is
        // cache-only: opening Home never fetches entire metadata for history.
        for row in &mut rows {
            // Artwork repair and ordinary Detail visits share the typed
            // header cache. Prefer its current URL over an older playback
            // snapshot without mutating the durable history clock.
            if let Some(header) = read_meta_header_for(&row.type_, &row.series_id)
                && !header.poster_url.trim().is_empty()
            {
                row.poster_url = header.poster_url;
            }
            if row.type_ != "movie"
                && row.episode_label.is_empty()
                && let Some(video) = read_episodes_cache_for(&row.type_, &row.series_id)
                    .and_then(|videos| videos.into_iter().find(|video| video.id == row.episode_id))
            {
                row.episode_label = episode_context_label(&video);
                row.season = row.season.or(video.season);
                row.episode = row.episode.or(video.episode_number());
            }
        }
        let state = self.shared.lock().unwrap();
        for row in &mut rows {
            if spoiler::hidden(&state, &row.type_, &row.series_id, &row.episode_id) {
                row.episode_label.clear();
            }
        }
        rows
    }

    pub(super) fn rebuild_watch_history_list(&self) {
        let entries = self.current_watch_history_entries();
        let archive = self.current_history_entries(false);
        let mut state = self.shared.lock().unwrap();
        state.home_history_list = entries;
        state.home_history_archive = archive;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias_video(id: &str, alias: &str) -> Video {
        serde_json::from_value(serde_json::json!({"id":id,"novaStreamIds":[alias],
            "novaMetadataRevision":nova_providers::metadata_revision()}))
        .unwrap()
    }

    #[test]
    fn canonical_parent_projections_collapse_before_the_history_limit() {
        let progress = map((0..101).flat_map(|n| {
            let episode = format!("tt{}:1:1", 100000 + n);
            [
                played("series", &format!("native-{n}"), &episode, n + 1),
                played("series", &format!("mirror-{n}"), &episode, n + 1),
            ]
        }));
        let rows = project_watch_history(&progress, &[], &HashSet::new());
        assert_eq!(rows.len(), WATCH_HISTORY_MAX);
        assert_eq!(
            rows.iter()
                .map(|row| &row.episode_id)
                .collect::<HashSet<_>>()
                .len(),
            WATCH_HISTORY_MAX
        );
        assert_eq!(
            project_history(&progress, &[], &HashSet::new(), false).len(),
            101
        );
        assert_eq!(
            progress.len(),
            202,
            "display collapse must preserve progress views"
        );
    }

    #[test]
    fn prepared_alias_history_shares_hide_and_visible_continue_identity() {
        let mut state = Shared {
            progress: map([
                played("series", "native", "opaque-e1", 100),
                played("series", "other", "kitsu:10:1", 100),
            ]),
            ..Default::default()
        };
        let original = state.progress.clone();
        let metadata = progress_projection::Metadata::new(
            |_, id| {
                (
                    Some(vec![alias_video(
                        if id == "native" {
                            "opaque-e1"
                        } else {
                            "kitsu:10:1"
                        },
                        "tt100:1:1",
                    )]),
                    None,
                )
            },
            || true,
        );
        prepare_progress_history(&mut state, &metadata, &HashMap::new());
        assert_eq!(state.home_history_list.len(), 1);
        assert_eq!(state.home_history_archive.len(), 1);
        let selected = state.home_history_list[0].clone();
        let hidden_keys = hidden_title_keys(&state, &selected);
        assert_eq!(
            hidden_keys,
            HashSet::from([title_key("series", "native"), title_key("series", "other")])
        );
        let rows = history_candidates(&state.progress, &state.entries, true);
        // A legacy dismissal written under just the other raw title must
        // suppress the currently selected representative too.
        let hidden = HashMap::from([(title_key("series", "other"), 150)]);
        assert!(
            filter_history(rows.clone(), &[], &hidden, &state.history_identities, true).is_empty()
        );
        let visible = [ContinueEntry {
            type_: "series".into(),
            series_id: "other".into(),
            episode_id: "kitsu:10:1".into(),
            updated_at_secs: 100,
        }];
        assert!(
            filter_history(
                rows.clone(),
                &visible,
                &HashMap::new(),
                &state.history_identities,
                true
            )
            .is_empty()
        );
        assert_eq!(
            filter_history(
                rows.clone(),
                &visible,
                &HashMap::new(),
                &state.history_identities,
                false
            )
            .len(),
            1
        );
        let mut newer = rows;
        newer[0].played_at_secs = 200;
        assert_eq!(
            filter_history(newer, &[], &hidden, &state.history_identities, true).len(),
            1
        );
        assert_eq!(state.progress, original);
        prepare_progress_history(&mut state, &metadata, &hidden);
        assert!(state.home_history_list.is_empty());
        assert!(state.home_history_archive.is_empty());
    }

    #[test]
    fn history_keeps_distinct_episodes_movies_and_unproven_or_stale_ids() {
        let progress = map([
            played("series", "native", "opaque-e1", 100),
            played("series", "canonical", "tt100:1:1", 100),
            played("series", "sequel", "tt100:2:1", 100),
            played("movie", "film-a", "tt100:1:1", 100),
            played("movie", "film-b", "tt100:1:1", 100),
            played("series", "unrelated", "opaque-e1", 100),
        ]);
        let rows = history_candidates(&progress, &[], true);
        let mut stale = alias_video("opaque-e1", "tt100:1:1");
        stale.extra.insert(
            "novaMetadataRevision".into(),
            serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        let identities = HistoryIdentities::prepare(&rows, &[], |_, id| {
            if id == "native" {
                vec![stale.clone()]
            } else {
                vec![]
            }
        });
        assert_eq!(
            filter_history(rows.clone(), &[], &HashMap::new(), &identities, true).len(),
            6
        );
        let ambiguous = HistoryIdentities::prepare(&rows, &[], |_, id| {
            if id == "native" {
                vec![
                    alias_video("opaque-e1", "tt100:1:1"),
                    alias_video("opaque-e2", "tt100:1:1"),
                ]
            } else {
                vec![]
            }
        });
        assert_eq!(
            filter_history(rows, &[], &HashMap::new(), &ambiguous, true).len(),
            6
        );
    }

    #[test]
    fn history_proof_rejects_conflicting_typed_inventory_keys() {
        let progress = map([
            played("series", "same", "opaque", 100),
            played("series", "other", "tt200:1:1", 100),
        ]);
        let rows = history_candidates(&progress, &[], true);
        let visible = [ContinueEntry {
            type_: "anime".into(),
            series_id: "same".into(),
            episode_id: "opaque".into(),
            updated_at_secs: 100,
        }];
        let identities = HistoryIdentities::prepare(&rows, &visible, |type_, id| {
            if id == "same" {
                vec![alias_video(
                    "opaque",
                    if type_ == "series" {
                        "tt100:1:1"
                    } else {
                        "tt200:1:1"
                    },
                )]
            } else {
                Vec::new()
            }
        });
        assert!(matches!(
            identities.episode("series", "same", "opaque"),
            HistoryEpisode::Native(..)
        ));
        assert_eq!(
            filter_history(rows, &visible, &HashMap::new(), &identities, true).len(),
            2
        );
    }

    #[test]
    fn latest_completed_alias_does_not_resurrect_an_older_resume() {
        let mut newest = played("series", "new", "tt100:1:1", 200);
        newest.position_secs = 1200.0;
        newest.watched = true;
        let progress = map([played("series", "old", "tt100:1:1", 100), newest]);
        assert!(project_watch_history(&progress, &[], &HashSet::new()).is_empty());
        let archive = project_history(&progress, &[], &HashSet::new(), false);
        assert_eq!(archive.len(), 1);
        assert_eq!(archive[0].series_id, "new");
    }

    fn played(type_: &str, id: &str, episode: &str, at: u64) -> EpisodeProgress {
        serde_json::from_value(serde_json::json!({
            "series_id": id, "episode_id": episode, "position_secs": 60.0,
            "duration_secs": 1200.0, "watched": false, "play_count": 1,
            "updated_at_secs": at,
            "history": { "type_": type_, "title": "Played title", "poster_url": "poster",
                "episode_label": "Pilot", "season": 1, "episode": 1, "played_at_secs": at }
        }))
        .unwrap()
    }

    fn library(type_: &str, id: &str) -> LibraryEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "type_": type_, "name": "Saved title", "categories": []
        }))
        .unwrap()
    }

    fn map(records: impl IntoIterator<Item = EpisodeProgress>) -> HashMap<String, EpisodeProgress> {
        records
            .into_iter()
            .map(|record| {
                (
                    progress_map_key(&record.series_id, &record.episode_id),
                    record,
                )
            })
            .collect()
    }

    #[test]
    fn played_items_survive_outside_library_and_unplayed_manual_marks_do_not_appear() {
        let mut manual = played("series", "manual", "e1", 100);
        manual.play_count = 0;
        let rows = project_watch_history(
            &map([played("movie", "outside", "outside", 50), manual]),
            &[],
            &HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].series_id, "outside");
        assert_eq!(rows[0].title, "Played title");
    }

    #[test]
    fn latest_partial_rewatch_keeps_its_episode_and_resume_despite_sticky_watched() {
        let mut rewatch = played("series", "show", "e1", 30);
        rewatch.position_secs = 60.0;
        rewatch.play_count = 2;
        rewatch.watched = true;
        rewatch.history.as_mut().unwrap().rewatch = true;
        let rows = project_watch_history(
            &map([played("series", "show", "e2", 20), rewatch]),
            &[],
            &HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].episode_id, "e1");
        assert!(rows[0].can_resume());
    }

    #[test]
    fn typed_dedup_excludes_only_visible_continue_titles() {
        let progress = map([
            played("series", "same", "e1", 30),
            played("series", "same", "e2", 40),
            played("movie", "same", "same", 20),
            played("series", "hidden", "e1", 10),
        ]);
        let visible = HashSet::from([("series".into(), "same".into())]);
        let rows = project_watch_history(&progress, &[], &visible);
        assert_eq!(
            rows.iter()
                .map(|row| (row.type_.as_str(), row.series_id.as_str()))
                .collect::<Vec<_>>(),
            vec![("movie", "same"), ("series", "hidden")]
        );
        let rows = project_watch_history(&progress, &[], &HashSet::new());
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].episode_id, "e2");
    }

    #[test]
    fn disabled_or_unrendered_continue_does_not_hide_history() {
        let mut state = Shared::default();
        state.entries = vec![library("series", "show")];
        state.continue_list = vec![ContinueEntry {
            series_id: "show".into(),
            type_: "series".into(),
            ..Default::default()
        }];
        assert!(visible_continue_entries(&state, &HashSet::new()).is_empty());
        state.cache_settings.home_rows = Some(vec![nova_config::HomeRow {
            source: HomeRowSource::ContinueWatching,
            enabled: false,
        }]);
        let visible = visible_continue_entries(&state, &HashSet::from(["show".into()]));
        assert!(visible.is_empty());
        assert_eq!(
            filter_history(
                history_candidates(
                    &map([played("series", "show", "e1", 10)]),
                    &state.entries,
                    true
                ),
                &visible,
                &HashMap::new(),
                &state.history_identities,
                true,
            )
            .len(),
            1
        );
        state.cache_settings.home_rows.as_mut().unwrap()[0].enabled = true;
        assert_eq!(
            visible_continue_entries(&state, &HashSet::from(["show".into()])).len(),
            1
        );
    }

    #[test]
    fn manual_progress_or_metadata_updates_do_not_reorder_playback() {
        let mut older = played("series", "older", "e1", 10);
        older.updated_at_secs = 1000;
        older.history.as_mut().unwrap().title = "Renamed".into();
        let rows = project_watch_history(
            &map([older, played("series", "newer", "e1", 20)]),
            &[],
            &HashSet::new(),
        );
        assert_eq!(rows[0].series_id, "newer");
        assert_eq!(rows[1].played_at_secs, 10);
    }

    #[test]
    fn order_is_bounded_and_ties_are_deterministic() {
        let records =
            (0..110).map(|index| played("series", &format!("show-{index:03}"), "e1", index));
        let rows = project_watch_history(&map(records), &[], &HashSet::new());
        assert_eq!(rows.len(), WATCH_HISTORY_MAX);
        assert_eq!(rows[0].series_id, "show-109");
        assert_eq!(rows.last().unwrap().series_id, "show-010");
        let ties = map([
            played("series", "show", "z", 10),
            played("series", "show", "a", 10),
        ]);
        assert_eq!(
            project_watch_history(&ties, &[], &HashSet::new())[0].episode_id,
            "a"
        );
    }

    #[test]
    fn completion_and_brief_openings_do_not_resurrect_older_episodes() {
        let older = played("series", "show", "e1", 10);
        let mut latest = played("series", "show", "e2", 20);
        latest.position_secs = latest.duration_secs;
        latest.watched = true;
        let records = map([older.clone(), latest.clone()]);
        assert!(project_watch_history(&records, &[], &HashSet::new()).is_empty());
        assert_eq!(records.len(), 2, "projection preserves recorded history");
        let archive = project_history(&records, &[], &HashSet::new(), false);
        assert_eq!(archive.len(), 1);
        assert_eq!(archive[0].episode_id, "e2");
        assert!(!archive[0].can_resume());
        let mut brief_rewatch = latest.clone();
        brief_rewatch.position_secs = 60.0;
        brief_rewatch.history.as_mut().unwrap().rewatch = true;
        brief_rewatch.history.as_mut().unwrap().engaged_secs = Some(5.0);
        let replay_records = map([brief_rewatch]);
        assert!(project_watch_history(&replay_records, &[], &HashSet::new()).is_empty());
        assert_eq!(
            project_history(&replay_records, &[], &HashSet::new(), false).len(),
            1,
            "a brief rewatch retains an earlier completion in the explicit archive"
        );
        latest.watched = false;
        latest.position_secs = 300.0; // a seek is not 30 seconds of viewing
        latest.history.as_mut().unwrap().engaged_secs = Some(5.0);
        assert!(
            project_watch_history(&map([older, latest.clone()]), &[], &HashSet::new()).is_empty()
        );
        latest.history.as_mut().unwrap().engaged_secs = Some(30.0);
        assert_eq!(
            project_watch_history(&map([latest]), &[], &HashSet::new()).len(),
            1
        );
    }

    #[test]
    fn unknown_duration_and_partial_rewatch_resume_but_manual_completion_does_not() {
        let mut record = played("series", "show", "e1", 10);
        record.duration_secs = 0.0;
        assert_eq!(
            project_watch_history(&map([record.clone()]), &[], &HashSet::new()).len(),
            1
        );
        record.watched = true;
        assert!(project_watch_history(&map([record.clone()]), &[], &HashSet::new()).is_empty());
        record.history.as_mut().unwrap().rewatch = true;
        assert_eq!(
            project_watch_history(&map([record.clone()]), &[], &HashSet::new()).len(),
            1
        );
        let mut records = map([record]);
        Bridge::set_episode_watched_locked(&mut records, "show", "e1", true);
        assert!(project_watch_history(&records, &[], &HashSet::new()).is_empty());
    }

    #[test]
    fn legacy_records_require_unambiguous_library_identity() {
        let mut legacy = played("series", "show", "e1", 10);
        legacy.history = None;
        let progress = map([legacy]);
        assert!(project_watch_history(&progress, &[], &HashSet::new()).is_empty());
        let saved = vec![library("series", "show")];
        assert_eq!(
            project_watch_history(&progress, &saved, &HashSet::new())[0].title,
            "Saved title"
        );
        assert!(
            project_watch_history(
                &progress,
                &[library("series", "show"), library("movie", "show")],
                &HashSet::new()
            )
            .is_empty()
        );
    }
}

/// History joins share the worker's typed metadata snapshot. Publication can
/// then rebuild catalog models without reopening any episode/header cache.
pub(super) fn prepare_progress_history(
    state: &mut Shared,
    metadata: &super::progress_projection::Metadata<'_>,
    hidden: &HashMap<String, u64>,
) {
    let candidates = history_candidates(&state.progress, &state.entries, false);
    let identities = Arc::new(HistoryIdentities::prepare(
        &candidates,
        &state.continue_list,
        |type_, id| metadata.episodes(type_, id).as_ref().clone(),
    ));
    let visible = if state
        .cache_settings
        .home_row_enabled(&HomeRowSource::ContinueWatching)
    {
        state.continue_list.clone()
    } else {
        Vec::new()
    };
    let prepare = |resume_only| {
        let mut rows = if resume_only
            && !state
                .cache_settings
                .home_row_enabled(&HomeRowSource::WatchHistory)
        {
            Vec::new()
        } else {
            history_candidates(&state.progress, &state.entries, resume_only)
        };
        rows.retain(|row| {
            profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &row.type_,
                &row.series_id,
            )
        });
        rows = filter_history(rows, &visible, hidden, &identities, resume_only);
        rows.truncate(WATCH_HISTORY_MAX);
        for row in &mut rows {
            let cached = metadata.title(&row.type_, &row.series_id);
            if let Some(header) = &cached.header
                && !header.poster_url.trim().is_empty()
            {
                row.poster_url.clone_from(&header.poster_url);
            }
            if row.type_ != "movie"
                && row.episode_label.is_empty()
                && let Some(video) = cached
                    .episodes
                    .as_deref()
                    .into_iter()
                    .flatten()
                    .find(|v| v.id == row.episode_id)
            {
                row.episode_label = episode_context_label(video);
                row.season = row.season.or(video.season);
                row.episode = row.episode.or(video.episode_number());
            }
        }
        for row in &mut rows {
            if spoiler::hidden(state, &row.type_, &row.series_id, &row.episode_id) {
                row.episode_label.clear();
            }
        }
        rows
    };
    let entries = prepare(true);
    let archive = prepare(false);
    state.home_history_list = entries;
    state.home_history_archive = archive;
    state.history_identities = identities;
}

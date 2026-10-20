//! Home's bounded, playback-backed history, independent of saved Library items.
use super::*;

const WATCH_HISTORY_MAX: usize = 100;
pub(super) const MEANINGFUL_PLAYBACK_SECS: f64 = 30.0;
pub(super) const HIDDEN_KEY: &str = "watch_history_hidden:v1";

// JSON tuple identity cannot collide when addon IDs contain separators.
pub(super) fn title_key(type_: &str, id: &str) -> String {
    serde_json::to_string(&(type_, id)).expect("history title identity")
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

fn project_history(
    progress: &HashMap<String, EpisodeProgress>,
    library: &[LibraryEntry],
    visible_continue: &HashSet<(String, String)>,
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
        if visible_continue.contains(&(type_.to_owned(), record.series_id.clone())) {
            continue;
        }
        let saved = library
            .iter()
            .find(|entry| entry.type_ == type_ && entry.id == record.series_id);
        let title = snapshot
            .map(|snapshot| snapshot.title.as_str())
            .filter(|title| !title.trim().is_empty())
            .or_else(|| saved.map(|entry| entry.name.as_str()))
            .unwrap_or_default();
        if title.trim().is_empty()
            || (resume_only && saved.is_some_and(|entry| entry.watch_status != WatchStatus::Auto))
        {
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
            resumable: playback_can_resume(record),
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
    rows.retain(|row| row.meaningful && (!resume_only || row.can_resume()));
    rows
}

fn visible_continue_titles(
    state: &Shared,
    visible_ids: &HashSet<String>,
) -> HashSet<(String, String)> {
    if !state
        .cache_settings
        .home_row_enabled(&HomeRowSource::ContinueWatching)
    {
        return HashSet::new();
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
        .map(|entry| (entry.type_.clone(), entry.series_id.clone()))
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
            visible_continue_titles(&state, &visible_ids)
        } else {
            HashSet::new()
        };
        let mut rows = project_history(
            &state.progress,
            &state.entries,
            &visible_continue,
            resume_only,
        );
        drop(state);
        let hidden: HashMap<String, u64> = read_json(HIDDEN_KEY).unwrap_or_default();
        rows.retain(|row| {
            hidden
                .get(&title_key(&row.type_, &row.series_id))
                .is_none_or(|hidden_at| row.played_at_secs > *hidden_at)
        });
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
        assert!(visible_continue_titles(&state, &HashSet::new()).is_empty());
        state.cache_settings.home_rows = Some(vec![nova_config::HomeRow {
            source: HomeRowSource::ContinueWatching,
            enabled: false,
        }]);
        let visible = visible_continue_titles(&state, &HashSet::from(["show".into()]));
        assert!(visible.is_empty());
        assert_eq!(
            project_watch_history(
                &map([played("series", "show", "e1", 10)]),
                &state.entries,
                &visible
            )
            .len(),
            1
        );
        state.cache_settings.home_rows.as_mut().unwrap()[0].enabled = true;
        assert_eq!(
            visible_continue_titles(&state, &HashSet::from(["show".into()])).len(),
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
    let visible = if state
        .cache_settings
        .home_row_enabled(&HomeRowSource::ContinueWatching)
    {
        state
            .continue_list
            .iter()
            .map(|c| (c.type_.clone(), c.series_id.clone()))
            .collect()
    } else {
        HashSet::new()
    };
    let empty = HashSet::new();
    let prepare = |resume_only| {
        let mut rows = if resume_only
            && !state
                .cache_settings
                .home_row_enabled(&HomeRowSource::WatchHistory)
        {
            Vec::new()
        } else {
            project_history(
                &state.progress,
                &state.entries,
                if resume_only { &visible } else { &empty },
                resume_only,
            )
        };
        rows.retain(|row| {
            hidden
                .get(&title_key(&row.type_, &row.series_id))
                .is_none_or(|at| row.played_at_secs > *at)
        });
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
        rows
    };
    let entries = prepare(true);
    let archive = prepare(false);
    state.home_history_list = entries;
    state.home_history_archive = archive;
}

//! Home's bounded, playback-backed history, independent of saved Library items.
use super::*;

const WATCH_HISTORY_MAX: usize = 100;

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
}

impl HistoryEntry {
    pub(super) fn can_resume(&self) -> bool {
        // Watched is intentionally absent: it is sticky across partial rewatches.
        self.position_secs.is_finite()
            && self.duration_secs.is_finite()
            && resumable_position(self.position_secs, self.duration_secs, false)
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

fn project_watch_history(
    progress: &HashMap<String, EpisodeProgress>,
    library: &[LibraryEntry],
    visible_continue: &HashSet<(String, String)>,
) -> Vec<HistoryEntry> {
    let mut rows = Vec::new();
    for record in progress.values().filter(|record| record.play_count > 0) {
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
    rows.retain(|row| seen.insert((row.type_.clone(), row.series_id.clone())));
    rows.truncate(WATCH_HISTORY_MAX);
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
        if !state
            .cache_settings
            .home_row_enabled(&HomeRowSource::WatchHistory)
        {
            return Vec::new();
        }
        let visible_continue = visible_continue_titles(&state, &visible_ids);
        let mut rows = project_watch_history(&state.progress, &state.entries, &visible_continue);
        drop(state);
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
        self.shared.lock().unwrap().home_history_list = entries;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn played(type_: &str, id: &str, episode: &str, at: u64) -> EpisodeProgress {
        serde_json::from_value(serde_json::json!({
            "series_id": id, "episode_id": episode, "position_secs": 1200.0,
            "duration_secs": 1200.0, "watched": true, "play_count": 1,
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

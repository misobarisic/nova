//! Viewing activity survives metadata refreshes: availability is not playback.
use super::*;

pub(crate) const VIEWING_ACTIVITY_KEY: &str = "viewing_activity:v1";
pub(crate) const NEW_EPISODES_FILTER: &str = "builtin:new_episodes";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ViewingPhase {
    Active,
    AwaitingReturn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ViewingActivity {
    pub phase: ViewingPhase,
}

pub(crate) fn read_viewing_activity() -> HashMap<String, ViewingActivity> {
    read_json(VIEWING_ACTIVITY_KEY).unwrap_or_default()
}

/// Only scheduled releases surface automatically. Dateless extras still
/// resume when explicitly started, and a wholly watched dateless list completes.
pub(crate) fn available_completed(
    id: &str,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    today: i64,
) -> bool {
    if episodes.is_empty() {
        return false;
    }
    let watched = |v: &Video| {
        progress
            .get(&progress_map_key(id, &v.id))
            .is_some_and(|p| p.watched)
    };
    let available: Vec<_> = episodes
        .iter()
        .filter(|v| episode_has_air_date(v) && episode_is_out(v, today))
        .collect();
    let unfinished = episodes.iter().any(|v| {
        progress
            .get(&progress_map_key(id, &v.id))
            .is_some_and(|p| resumable_position(p.position_secs, p.duration_secs, p.watched))
    });
    !unfinished
        && if available.is_empty() {
            series_fully_watched(id, episodes, progress)
        } else {
            available.into_iter().all(watched)
        }
}

pub(crate) fn activity_bucket(
    id: &str,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    activity: Option<&ViewingActivity>,
    today: i64,
) -> &'static str {
    if available_completed(id, episodes, progress, today) {
        "Completed"
    } else if activity.is_some_and(|a| a.phase == ViewingPhase::AwaitingReturn)
        && !progress.values().any(|p| {
            p.series_id == id && resumable_position(p.position_secs, p.duration_secs, p.watched)
        })
    {
        // Missing coverage or a future-only response cannot reactivate a
        // completed viewing run. The durable checkpoint remains authoritative.
        if episodes.iter().any(|v| {
            episode_has_air_date(v)
                && episode_is_out(v, today)
                && !progress
                    .get(&progress_map_key(id, &v.id))
                    .is_some_and(|p| p.watched)
        }) {
            "New episodes"
        } else {
            "Completed"
        }
    } else if progress.values().any(|p| p.series_id == id) {
        "Watching"
    } else {
        "Plan to Watch"
    }
}

/// Metadata can establish a missing checkpoint, but never activate viewing.
/// Only changed, meaningful progress (including explicit unwatch) does that.
pub(crate) fn reconcile_viewing_activity(
    activity: &mut HashMap<String, ViewingActivity>,
    entries: &[LibraryEntry],
    old: &HashMap<String, EpisodeProgress>,
    progress: &HashMap<String, EpisodeProgress>,
    episodes_for: impl Fn(&LibraryEntry) -> Vec<Video>,
    today: i64,
) {
    for entry in entries.iter().filter(|e| e.type_ != "movie") {
        let episodes = episodes_for(entry);
        let complete = available_completed(&entry.id, &episodes, progress, today);
        let changed = progress.iter().any(|(key, p)| {
            p.series_id == entry.id
                && (p.watched || p.position_secs >= RESUME_MIN_SECS || p.unwatched_at_secs > 0)
                && old.get(key).is_none_or(|previous| {
                    previous.watched != p.watched
                        || previous.position_secs != p.position_secs
                        || previous.unwatched_at_secs != p.unwatched_at_secs
                })
        });
        if complete || changed {
            activity.insert(
                entry.id.clone(),
                ViewingActivity {
                    phase: if complete {
                        ViewingPhase::AwaitingReturn
                    } else {
                        ViewingPhase::Active
                    },
                },
            );
        }
    }
}

pub(crate) fn viewing_menu_action(
    entry: &LibraryEntry,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    activity: Option<&ViewingActivity>,
) -> i32 {
    if entry.type_ == "movie" || entry.watch_status != WatchStatus::Auto {
        return 0;
    }
    match activity_bucket(&entry.id, episodes, progress, activity, today_days()) {
        "New episodes" => 4,
        "Watching"
            if progress
                .values()
                .any(|p| p.series_id == entry.id && p.watched)
                && !progress.values().any(|p| {
                    p.series_id == entry.id
                        && resumable_position(p.position_secs, p.duration_secs, p.watched)
                }) =>
        {
            3
        }
        _ => 0,
    }
}

impl Bridge {
    pub(super) fn migrate_viewing_activity(&self) {
        if applying() {
            return;
        }
        let (entries, progress) = {
            let state = self.shared.lock().unwrap();
            (state.entries.clone(), state.progress.clone())
        };
        let mut activity = read_viewing_activity();
        let before = activity.clone();
        // A received domain may not have been projected yet. Never let a
        // local migration replace a newer activity record waiting in the store.
        let known: HashSet<String> = nova_sync::local_store()
            .ok()
            .map(|store| {
                store
                    .lock()
                    .unwrap()
                    .records(DOMAIN_VIEWING_ACTIVITY)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<_> = entries
            .into_iter()
            .filter(|e| match activity.get(&e.id) {
                Some(value) => value.phase == ViewingPhase::Active,
                None => !known.contains(&e.id),
            })
            .collect();
        reconcile_viewing_activity(
            &mut activity,
            &missing,
            &progress,
            &progress,
            |e| read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default(),
            today_days(),
        );
        if activity != before {
            write_json(VIEWING_ACTIVITY_KEY, &activity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episodes() -> Vec<Video> {
        (1..=3)
            .map(|number| Video {
                id: format!("e{number}"),
                season: Some(1),
                episode: Some(number),
                released: Some(format!("2024-01-0{number}")),
                ..Default::default()
            })
            .collect()
    }

    fn progress(episode: &str, position: f64, watched: bool) -> EpisodeProgress {
        EpisodeProgress {
            series_id: "s".into(),
            episode_id: episode.into(),
            position_secs: position,
            duration_secs: 1000.0,
            watched,
            ..Default::default()
        }
    }

    fn reconcile(
        activity: &mut HashMap<String, ViewingActivity>,
        old: &HashMap<String, EpisodeProgress>,
        map: &HashMap<String, EpisodeProgress>,
        episodes: &[Video],
        today: i64,
    ) {
        let entry = serde_json::from_value(serde_json::json!({
            "id": "s", "type_": "series", "name": "Show"
        }))
        .unwrap();
        reconcile_viewing_activity(activity, &[entry], old, map, |_| episodes.to_vec(), today);
    }

    #[test]
    fn returning_show_waits_until_viewing_resumes_and_stays_active_through_backlog() {
        let episodes = episodes();
        let first_day = iso_days("2024-01-01").unwrap();
        let release_day = iso_days("2024-01-03").unwrap();
        let mut map = HashMap::from([(progress_map_key("s", "e1"), progress("e1", 1000.0, true))]);
        let mut activity = HashMap::new();
        reconcile(&mut activity, &HashMap::new(), &map, &episodes, first_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), first_day),
            "Completed"
        );

        // A restart and a release-date boundary preserve the viewing phase.
        let mut activity: HashMap<String, ViewingActivity> =
            serde_json::from_str(&serde_json::to_string(&activity).unwrap()).unwrap();
        reconcile(&mut activity, &map, &map, &episodes, release_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), release_day),
            "New episodes"
        );

        let old = map.clone();
        map.insert(progress_map_key("s", "e2"), progress("e2", 3.0, false));
        reconcile(&mut activity, &old, &map, &episodes, release_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), release_day),
            "New episodes"
        );

        let old = map.clone();
        map.get_mut(&progress_map_key("s", "e2"))
            .unwrap()
            .position_secs = RESUME_MIN_SECS;
        reconcile(&mut activity, &old, &map, &episodes, release_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), release_day),
            "Watching"
        );

        let old = map.clone();
        map.insert(progress_map_key("s", "e2"), progress("e2", 1000.0, true));
        reconcile(&mut activity, &old, &map, &episodes, release_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), release_day),
            "Watching"
        );
        let old = map.clone();
        map.insert(progress_map_key("s", "e3"), progress("e3", 1000.0, true));
        reconcile(&mut activity, &old, &map, &episodes, release_day);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), release_day),
            "Completed"
        );
        assert_eq!(activity["s"].phase, ViewingPhase::AwaitingReturn);
    }

    #[test]
    fn migration_never_guesses_that_an_existing_backlog_is_a_returning_show() {
        let episodes = episodes();
        let today = iso_days("2024-01-03").unwrap();
        let map = HashMap::from([(progress_map_key("s", "e1"), progress("e1", 1000.0, true))]);
        let mut activity = HashMap::new();
        reconcile(&mut activity, &map, &map, &episodes, today);
        assert!(activity.is_empty());
        assert_eq!(
            activity_bucket("s", &episodes, &map, None, today),
            "Watching"
        );
        assert_eq!(
            activity_bucket("s", &episodes, &HashMap::new(), None, today),
            "Plan to Watch"
        );
    }

    #[test]
    fn explicit_unwatch_reactivates_a_backlog_without_playback() {
        let episodes = &episodes()[..1];
        let today = iso_days("2024-01-03").unwrap();
        let old = HashMap::from([(progress_map_key("s", "e1"), progress("e1", 1000.0, true))]);
        let mut activity = HashMap::new();
        reconcile(&mut activity, &old, &old, episodes, today);
        let mut unwatched = progress("e1", 0.0, false);
        unwatched.unwatched_at_secs = 10;
        let map = HashMap::from([(progress_map_key("s", "e1"), unwatched)]);
        reconcile(&mut activity, &old, &map, episodes, today);
        assert_eq!(activity["s"].phase, ViewingPhase::Active);
        assert_eq!(
            activity_bucket("s", episodes, &map, activity.get("s"), today),
            "Watching"
        );
    }

    #[test]
    fn missing_metadata_retains_phase_and_dateless_extras_only_resume_when_started() {
        let mut episodes = episodes();
        episodes.truncate(1);
        episodes.push(Video {
            id: "extra".into(),
            ..Default::default()
        });
        let today = iso_days("2024-01-03").unwrap();
        let mut map = HashMap::from([(progress_map_key("s", "e1"), progress("e1", 1000.0, true))]);
        let mut activity = HashMap::new();
        reconcile(&mut activity, &map, &map, &episodes, today);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), today),
            "Completed"
        );
        reconcile(&mut activity, &map, &map, &[], today);
        assert_eq!(activity["s"].phase, ViewingPhase::AwaitingReturn);
        assert_eq!(
            activity_bucket("s", &[], &map, activity.get("s"), today),
            "Completed"
        );
        let future_only = vec![Video {
            id: "future".into(),
            released: Some("2999-01-01".into()),
            ..Default::default()
        }];
        assert_eq!(
            activity_bucket("s", &future_only, &map, activity.get("s"), today),
            "Completed"
        );
        let old = map.clone();
        map.insert(
            progress_map_key("s", "extra"),
            progress("extra", 20.0, false),
        );
        reconcile(&mut activity, &old, &map, &episodes, today);
        assert_eq!(
            activity_bucket("s", &episodes, &map, activity.get("s"), today),
            "Watching"
        );
    }
}

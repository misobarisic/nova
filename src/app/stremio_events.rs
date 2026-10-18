//! Native source events retain their original field timestamps across mesh
//! projections, metadata refreshes and Stremio polling. Rows are never swept
//! against a snapshot: a removal/unwatch is itself a durable event.
use super::*;
use nova_stremio::{Canonical, Resume, Stamped};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

thread_local! { static EXTERNAL: Cell<u32> = const { Cell::new(0) }; }

/// External ingress creates mesh records once, while ApplyingGuard still
/// suppresses Stremio events and outbound tracker journal entries.
pub(super) struct ExternalGuard(std::marker::PhantomData<Rc<()>>);
impl ExternalGuard {
    pub(super) fn new() -> Self {
        EXTERNAL.with(|depth| depth.set(depth.get() + 1));
        Self(std::marker::PhantomData)
    }
}
impl Drop for ExternalGuard {
    fn drop(&mut self) {
        EXTERNAL.with(|depth| depth.set(depth.get() - 1));
    }
}
pub(super) fn external() -> bool {
    EXTERNAL.with(|depth| depth.get() != 0)
}

#[derive(Deserialize)]
struct Connection {
    version: u32,
    account_id: String,
}

pub(super) fn event_key(account: &str, type_: &str, id: &str) -> String {
    // JSON tuple encoding preserves arbitrary provider IDs without delimiter
    // collisions (IDs and account identifiers can themselves contain ':').
    format!(
        "event:{}",
        serde_json::to_string(&(account, type_, id)).expect("string tuple")
    )
}
pub(super) fn try_load(
    store: &nova_sync::Store,
    account: &str,
    type_: &str,
    id: &str,
) -> Result<Option<Canonical>, serde_json::Error> {
    let prefix = format!("{}:", event_key(account, type_, id));
    let mut canonical = Canonical::default();
    let mut found = false;
    for (key, raw) in
        store.records_with_prefix(&crate::app::sync::profile_domain(stremio::DOMAIN), &prefix)
    {
        let Some(field) = key.strip_prefix(&prefix) else {
            continue;
        };
        match field {
            "saved" => canonical.saved = serde_json::from_str(&raw)?,
            "resume" => canonical.resume = serde_json::from_str(&raw)?,
            _ => {
                let Some(episode) = field.strip_prefix("watched:") else {
                    // A newer peer may add fields; unknown names have no
                    // authority over the clocks this version understands.
                    continue;
                };
                let episode: String = serde_json::from_str(episode)?;
                canonical
                    .watched
                    .insert(episode, serde_json::from_str(&raw)?);
            }
        }
        found = true;
    }
    Ok(found.then_some(canonical))
}

// Kept for test assertions; production callers must preserve corruption as an
// error rather than reinterpret it as a missing source event.
#[cfg(test)]
pub(super) fn load(
    store: &nova_sync::Store,
    account: &str,
    type_: &str,
    id: &str,
) -> Option<Canonical> {
    try_load(store, account, type_, id).expect("valid source event fixture")
}

type Events = BTreeMap<(String, String), Canonical>;
fn event<'a>(
    events: &'a mut Events,
    store: &nova_sync::Store,
    account: &str,
    type_: &str,
    id: &str,
) -> Result<&'a mut Canonical, serde_json::Error> {
    use std::collections::btree_map::Entry;
    match events.entry((type_.into(), id.into())) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(entry) => {
            Ok(entry.insert(try_load(store, account, type_, id)?.unwrap_or_default()))
        }
    }
}

pub(super) fn capture(
    store: &mut nova_sync::Store,
    key: &str,
    previous: Option<&str>,
    next: &str,
    seed: bool,
) -> nova_sync::Result<()> {
    if seed || sync::applying() || !matches!(key, "library" | EPISODE_PROGRESS_KEY) {
        return Ok(());
    }
    let Some(raw) = store
        .record(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            "connection",
        )
        .and_then(|record| record.value.as_deref())
    else {
        return Ok(());
    };
    let Ok(connection) = serde_json::from_str::<Connection>(raw) else {
        return Ok(());
    };
    if connection.version != 1 || connection.account_id.is_empty() {
        return Ok(());
    }
    let stamp = nova_config::now_ms();
    let events = changes(store, &connection.account_id, key, previous, next, stamp)?;
    if events.is_empty() {
        return Ok(());
    }
    let dev = nova_sync::local_device()?;
    for ((media_type, id), canonical) in events {
        let key = event_key(&connection.account_id, &media_type, &id);
        let previous =
            try_load(store, &connection.account_id, &media_type, &id)?.unwrap_or_default();
        if canonical.saved != previous.saved {
            store.set(
                &crate::app::sync::profile_domain(stremio::DOMAIN),
                &format!("{key}:saved"),
                Some(serde_json::to_string(&canonical.saved)?),
                0,
                dev,
            );
        }
        if canonical.resume != previous.resume {
            store.set(
                &crate::app::sync::profile_domain(stremio::DOMAIN),
                &format!("{key}:resume"),
                Some(serde_json::to_string(&canonical.resume)?),
                0,
                dev,
            );
        }
        for (episode, value) in canonical.watched {
            if previous.watched.get(&episode) != Some(&value) {
                let episode = serde_json::to_string(&episode)?;
                store.set(
                    &crate::app::sync::profile_domain(stremio::DOMAIN),
                    &format!("{key}:watched:{episode}"),
                    Some(serde_json::to_string(&value)?),
                    0,
                    dev,
                );
            }
        }
    }
    Ok(())
}

fn changes(
    store: &nova_sync::Store,
    account: &str,
    key: &str,
    previous: Option<&str>,
    next: &str,
    stamp: u64,
) -> nova_sync::Result<Events> {
    let mut events = Events::new();
    if key == "library" {
        let before: Vec<LibraryEntry> = previous
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let after: Vec<LibraryEntry> = serde_json::from_str(next)?;
        let before: BTreeSet<_> = before
            .iter()
            .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
            .collect();
        let after: BTreeSet<_> = after
            .iter()
            .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
            .collect();
        for (type_, id) in before.symmetric_difference(&after) {
            event(&mut events, store, account, type_, id)?.saved = Stamped {
                value: after.contains(&(*type_, *id)),
                changed_at: stamp,
            };
        }
    } else if key == EPISODE_PROGRESS_KEY {
        let before: HashMap<String, EpisodeProgress> = previous
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let after: HashMap<String, EpisodeProgress> = serde_json::from_str(next)?;
        let library: Vec<LibraryEntry> = store
            .extra_value(&profiles::storage_key("library"))?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()?
            .unwrap_or_default();
        let source_type = |progress: &EpisodeProgress| -> Option<String> {
            library
                .iter()
                .find(|entry| entry.id == progress.series_id)
                .map(|entry| entry.type_.clone())
                .or_else(|| {
                    progress
                        .history
                        .as_ref()
                        .map(|history| history.type_.clone())
                })
                .filter(|type_| !type_.is_empty())
        };
        for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let old = before.get(key);
            let new = after.get(key);
            let progress = new.or(old).expect("union key");
            let Some(type_) = source_type(progress) else {
                continue;
            };
            let old_watched = old.is_some_and(|value| value.watched);
            let new_watched = new.is_some_and(|value| value.watched);
            let unwatch = new.is_some_and(|value| {
                value.unwatched_at_secs > 0
                    && old.is_none_or(|old| old.unwatched_at_secs != value.unwatched_at_secs)
            });
            if old_watched != new_watched || unwatch {
                event(&mut events, store, account, &type_, &progress.series_id)?
                    .watched
                    .insert(
                        progress.episode_id.clone(),
                        Stamped {
                            value: new_watched,
                            changed_at: stamp,
                        },
                    );
            }
        }
        let latest = |map: &HashMap<String, EpisodeProgress>| {
            let mut titles: BTreeMap<String, (String, EpisodeProgress)> = BTreeMap::new();
            for (key, progress) in map {
                let replace = titles
                    .get(&progress.series_id)
                    .is_none_or(|(old_key, old)| {
                        (progress.updated_at_secs, key) > (old.updated_at_secs, old_key)
                    });
                if replace {
                    titles.insert(progress.series_id.clone(), (key.clone(), progress.clone()));
                }
            }
            titles
        };
        let old_latest = latest(&before);
        let new_latest = latest(&after);
        for id in old_latest
            .keys()
            .chain(new_latest.keys())
            .collect::<BTreeSet<_>>()
        {
            let old = old_latest.get(id);
            let new = new_latest.get(id);
            let progress = new.or(old).expect("union title");
            let Some(type_) = source_type(&progress.1) else {
                continue;
            };
            let old_resume = old.and_then(|(_, progress)| resume(progress));
            // Removing the latest checkpoint is a clear, rather than permission
            // to resurrect a stale checkpoint from an older episode.
            let removed_latest = old.is_some_and(|(key, _)| !after.contains_key(key));
            let new_resume = if removed_latest
                && new.is_none_or(|(_, value)| {
                    old.is_some_and(|(_, old)| value.updated_at_secs <= old.updated_at_secs)
                }) {
                None
            } else {
                new.and_then(|(_, progress)| resume(progress))
            };
            if old_resume != new_resume {
                event(&mut events, store, account, &type_, id)?.resume = Stamped {
                    value: new_resume,
                    changed_at: stamp,
                };
            }
        }
    }
    Ok(events)
}

fn resume(progress: &EpisodeProgress) -> Option<Resume> {
    // Use the same completed/replay and near-EOF policy as native Continue
    // Watching and the Stremio actor's projection. Manual completion clears a
    // pointer; a real replay can retain sticky watched history and resume.
    if !playback::playback_can_resume(progress) {
        return None;
    }
    Some(Resume {
        video_id: progress.episode_id.clone(),
        position_ms: (progress.position_secs * 1000.0) as u64,
        duration_ms: (progress.duration_secs.max(0.0) * 1000.0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(category: &str) -> String {
        serde_json::json!([{"id":"tt1","type_":"series","name":"Title","categories":[category]}])
            .to_string()
    }
    fn progress(watched: bool, position: f64, unwatch: u64) -> String {
        let value = EpisodeProgress {
            series_id: "tt1".into(),
            episode_id: "tt1:1:1".into(),
            watched,
            position_secs: position,
            duration_secs: 100.0,
            unwatched_at_secs: unwatch,
            updated_at_secs: 12,
            history: Some(PlaybackHistory {
                type_: "series".into(),
                rewatch: watched && position > 0.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        serde_json::to_string(&HashMap::from([("episode".to_string(), value)])).unwrap()
    }

    #[test]
    fn membership_changes_only_on_add_remove_and_keeps_other_field_clocks() {
        let mut store = nova_sync::Store::default();
        let prefix = event_key("account", "series", "tt1");
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:resume"),
            Some(
                serde_json::to_string(&Stamped {
                    value: Some(Resume {
                        video_id: "tt1:1:1".into(),
                        position_ms: 4000,
                        duration_ms: 10000,
                    }),
                    changed_at: 7,
                })
                .unwrap(),
            ),
            0,
            1,
        );
        assert!(
            changes(
                &store,
                "account",
                "library",
                Some(&library("A")),
                &library("B"),
                100
            )
            .unwrap()
            .is_empty()
        );
        let added = changes(&store, "account", "library", Some("[]"), &library("A"), 100).unwrap();
        let value = &added[&("series".into(), "tt1".into())];
        assert_eq!(
            value.saved,
            Stamped {
                value: true,
                changed_at: 100
            }
        );
        assert_eq!(value.resume.changed_at, 7);
        let removed =
            changes(&store, "account", "library", Some(&library("A")), "[]", 200).unwrap();
        assert_eq!(
            removed[&("series".into(), "tt1".into())].saved,
            Stamped {
                value: false,
                changed_at: 200
            }
        );
    }

    #[test]
    fn unwatch_and_resume_clear_are_independent_source_events() {
        let mut store = nova_sync::Store::default();
        store.queue_extra(&profiles::storage_key("library"), Some("[]".into()));
        let before = progress(true, 50.0, 0);
        let after = progress(false, 0.0, 15);
        let events = changes(
            &store,
            "account",
            EPISODE_PROGRESS_KEY,
            Some(&before),
            &after,
            15000,
        )
        .unwrap();
        let value = &events[&("series".into(), "tt1".into())];
        assert_eq!(
            value.watched["tt1:1:1"],
            Stamped {
                value: false,
                changed_at: 15000
            }
        );
        assert_eq!(
            value.resume,
            Stamped {
                value: None,
                changed_at: 15000
            }
        );
        assert_eq!(value.saved.changed_at, 0);
        assert!(
            changes(
                &store,
                "account",
                EPISODE_PROGRESS_KEY,
                Some(&after),
                &after,
                16000
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn manual_completion_clears_resume_but_actual_replay_keeps_it() {
        let mut store = nova_sync::Store::default();
        store.queue_extra(&profiles::storage_key("library"), Some("[]".into()));
        let before = progress(false, 50.0, 0);
        let mut completed: HashMap<String, EpisodeProgress> =
            serde_json::from_str(&before).unwrap();
        completed.get_mut("episode").unwrap().watched = true;
        let completed = serde_json::to_string(&completed).unwrap();
        let events = changes(
            &store,
            "account",
            EPISODE_PROGRESS_KEY,
            Some(&before),
            &completed,
            15000,
        )
        .unwrap();
        let value = &events[&("series".into(), "tt1".into())];
        assert!(value.watched["tt1:1:1"].value);
        assert_eq!(
            value.resume,
            Stamped {
                value: None,
                changed_at: 15000
            }
        );
        let replay = progress(true, 50.0, 0);
        let events = changes(
            &store,
            "account",
            EPISODE_PROGRESS_KEY,
            Some(&completed),
            &replay,
            16000,
        )
        .unwrap();
        assert!(
            events[&("series".into(), "tt1".into())]
                .resume
                .value
                .is_some()
        );
    }

    #[test]
    fn deleting_latest_checkpoint_clears_resume_and_retains_unwatch() {
        let mut store = nova_sync::Store::default();
        store.queue_extra(&profiles::storage_key("library"), Some("[]".into()));
        let before = progress(true, 50.0, 0);
        let events = changes(
            &store,
            "account",
            EPISODE_PROGRESS_KEY,
            Some(&before),
            "{}",
            100,
        )
        .unwrap();
        let value = &events[&("series".into(), "tt1".into())];
        assert!(!value.watched["tt1:1:1"].value);
        assert_eq!(
            value.resume,
            Stamped {
                value: None,
                changed_at: 100
            }
        );
    }

    #[test]
    fn episode_fields_load_independently_and_account_keys_do_not_collide() {
        let mut store = nova_sync::Store::default();
        let prefix = event_key("a:b", "series", "tt1");
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:watched:\"one\""),
            Some(r#"{"value":true,"changed_at":1}"#.into()),
            0,
            1,
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:watched:\"two\""),
            Some(r#"{"value":false,"changed_at":2}"#.into()),
            0,
            2,
        );
        let value = load(&store, "a:b", "series", "tt1").unwrap();
        assert!(value.watched["one"].value);
        assert!(!value.watched["two"].value);
        assert_eq!(value.watched["two"].changed_at, 2);
        assert!(load(&store, "a", "b:series", "tt1").is_none());
    }

    #[test]
    fn corrupt_known_field_blocks_capture_without_overwriting_clock_rows() {
        let mut store = nova_sync::Store::default();
        let prefix = event_key("account", "series", "tt1");
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            "connection",
            Some(r#"{"version":1,"account_id":"account"}"#.into()),
            0,
            1,
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:saved"),
            Some("unreadable".into()),
            0,
            1,
        );
        let before = store.records(&crate::app::sync::profile_domain(stremio::DOMAIN));
        assert!(try_load(&store, "account", "series", "tt1").is_err());
        // Failure happens before resolving a local device or mutating any row.
        assert!(capture(&mut store, "library", Some("[]"), &library("A"), false).is_err());
        assert_eq!(
            store.records(&crate::app::sync::profile_domain(stremio::DOMAIN)),
            before
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:saved"),
            Some(r#"{"value":true,"changed_at":5}"#.into()),
            0,
            1,
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &format!("{prefix}:future-field"),
            Some("unknown future schema".into()),
            0,
            1,
        );
        assert_eq!(
            try_load(&store, "account", "series", "tt1")
                .unwrap()
                .unwrap()
                .saved
                .changed_at,
            5
        );
    }

    #[test]
    fn credential_tombstone_prevents_old_account_events_from_being_updated() {
        let mut store = nova_sync::Store::default();
        let key = format!("{}:saved", event_key("old-account", "series", "tt1"));
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            &key,
            Some(r#"{"value":true,"changed_at":10}"#.into()),
            0,
            1,
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            "connection",
            Some(r#"{"version":1,"account_id":"old-account"}"#.into()),
            0,
            1,
        );
        store.set(
            &crate::app::sync::profile_domain(stremio::DOMAIN),
            "connection",
            None,
            0,
            1,
        );
        let before = store.records(&crate::app::sync::profile_domain(stremio::DOMAIN));
        capture(&mut store, "library", Some(&library("A")), "[]", false).unwrap();
        assert_eq!(
            store.records(&crate::app::sync::profile_domain(stremio::DOMAIN)),
            before
        );
        assert!(
            store
                .record(
                    &crate::app::sync::profile_domain(stremio::DOMAIN),
                    "connection"
                )
                .unwrap()
                .value
                .is_none()
        );
        assert_eq!(
            load(&store, "old-account", "series", "tt1")
                .unwrap()
                .saved
                .changed_at,
            10
        );
        assert!(load(&store, "new-account", "series", "tt1").is_none());
    }

    #[test]
    fn guarded_projections_never_capture_events_and_external_guard_nests() {
        let mut store = nova_sync::Store::default();
        let applying = sync::ApplyingGuard::new();
        capture(&mut store, "library", Some("[]"), &library("A"), false).unwrap();
        {
            let _external = ExternalGuard::new();
            assert!(external());
            {
                let _nested = ExternalGuard::new();
                assert!(external());
            }
            assert!(external());
            assert!(
                !crate::app::profiles::thread::spawn(external)
                    .join()
                    .unwrap()
            );
            capture(&mut store, "library", Some("[]"), &library("A"), false).unwrap();
        }
        assert!(!external());
        assert!(
            store
                .records(&crate::app::sync::profile_domain(stremio::DOMAIN))
                .is_empty()
        );
        drop(applying);
        capture(&mut store, "library", Some("[]"), &library("A"), true).unwrap();
        assert!(
            store
                .records(&crate::app::sync::profile_domain(stremio::DOMAIN))
                .is_empty()
        );
    }
}

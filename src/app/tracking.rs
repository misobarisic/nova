//! Local-only tracking coordinator and semantic playback journal.
use super::*;
use crate::providers;
mod ui;
mod worker;
use nova_tracking::{Service, SourceRef};
use std::sync::mpsc::{self, Receiver, SyncSender};

pub(super) type StateHandle = Arc<TrackingHandle>;
pub(super) struct TrackingHandle {
    tx: SyncSender<worker::Command>,
    alive: AtomicBool,
    rx: Mutex<Option<Receiver<worker::Command>>>,
    context_generation: AtomicU64,
    login_generation: [AtomicU64; 2],
}
impl TrackingHandle {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(64);
        Self {
            tx,
            alive: AtomicBool::new(true),
            rx: Mutex::new(Some(rx)),
            context_generation: AtomicU64::new(0),
            login_generation: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }
    fn send(&self, command: worker::Command) {
        if self.tx.try_send(command).is_err() {
            tracing::warn!("tracking command queue unavailable");
        }
    }
}
#[derive(Clone)]
struct SourceContext {
    source: SourceRef,
    title: String,
    ids: providers::ExternalIds,
    aliases: Vec<String>,
    year: Option<u16>,
    episodes: Vec<(String, String)>,
}
fn service(index: i32) -> Option<Service> {
    match index {
        0 => Some(Service::MyAnimeList),
        1 => Some(Service::AniList),
        _ => None,
    }
}
fn service_index(service: Service) -> usize {
    match service {
        Service::MyAnimeList => 0,
        Service::AniList => 1,
    }
}
impl Bridge {
    pub(super) fn shutdown_tracking(&self) {
        self.tracking.alive.store(false, Ordering::Release);
        for epoch in &self.tracking.login_generation {
            epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
    pub(super) fn initialize_tracking(&self) {
        let Some(rx) = self.tracking.rx.lock().unwrap().take() else {
            return;
        };
        let bridge = self.clone();
        thread::spawn(move || worker::run(bridge, rx));
    }
}
#[derive(Serialize, Deserialize)]
struct SourceEvidence {
    ids: providers::ExternalIds,
    title: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    year: Option<u16>,
    retrieved_at: u64,
}
fn evidence_key(type_: &str, id: &str) -> String {
    format!(
        "tracking:source:{}",
        serde_json::to_string(&(type_, id)).unwrap()
    )
}
pub(super) fn remember_source(preview: &MetaPreview) {
    let mut ids = providers::normalize_external_ids(&preview.extra, &preview.id, &preview.type_);
    if let Ok(Some(previous)) =
        read_json_result::<SourceEvidence>(&evidence_key(&preview.type_, &preview.id))
    {
        for id in previous.ids.typed {
            if !ids.typed.contains(&id) && ids.typed.len() < 32 {
                ids.typed.push(id);
            }
        }
    }
    let evidence = SourceEvidence {
        ids,
        title: preview.title(),
        aliases: source_aliases(&preview.extra),
        year: preview
            .year_str()
            .and_then(|y| y.get(..4).and_then(|y| y.parse().ok())),
        retrieved_at: now_secs(),
    };
    if let Ok(raw) = serde_json::to_string(&evidence)
        && let Err(error) = storage::try_set_str(&evidence_key(&preview.type_, &preview.id), &raw)
    {
        storage::report(error);
    }
}

fn source_aliases(extra: &HashMap<String, serde_json::Value>) -> Vec<String> {
    let mut aliases = Vec::new();
    for key in ["aliases", "aka", "akaNames"] {
        if let Some(values) = extra.get(key).and_then(serde_json::Value::as_array) {
            for value in values.iter().take(8).filter_map(serde_json::Value::as_str) {
                if !value.trim().is_empty()
                    && value.len() <= 512
                    && !aliases.iter().any(|s| s == value)
                    && aliases.len() < 8
                {
                    aliases.push(value.to_string());
                }
            }
        }
    }
    aliases
}

/// Called while the sync snapshot store is locked; desired failed writes are
/// part of the baseline, so retrying persistence cannot invent transitions.
pub(super) fn journal_progress(
    store: &mut nova_sync::Store,
    previous: Option<&str>,
    next: &str,
    remote: bool,
    seed: bool,
) -> nova_sync::Result<()> {
    if seed {
        return Ok(());
    }
    let before: HashMap<String, EpisodeProgress> = previous
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let after: HashMap<String, EpisodeProgress> = serde_json::from_str(next)?;
    let mut changes = Vec::new();
    for (key, progress) in &after {
        let started = progress.position_secs >= 1.0
            && before.get(key).is_none_or(|old| old.position_secs < 1.0)
            && progress.play_count > 0;
        if before.get(key).is_some_and(|old| old.watched) != progress.watched || started {
            changes.push(nova_tracking::WatchChange {
                series_id: progress.series_id.clone(),
                episode_id: progress.episode_id.clone(),
                watched: progress.watched,
                started,
            });
        }
    }
    for (key, progress) in &before {
        if progress.watched && !after.contains_key(key) {
            changes.push(nova_tracking::WatchChange {
                series_id: progress.series_id.clone(),
                episode_id: progress.episode_id.clone(),
                watched: false,
                started: false,
            });
        }
    }
    if changes.is_empty() {
        return Ok(());
    }
    changes.sort_by(|a, b| (&a.series_id, &a.episode_id).cmp(&(&b.series_id, &b.episode_id)));
    if store
        .extra_value(nova_tracking::JOURNAL_ERROR_KEY)?
        .is_some()
    {
        return Ok(());
    }
    let sequence = store
        .extra_value(nova_tracking::EVENT_COUNTER_KEY)?
        .map(|raw| serde_json::from_str::<u64>(&raw))
        .transpose();
    let sequence = match sequence {
        Ok(value) => value.unwrap_or(0),
        Err(_) => {
            store.queue_extra(
                nova_tracking::JOURNAL_ERROR_KEY,
                Some("invalid event counter; primary retained".into()),
            );
            return Ok(());
        }
    };
    let Some(sequence) = sequence.checked_add(1) else {
        store.queue_extra(
            nova_tracking::JOURNAL_ERROR_KEY,
            Some("event counter exhausted; primary retained".into()),
        );
        return Ok(());
    };
    let event = nova_tracking::WatchEvent {
        sequence,
        origin: if remote {
            nova_tracking::EventOrigin::PairedDevice
        } else {
            nova_tracking::EventOrigin::Local
        },
        changes,
        date: Some(nova_tracking::ListDate::today()),
    };
    store.queue_extra(
        &format!("{}{sequence:020}", nova_tracking::EVENT_PREFIX),
        Some(serde_json::to_string(&event)?),
    );
    store.queue_extra(nova_tracking::EVENT_COUNTER_KEY, Some(sequence.to_string()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_tracking::{
        EVENT_COUNTER_KEY, EVENT_PREFIX, EventOrigin, JOURNAL_ERROR_KEY, WatchEvent,
    };
    fn store() -> nova_sync::Store {
        let mut store = nova_sync::Store::default();
        store.queue_extra(EVENT_COUNTER_KEY, Some("0".into()));
        store.queue_extra(JOURNAL_ERROR_KEY, None);
        store
    }
    fn map(watched: bool, position: f64) -> HashMap<String, EpisodeProgress> {
        HashMap::from([(
            "source:episode".into(),
            EpisodeProgress {
                series_id: "source".into(),
                episode_id: "native-episode".into(),
                watched,
                position_secs: position,
                play_count: 1,
                ..Default::default()
            },
        )])
    }
    fn json(map: &HashMap<String, EpisodeProgress>) -> String {
        serde_json::to_string(map).unwrap()
    }
    #[test]
    fn journal_ignores_timestamps_and_repeated_position_saves() {
        let mut store = store();
        let previous = json(&map(false, 10.0));
        let mut next = map(false, 20.0);
        next.values_mut().next().unwrap().updated_at_secs = 123;
        journal_progress(&mut store, Some(&previous), &json(&next), false, false).unwrap();
        assert_eq!(
            store.extra_value(EVENT_COUNTER_KEY).unwrap(),
            Some("0".into())
        );
    }
    #[test]
    fn journal_distinguishes_internal_start_watched_and_remote_origin() {
        let mut store = store();
        let old = json(&map(false, 0.0));
        let started = json(&map(false, 1.5));
        journal_progress(&mut store, Some(&old), &started, false, false).unwrap();
        let event: WatchEvent = serde_json::from_str(
            &store
                .extra_value(&format!("{EVENT_PREFIX}{:020}", 1))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(event.changes[0].started);
        assert!(!event.changes[0].watched);
        assert_eq!(event.origin, EventOrigin::Local);
        journal_progress(
            &mut store,
            Some(&started),
            &json(&map(true, 12.0)),
            true,
            false,
        )
        .unwrap();
        let event: WatchEvent = serde_json::from_str(
            &store
                .extra_value(&format!("{EVENT_PREFIX}{:020}", 2))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(event.changes[0].watched);
        assert_eq!(event.changes[0].episode_id, "native-episode");
        assert_eq!(event.origin, EventOrigin::PairedDevice);
    }
    #[test]
    fn corrupt_journal_counter_pauses_tracking_without_rejecting_history() {
        let mut store = store();
        store.queue_extra(EVENT_COUNTER_KEY, Some("corrupt".into()));
        journal_progress(
            &mut store,
            Some(&json(&map(false, 0.0))),
            &json(&map(true, 0.0)),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            store.extra_value(EVENT_COUNTER_KEY).unwrap(),
            Some("corrupt".into())
        );
        assert!(store.extra_value(JOURNAL_ERROR_KEY).unwrap().is_some());
    }
}

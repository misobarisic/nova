//! Tracking coordinator, peer projection and local semantic playback journal.
use super::*;
use crate::providers;
mod ui;
mod worker;
pub(super) fn sign_in_background() -> Result<Box<dyn Send>, String> {
    worker::sign_in_background()
}
use nova_tracking::{Service, SourceRef};
use std::sync::mpsc::{self, Receiver, SyncSender};

pub(super) type StateHandle = Arc<TrackingHandle>;
#[derive(Clone, Copy)]
struct CompletionPromptPolicy {
    enabled: bool,
    watermark: u64,
    revision: u64,
}
pub(super) struct TrackingHandle {
    tx: SyncSender<worker::Command>,
    alive: AtomicBool,
    rx: Mutex<Option<Receiver<worker::Command>>>,
    context_generation: AtomicU64,
    context_evidence: Mutex<Option<(SourceRef, Vec<nova_tracking::EpisodeInfo>)>>,
    login_generation: [AtomicU64; 2],
    completion_policy: Mutex<Option<CompletionPromptPolicy>>,
}
impl TrackingHandle {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(64);
        Self {
            tx,
            alive: AtomicBool::new(true),
            rx: Mutex::new(Some(rx)),
            context_generation: AtomicU64::new(0),
            context_evidence: Mutex::new(None),
            login_generation: [AtomicU64::new(0), AtomicU64::new(0)],
            completion_policy: Mutex::new(None),
        }
    }
    fn completion_policy_with(
        &self,
        enabled: bool,
        read_watermark: impl FnOnce() -> Result<u64, String>,
    ) -> Result<CompletionPromptPolicy, String> {
        let mut policy = self.completion_policy.lock().unwrap();
        if let Some(current) = *policy
            && current.enabled == enabled
        {
            return Ok(current);
        }
        let current = CompletionPromptPolicy {
            enabled,
            watermark: read_watermark()?,
            revision: policy.map_or(1, |p| p.revision.wrapping_add(1)),
        };
        *policy = Some(current);
        Ok(current)
    }

    fn send(&self, command: worker::Command) {
        if self.tx.try_send(command).is_err() {
            tracing::warn!("tracking command queue unavailable");
        }
    }
}
impl TrackingHandle {
    fn metadata_generation(
        &self,
        source: &SourceRef,
        info: &[nova_tracking::EpisodeInfo],
    ) -> Option<u64> {
        let mut evidence = self.context_evidence.lock().unwrap();
        let changed = evidence
            .as_ref()
            .is_some_and(|(current, old)| current == source && old != info);
        if changed {
            *evidence = None;
            Some(self.context_generation.fetch_add(1, Ordering::AcqRel) + 1)
        } else {
            None
        }
    }
}

#[derive(Clone)]
struct SourceContext {
    source: SourceRef,
    title: String,
    ids: providers::ExternalIds,
    historical_ids: Vec<providers::ExternalId>,
    aliases: Vec<String>,
    year: Option<u16>,
    episodes: Vec<(String, String)>,
    episode_info: Vec<nova_tracking::EpisodeInfo>,
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
    /// Capture the journal boundary when a preference changes, rather than
    /// after a slow tracker request. The sync store includes not-yet-flushed
    /// history events, so retrying a failed write cannot turn old history new.
    fn tracking_completion_policy(&self) -> Result<CompletionPromptPolicy, String> {
        let enabled = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .completion_rating_prompts_enabled;
        self.tracking.completion_policy_with(enabled, || {
            let owner = nova_sync::local_store()
                .map_err(|_| text::tr("Tracking event data needs recovery.").to_string())?;
            owner
                .lock()
                .unwrap()
                .extra_value(nova_tracking::EVENT_COUNTER_KEY)
                .map_err(|_| text::tr("Tracking event data needs recovery.").to_string())?
                .map(|raw| serde_json::from_str::<u64>(&raw))
                .transpose()
                .map_err(|_| text::tr("Tracking event data needs recovery.").to_string())
                .map(|value| value.unwrap_or(0))
        })
    }
    pub(in crate::app) fn tracking_completion_preferences(&self, enabled: bool) {
        if self.tracking_completion_policy().is_err() {
            return;
        }
        self.tracking
            .send(worker::Command::CompletionPreferences { enabled });
    }
    pub(in crate::app) fn tracking_completion_sync_apply(&self) {
        self.tracking.send(worker::Command::CompletionSync);
    }
    pub(in crate::app) fn refresh_completion_language_text(&self) {
        self.tracking.send(worker::Command::CompletionRefresh);
    }
    pub(in crate::app) fn wire_completion_prompts(&self) {
        let Some(app) = self.app() else { return };
        let b = self.clone();
        app.on_completion_score_changed(move |token, id, value| {
            b.tracking.send(worker::Command::CompletionScore {
                token: token.to_string(),
                id: id.to_string(),
                value: value.to_string(),
            });
        });
        let b = self.clone();
        app.on_completion_save(move |token| {
            b.tracking.send(worker::Command::CompletionSave {
                token: token.to_string(),
            });
        });
        let b = self.clone();
        app.on_completion_dismiss(move |token| {
            b.tracking.send(worker::Command::CompletionDismiss {
                token: token.to_string(),
            });
        });
    }
    pub(in crate::app) fn tracking_sync_apply(&self) {
        self.tracking.send(worker::Command::Sync);
    }
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
    /// Older observations can suggest releases, but cannot contradict the
    /// latest snapshot or establish a current source identity.
    #[serde(default)]
    historical_ids: Vec<providers::ExternalId>,
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
    let previous = read_json_result::<SourceEvidence>(&evidence_key(&preview.type_, &preview.id))
        .ok()
        .flatten();
    let evidence = source_evidence(preview, previous.as_ref(), now_secs());
    if let Ok(raw) = serde_json::to_string(&evidence)
        && let Err(error) = storage::try_set_str(&evidence_key(&preview.type_, &preview.id), &raw)
    {
        storage::report(error);
    }
}

fn source_evidence(
    preview: &MetaPreview,
    previous: Option<&SourceEvidence>,
    retrieved_at: u64,
) -> SourceEvidence {
    let ids = providers::normalize_external_ids(&preview.extra, &preview.id, &preview.type_);
    let current_ids = evidence_ids(&ids);
    let mut historical_ids = Vec::new();
    if let Some(previous) = previous {
        // A catalog preview can be sparse. Keep displaced IDs as discovery
        // hints, rather than inventing a simultaneous conflict or forgetting
        // useful metadata. Newer observations take precedence within the bound.
        for id in evidence_ids(&previous.ids)
            .into_iter()
            .chain(previous.historical_ids.iter().cloned())
        {
            if !current_ids.contains(&id)
                && !historical_ids.contains(&id)
                && historical_ids.len() < 32
            {
                historical_ids.push(id);
            }
        }
    }
    let mut aliases = source_aliases(&preview.extra);
    if aliases.is_empty() {
        aliases = previous.map(|e| e.aliases.clone()).unwrap_or_default();
    }
    let title = preview.title();
    SourceEvidence {
        ids,
        historical_ids,
        title: if title.is_empty() {
            previous.map(|e| e.title.clone()).unwrap_or_default()
        } else {
            title
        },
        aliases,
        year: preview
            .year_str()
            .and_then(|y| y.get(..4).and_then(|y| y.parse().ok()))
            .or_else(|| previous.and_then(|e| e.year)),
        retrieved_at,
    }
}

fn evidence_ids(ids: &providers::ExternalIds) -> Vec<providers::ExternalId> {
    let mut claims = Vec::new();
    for namespace in [
        providers::IdNamespace::Imdb,
        providers::IdNamespace::TmdbTv,
        providers::IdNamespace::TmdbMovie,
        providers::IdNamespace::MalAnime,
        providers::IdNamespace::MalManga,
        providers::IdNamespace::AnilistAnime,
        providers::IdNamespace::AnilistManga,
    ] {
        match ids.resolve_id(namespace) {
            providers::IdResolution::Unique(id) => claims.push(id),
            providers::IdResolution::Conflict(ids) => claims.extend(ids),
            providers::IdResolution::Missing => {}
        }
    }
    claims
}

#[cfg(test)]
mod source_evidence_tests {
    use super::*;
    use serde_json::json;

    fn preview(extra: serde_json::Value) -> MetaPreview {
        let mut value =
            json!({"id":"tt26743760", "type":"series", "name":"The Apothecary Diaries"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn fresh_release_ids_replace_historical_identity_without_losing_hints() {
        let first = source_evidence(&preview(json!({"_malId":54492, "year":"2023"})), None, 1);
        let latest = source_evidence(&preview(json!({"_malId":61987})), Some(&first), 2);
        assert_eq!(
            latest.ids.resolve_id(providers::IdNamespace::MalAnime),
            providers::IdResolution::Unique(providers::ExternalId::parse("mal:61987").unwrap())
        );
        assert_eq!(
            latest.historical_ids,
            [providers::ExternalId::parse("mal:54492").unwrap()]
        );
        assert_eq!(latest.year, Some(2023));
        let repeated = source_evidence(&preview(json!({"_malId":61987})), Some(&latest), 3);
        assert_eq!(repeated.historical_ids, latest.historical_ids);
    }

    #[test]
    fn simultaneous_claims_stay_ambiguous_and_service_namespaces_stay_separate() {
        let evidence = source_evidence(
            &preview(json!({"mal_id":54492,"_malId":61987,"anilist_id":195516})),
            None,
            1,
        );
        assert!(
            matches!(evidence.ids.resolve_id(providers::IdNamespace::MalAnime), providers::IdResolution::Conflict(ids) if ids.len()==2)
        );
        assert_eq!(
            evidence
                .ids
                .resolve_id(providers::IdNamespace::AnilistAnime),
            providers::IdResolution::Unique(
                providers::ExternalId::parse("anilist:195516").unwrap()
            )
        );
        assert!(evidence.historical_ids.is_empty());
    }

    #[test]
    fn sparse_preview_demotes_ids_to_hints_and_preserves_aliases_and_year() {
        let first = source_evidence(
            &preview(json!({"mal_id":54492,"year":"2023","aliases":["Kusuriya no Hitorigoto"]})),
            None,
            1,
        );
        let sparse = source_evidence(&preview(json!({})), Some(&first), 2);
        assert_eq!(
            sparse.ids.resolve_id(providers::IdNamespace::MalAnime),
            providers::IdResolution::Missing
        );
        assert_eq!(
            sparse.historical_ids,
            [providers::ExternalId::parse("mal:54492").unwrap()]
        );
        assert_eq!(sparse.aliases, first.aliases);
        assert_eq!(sparse.year, first.year);
    }

    #[test]
    fn legacy_combined_evidence_is_readable_and_refresh_separates_its_claims() {
        let legacy: SourceEvidence = serde_json::from_value(json!({
            "ids":{"mal":"61987","typed":[{"namespace":"mal_anime","value":54492},{"namespace":"mal_anime","value":61987}]},
            "title":"The Apothecary Diaries","retrieved_at":1
        })).unwrap();
        assert!(legacy.historical_ids.is_empty());
        assert!(matches!(
            legacy.ids.resolve_id(providers::IdNamespace::MalAnime),
            providers::IdResolution::Conflict(_)
        ));
        let fresh = source_evidence(&preview(json!({"_malId":61987})), Some(&legacy), 2);
        assert_eq!(
            fresh.historical_ids,
            [providers::ExternalId::parse("mal:54492").unwrap()]
        );
        assert!(matches!(
            fresh.ids.resolve_id(providers::IdNamespace::MalAnime),
            providers::IdResolution::Unique(_)
        ));
        let roundtrip: SourceEvidence =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert_eq!(roundtrip.ids, fresh.ids);
        assert_eq!(roundtrip.historical_ids, fresh.historical_ids);
    }

    #[test]
    fn retained_hints_are_bounded_and_exclude_current_and_duplicate_claims() {
        let mut previous = source_evidence(&preview(json!({"mal_id":1})), None, 1);
        previous.historical_ids = (1..=100)
            .map(|id| providers::ExternalId::parse(&format!("mal:{id}")).unwrap())
            .collect();
        let next = source_evidence(&preview(json!({"mal_id":2})), Some(&previous), 2);
        assert_eq!(next.historical_ids.len(), 32);
        assert!(
            !next
                .historical_ids
                .contains(&providers::ExternalId::parse("mal:2").unwrap())
        );
        assert_eq!(
            next.historical_ids
                .iter()
                .filter(|id| **id == providers::ExternalId::parse("mal:1").unwrap())
                .count(),
            1
        );
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

fn tracking_episode_info(videos: &[Video]) -> Vec<nova_tracking::EpisodeInfo> {
    videos
        .iter()
        .map(|v| nova_tracking::EpisodeInfo {
            id: v.id.clone(),
            season: v.season,
            number: v.episode_number(),
            title: v.label(),
            released: tracking_source_date(v.released.as_deref()),
        })
        .collect()
}
fn tracking_source_date(value: Option<&str>) -> nova_tracking::ListDate {
    let mut parts = value.unwrap_or_default().split(['-', 'T']);
    let date = nova_tracking::ListDate {
        year: parts.next().and_then(|v| v.parse().ok()),
        month: parts.next().and_then(|v| v.parse().ok()),
        day: parts.next().and_then(|v| v.parse().ok()),
    };
    if date.valid() {
        date
    } else {
        nova_tracking::ListDate::default()
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn episode_number_or_date_changes_invalidate_the_active_review_but_unrelated_metadata_does_not()
    {
        let handle = TrackingHandle::new();
        let source = SourceRef {
            provider_id: "nova".into(),
            source_id: "merged".into(),
            media_type: "series".into(),
        };
        let episode = nova_tracking::EpisodeInfo {
            id: "stable-id".into(),
            season: Some(2),
            number: Some(8),
            title: "Episode".into(),
            released: nova_tracking::ListDate::default(),
        };
        *handle.context_evidence.lock().unwrap() = Some((source.clone(), vec![episode.clone()]));
        assert_eq!(
            handle.metadata_generation(&source, std::slice::from_ref(&episode)),
            None
        );
        let mut unrelated = source.clone();
        unrelated.source_id = "other".into();
        assert_eq!(handle.metadata_generation(&unrelated, &[]), None);
        let mut changed = episode.clone();
        changed.number = Some(1);
        assert_eq!(handle.metadata_generation(&source, &[changed]), Some(1));
        *handle.context_evidence.lock().unwrap() = Some((source.clone(), vec![episode.clone()]));
        let mut dated = episode;
        dated.released.year = Some(2021);
        assert_eq!(handle.metadata_generation(&source, &[dated]), Some(2));
    }
}

#[cfg(test)]
mod completion_policy_tests {
    use super::*;

    #[test]
    fn preference_boundary_is_captured_once_before_delayed_processing() {
        let handle = TrackingHandle::new();
        let enabled = handle.completion_policy_with(true, || Ok(5)).unwrap();
        let delayed = handle
            .completion_policy_with(true, || panic!("must not resample event 6"))
            .unwrap();
        assert_eq!(delayed.watermark, 5);
        assert_eq!(delayed.revision, enabled.revision);
        let disabled = handle.completion_policy_with(false, || Ok(6)).unwrap();
        let enabled_again = handle.completion_policy_with(true, || Ok(6)).unwrap();
        assert!(disabled.revision > enabled.revision);
        assert!(enabled_again.revision > disabled.revision);
        assert_eq!(enabled_again.watermark, 6);
    }

    #[test]
    fn failed_watermark_read_cannot_enable_a_zero_floor() {
        let handle = TrackingHandle::new();
        assert!(
            handle
                .completion_policy_with(true, || Err("unreadable counter".into()))
                .is_err()
        );
        assert!(handle.completion_policy.lock().unwrap().is_none());
        let recovered = handle.completion_policy_with(true, || Ok(42)).unwrap();
        assert_eq!(recovered.watermark, 42);
    }
}

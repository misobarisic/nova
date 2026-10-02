use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    sync::{Arc, Mutex},
};

use crate::*;

fn n(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}
fn rev(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}
fn source(id: &str) -> SourceRef {
    SourceRef {
        provider_id: "addon".into(),
        source_id: id.into(),
        media_type: "series".into(),
    }
}
fn target(service: Service, media: u32) -> Target {
    Target {
        key: TargetKey {
            account: AccountKey {
                service,
                remote_user_id: n(7),
            },
            media_kind: MediaKind::Anime,
            remote_media_id: n(media),
        },
        remote_entry_id: None,
        final_episode_total: Some(n(12)),
        release_finished: true,
    }
}
fn binding(target: &Target, source: &str, id: &str, assignments: &[(&str, u32)]) -> Binding {
    Binding {
        id: id.into(),
        source: self::source(source),
        target: target.key.clone(),
        account_generation: rev(1),
        mapping_revision: rev(1),
        enabled: true,
        assignments: assignments
            .iter()
            .map(|(id, number)| Assignment {
                episode_id: (*id).into(),
                target_episode: n(*number),
            })
            .collect(),
    }
}
fn episode(binding: &Binding, id: &str) -> SourceEpisode {
    SourceEpisode {
        source: binding.source.clone(),
        episode_id: id.into(),
    }
}
fn state() -> TrackingState {
    let target = target(Service::AniList, 1);
    let binding = binding(
        &target,
        "opaque-source",
        "binding",
        &[("opaque-episode", 1)],
    );
    TrackingState {
        accounts: vec![Account {
            key: target.key.account.clone(),
            generation: rev(1),
            display_name: "User".into(),
        }],
        targets: vec![target],
        bindings: vec![binding],
        projections: vec![],
        outbox: Outbox::default(),
    }
}

#[test]
fn merged_source_has_independent_release_coverage() {
    let a = target(Service::MyAnimeList, 1);
    let b = target(Service::MyAnimeList, 2);
    let bindings = [
        binding(&a, "series", "a", &[("native-12", 12)]),
        binding(&b, "series", "b", &[("native-15", 3)]),
    ];
    validate_bindings(&bindings, &[a.clone(), b.clone()]).unwrap();
    let mut projection = Projection::new(b.key.clone(), rev(1), 0, vec![]);
    projection
        .observe(&bindings[1], &episode(&bindings[1], "native-15"), true)
        .unwrap();
    assert_eq!(
        projection.proposal(&b).unwrap(),
        ProgressProposal {
            progress: 3,
            may_complete: false
        }
    );
    assert_eq!(projection.proposal(&a), Err(ProjectionError::WrongTarget));
}

#[test]
fn overlapping_targets_and_impossible_assignments_are_rejected() {
    let a = target(Service::MyAnimeList, 1);
    let b = target(Service::MyAnimeList, 2);
    let first = binding(&a, "series", "a", &[("native", 1)]);
    let other = binding(&b, "series", "b", &[("native", 2)]);
    assert!(validate_bindings(&[first.clone(), other], &[a.clone(), b]).is_err());
    let anilist = target(Service::AniList, 3);
    let independent = binding(&anilist, "series", "c", &[("native", 8)]);
    validate_bindings(&[first, independent], &[a.clone(), anilist]).unwrap();
    assert!(validate_bindings(&[binding(&a, "series", "bad", &[("native", 13)])], &[a]).is_err());
}

#[test]
fn sparse_events_preserve_remote_progress_and_do_not_replay_history() {
    let target = target(Service::AniList, 1);
    let binding = binding(
        &target,
        "source",
        "binding",
        &[("old", 12), ("new", 8), ("first", 1)],
    );
    let old = episode(&binding, "old");
    let mut projection = Projection::new(
        target.key.clone(),
        rev(1),
        10,
        vec![Observation {
            episode: old.clone(),
            watched: true,
        }],
    );
    assert!(!projection.observe(&binding, &old, true).unwrap());
    projection
        .observe(&binding, &episode(&binding, "new"), true)
        .unwrap();
    assert_eq!(projection.proposal(&target).unwrap().progress, 10);
    assert!(!projection.proposal(&target).unwrap().may_complete);
    projection.observe(&binding, &old, false).unwrap();
    projection.observe(&binding, &old, true).unwrap();
    assert_eq!(projection.proposal(&target).unwrap().progress, 12);
    assert!(projection.proposal(&target).unwrap().may_complete);
    let mut ongoing = target.clone();
    ongoing.release_finished = false;
    assert!(!projection.proposal(&ongoing).unwrap().may_complete);
    ongoing.final_episode_total = None;
    ongoing.release_finished = true;
    assert!(!projection.proposal(&ongoing).unwrap().may_complete);
}

#[test]
fn split_sources_deduplicate_progress_and_local_unwatch_does_not_lower_it() {
    let target = target(Service::AniList, 1);
    let a = binding(&target, "part-a", "a", &[("a", 8)]);
    let b = binding(&target, "part-b", "b", &[("b", 8)]);
    let mut projection = Projection::new(target.key.clone(), rev(1), 0, vec![]);
    projection.observe(&a, &episode(&a, "a"), true).unwrap();
    projection.observe(&b, &episode(&b, "b"), true).unwrap();
    projection.observe(&a, &episode(&a, "a"), false).unwrap();
    assert_eq!(projection.proposal(&target).unwrap().progress, 8);
}

#[test]
fn explicit_decrease_invalidates_old_acknowledgments_and_history() {
    let target = target(Service::AniList, 1);
    let binding = binding(&target, "source", "binding", &[("last", 12)]);
    let last = episode(&binding, "last");
    let mut projection = Projection::new(target.key.clone(), rev(1), 12, vec![]);
    projection.observe(&binding, &last, true).unwrap();
    let old_revision = projection.revision;
    projection
        .replace(
            2,
            vec![Observation {
                episode: last.clone(),
                watched: true,
            }],
        )
        .unwrap();
    assert!(!projection.acknowledge(rev(1), old_revision, 12));
    assert!(!projection.refresh_remote(rev(1), old_revision, 12));
    assert!(!projection.observe(&binding, &last, true).unwrap());
    assert_eq!(projection.proposal(&target).unwrap().progress, 2);
}

#[test]
fn observations_are_scoped_to_account_generation_and_enabled_mapping() {
    let target = target(Service::AniList, 1);
    let mut binding = binding(&target, "source", "binding", &[("episode", 1)]);
    let episode = episode(&binding, "episode");
    let mut projection = Projection::new(target.key.clone(), rev(2), 0, vec![]);
    assert_eq!(
        projection.observe(&binding, &episode, true),
        Err(ProjectionError::StaleAccount)
    );
    assert!(!projection.acknowledge(rev(1), rev(1), 12));
    assert!(!projection.refresh_remote(rev(1), rev(1), 12));
    binding.account_generation = rev(2);
    binding.enabled = false;
    assert_eq!(
        projection.observe(&binding, &episode, true),
        Err(ProjectionError::DisabledBinding)
    );
    binding.enabled = true;
    binding.target.account.remote_user_id = n(8);
    assert_eq!(
        projection.observe(&binding, &episode, true),
        Err(ProjectionError::WrongTarget)
    );
    assert_eq!(projection.proposal(&target).unwrap().progress, 0);
}

#[derive(Default)]
struct Memory {
    rows: BTreeMap<String, String>,
    fail: bool,
}
#[derive(Clone, Default)]
struct MemoryStorage(Arc<Mutex<Memory>>);
impl StateStorage for MemoryStorage {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
        Ok(self.0.lock().unwrap().rows.get(key).cloned())
    }
    fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
        let mut memory = self.0.lock().unwrap();
        if memory.fail {
            return Err(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                "injected failure",
            ));
        }
        for (key, value) in entries {
            match value {
                Some(value) => {
                    memory.rows.insert(key.clone(), value.clone());
                }
                None => {
                    memory.rows.remove(key);
                }
            }
        }
        Ok(())
    }
}

#[test]
fn persistence_round_trips_and_failed_commit_preserves_last_state() {
    let storage = MemoryStorage::default();
    let mut store = Store::load(storage.clone()).unwrap();
    let mut state = state();
    state.projections.push(Projection::new(
        state.targets[0].key.clone(),
        rev(1),
        8,
        vec![],
    ));
    store.save(state.clone()).unwrap();
    assert_eq!(Store::load(storage.clone()).unwrap().state(), &state);
    storage.0.lock().unwrap().fail = true;
    assert!(store.save(TrackingState::default()).is_err());
    assert_eq!(store.state(), &state);
    assert_eq!(Store::load(storage.clone()).unwrap().state(), &state);
}

#[test]
fn corrupt_records_are_quarantined_once_without_wiping_primary() {
    let storage = MemoryStorage::default();
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some("broken JSON".into()))])
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            Store::load(storage.clone()),
            Err(LoadError::InvalidState(_))
        ));
    }
    let memory = storage.0.lock().unwrap();
    assert_eq!(memory.rows[TRACKING_STATE_KEY], "broken JSON");
    assert_eq!(
        memory
            .rows
            .keys()
            .filter(|key| key.starts_with("tracking:quarantine:"))
            .count(),
        1
    );
}

#[test]
fn newer_schemas_and_semantically_invalid_records_are_preserved() {
    let storage = MemoryStorage::default();
    let raw = r#"{"version":3,"state":"new schema"}"#;
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(raw.into()))])
        .unwrap();
    assert!(matches!(
        Store::load(storage.clone()),
        Err(LoadError::UnsupportedVersion(3))
    ));
    assert_eq!(
        storage.read(TRACKING_STATE_KEY).unwrap().as_deref(),
        Some(raw)
    );
    let mut invalid = state();
    invalid.accounts.clear();
    let raw = serde_json::json!({"version":1,"state":invalid}).to_string();
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(raw))])
        .unwrap();
    assert!(matches!(
        Store::load(storage),
        Err(LoadError::InvalidState(_))
    ));
}

#[test]
fn restart_preserves_link_checkpoint_and_explicit_replacement_revision() {
    let storage = MemoryStorage::default();
    let mut state = state();
    state.bindings[0].assignments[0].target_episode = n(12);
    let binding = state.bindings[0].clone();
    let episode = episode(&binding, "opaque-episode");
    let target = state.targets[0].clone();
    let snapshot = vec![Observation {
        episode: episode.clone(),
        watched: true,
    }];
    let mut projection = Projection::new(target.key.clone(), rev(1), 12, snapshot.clone());
    projection.replace(2, snapshot).unwrap();
    state.projections.push(projection);
    Store::load(storage.clone()).unwrap().save(state).unwrap();
    let restored = Store::load(storage).unwrap();
    let mut projection = restored.state().projections[0].clone();
    assert!(!projection.observe(&binding, &episode, true).unwrap());
    assert!(!projection.acknowledge(rev(1), rev(1), 12));
    assert_eq!(projection.proposal(&target).unwrap().progress, 2);
}

#[test]
fn invalid_checkpoints_and_stale_account_records_cannot_be_committed() {
    let storage = MemoryStorage::default();
    let mut store = Store::load(storage.clone()).unwrap();
    let mut state = state();
    let observation = Observation {
        episode: episode(&state.bindings[0], "opaque-episode"),
        watched: true,
    };
    state.projections.push(Projection::new(
        state.targets[0].key.clone(),
        rev(1),
        0,
        vec![observation.clone(), observation],
    ));
    assert!(store.save(state.clone()).is_err());
    assert!(storage.read(TRACKING_STATE_KEY).unwrap().is_none());
    state.projections.clear();
    state.accounts[0].generation = rev(2);
    assert!(store.save(state).is_err());
    assert!(store.state().accounts.is_empty());
}

fn delivery_state() -> TrackingState {
    let mut state = state();
    state.bindings[0].assignments = [1, 2, 8, 12]
        .into_iter()
        .map(|ordinal| Assignment {
            episode_id: format!("episode-{ordinal}"),
            target_episode: n(ordinal),
        })
        .collect();
    state.projections.push(Projection::new(
        state.targets[0].key.clone(),
        rev(1),
        0,
        vec![],
    ));
    state
}
fn delivery_store(storage: &MemoryStorage) -> Store<MemoryStorage> {
    let mut store = Store::load(storage.clone()).unwrap();
    store.save(delivery_state()).unwrap();
    store
}
fn watch(store: &mut Store<MemoryStorage>, ordinal: u32) -> NonZeroU64 {
    let binding = store.state().bindings[0].clone();
    store
        .observe_episode(
            &binding.id,
            &episode(&binding, &format!("episode-{ordinal}")),
            true,
        )
        .unwrap()
        .unwrap()
}

#[test]
fn outbox_and_watched_checkpoint_commit_together_and_recover_after_restart() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    let episode = episode(&store.state().bindings[0], "episode-8");
    storage.0.lock().unwrap().fail = true;
    assert!(store.observe_episode("binding", &episode, true).is_err());
    assert!(store.state().outbox.pending().is_empty());
    assert_eq!(
        store.state().projections[0]
            .proposal(&store.state().targets[0])
            .unwrap()
            .progress,
        0
    );
    storage.0.lock().unwrap().fail = false;
    store
        .observe_episode("binding", &episode, true)
        .unwrap()
        .unwrap();
    let mut restored = Store::load(storage).unwrap();
    assert_eq!(restored.state().outbox.pending().len(), 1);
    assert_eq!(
        restored.observe_episode("binding", &episode, true).unwrap(),
        None
    );
    assert_eq!(restored.state().outbox.pending()[0].progress, 8);
}

#[test]
fn automatic_updates_coalesce_without_clearing_work_queued_during_delivery() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    let first_revision = watch(&mut store, 1);
    let revision = watch(&mut store, 2);
    assert!(revision > first_revision);
    assert_eq!(store.state().outbox.pending().len(), 1);
    let target = store.state().targets[0].key.clone();
    let first = store.begin_delivery(&target, 0).unwrap().unwrap();
    assert_eq!(first.patch().progress, 2);
    assert_eq!(first.progress_for_remote(10), 10);
    watch(&mut store, 8);
    assert_eq!(store.state().outbox.pending().len(), 2);
    assert!(store.begin_delivery(&target, 0).unwrap().is_none());
    store
        .finish_delivery(&first, DeliveryOutcome::Applied { progress: 2 }, 0)
        .unwrap();
    assert_eq!(store.state().outbox.pending().len(), 1);
    let second = store.begin_delivery(&target, 0).unwrap().unwrap();
    assert_eq!(second.patch().progress, 8);
    assert!(matches!(
        store.finish_delivery(&first, DeliveryOutcome::Applied { progress: 2 }, 0),
        Err(TrackingError::StaleAttempt)
    ));
}

#[test]
fn explicit_decrease_supersedes_unsent_updates_and_ignores_older_acknowledgments() {
    for failure in [false, true] {
        let storage = MemoryStorage::default();
        let mut store = delivery_store(&storage);
        watch(&mut store, 12);
        let target = store.state().targets[0].clone();
        let old = store.begin_delivery(&target.key, 0).unwrap().unwrap();
        store.replace_progress(&target.key, 2, vec![]).unwrap();
        assert!(store.begin_delivery(&target.key, 0).unwrap().is_none());
        let outcome = if failure {
            DeliveryOutcome::Failed(DeliveryFailure::Transient)
        } else {
            DeliveryOutcome::Applied { progress: 12 }
        };
        store.finish_delivery(&old, outcome, 0).unwrap();
        assert_eq!(
            store.state().projections[0]
                .proposal(&target)
                .unwrap()
                .progress,
            2
        );
        assert_eq!(store.state().outbox.pending().len(), 1);
        let edit = store.begin_delivery(&target.key, 0).unwrap().unwrap();
        assert_eq!(edit.patch().intent, ProgressIntent::ExplicitReplacement);
        assert_eq!(edit.progress_for_remote(12), 2);
        store
            .finish_delivery(
                &edit,
                DeliveryOutcome::Failed(DeliveryFailure::Transient),
                0,
            )
            .unwrap();
        let retry_at = store.state().outbox.pending()[0].next_attempt_at;
        let retry = store
            .begin_delivery(&target.key, retry_at)
            .unwrap()
            .unwrap();
        assert_eq!(retry.progress_for_remote(12), 2);
    }
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 8);
    let target = store.state().targets[0].key.clone();
    store.replace_progress(&target, 2, vec![]).unwrap();
    assert_eq!(store.state().outbox.pending().len(), 1);
    assert_eq!(store.state().outbox.pending()[0].progress, 2);
}

#[test]
fn failed_begin_commit_never_returns_a_sendable_attempt() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 1);
    let target = store.state().targets[0].key.clone();
    storage.0.lock().unwrap().fail = true;
    assert!(store.begin_delivery(&target, 0).is_err());
    assert_eq!(
        store.state().outbox.pending()[0].state,
        DeliveryState::Queued
    );
    assert_eq!(store.state().outbox.pending()[0].attempts, 0);
}

#[test]
fn interrupted_attempt_is_uncertain_and_old_completion_cannot_clear_retry() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 8);
    let target = store.state().targets[0].key.clone();
    let old = store.begin_delivery(&target, 0).unwrap().unwrap();
    let mut restored = Store::load(storage).unwrap();
    assert_eq!(
        restored.state().outbox.pending()[0].state,
        DeliveryState::Uncertain
    );
    let retry = restored.begin_delivery(&target, 0).unwrap().unwrap();
    assert_eq!(retry.patch().attempts, old.patch().attempts + 1);
    assert_eq!(retry.progress_for_remote(12), 12);
    assert!(matches!(
        restored.finish_delivery(&old, DeliveryOutcome::Applied { progress: 8 }, 0),
        Err(TrackingError::StaleAttempt)
    ));
    restored
        .finish_delivery(&retry, DeliveryOutcome::Applied { progress: 12 }, 0)
        .unwrap();
    assert!(restored.state().outbox.pending().is_empty());
}

#[test]
fn mapping_changes_pause_projection_and_pending_work_even_without_revision_bump() {
    for bump_revision in [false, true] {
        let storage = MemoryStorage::default();
        let mut store = delivery_store(&storage);
        watch(&mut store, 1);
        let mut state = store.state().clone();
        let target = state.targets[0].key.clone();
        if bump_revision {
            state.bindings[0].mapping_revision = rev(2);
        } else {
            state.bindings[0].assignments[1].target_episode = n(3);
        }
        store.save(state).unwrap();
        let binding = store.state().bindings[0].clone();
        assert!(matches!(
            store.observe_episode(&binding.id, &episode(&binding, "episode-2"), true),
            Err(TrackingError::Projection(ProjectionError::StaleMapping))
        ));
        assert!(store.begin_delivery(&target, 0).unwrap().is_none());
        assert_eq!(
            store.state().outbox.pending()[0].state,
            DeliveryState::NeedsAlignment
        );
        assert_eq!(
            Store::load(storage).unwrap().state().outbox.pending()[0].state,
            DeliveryState::NeedsAlignment
        );
    }
}

#[test]
fn account_generation_change_cannot_send_old_pending_work() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 1);
    let mut state = store.state().clone();
    state.accounts[0].generation = rev(2);
    state.bindings[0].enabled = false;
    state.projections[0].account_generation = rev(2);
    let target = state.targets[0].key.clone();
    store.save(state).unwrap();
    assert!(store.begin_delivery(&target, 0).unwrap().is_none());
    assert_eq!(
        store.state().outbox.pending()[0].state,
        DeliveryState::StaleAccount
    );
}

fn add_delivery_target(state: &mut TrackingState, service: Service, id: u32) -> Target {
    let target = target(service, id);
    if !state
        .accounts
        .iter()
        .any(|account| account.key == target.key.account)
    {
        state.accounts.push(Account {
            key: target.key.account.clone(),
            generation: rev(1),
            display_name: "Other user".into(),
        });
    }
    state.bindings.push(binding(
        &target,
        "other-source",
        &format!("binding-{id}"),
        &[("other-episode", 1)],
    ));
    state
        .projections
        .push(Projection::new(target.key.clone(), rev(1), 0, vec![]));
    state.targets.push(target.clone());
    target
}
fn watch_other(store: &mut Store<MemoryStorage>, id: u32) {
    let binding = store
        .state()
        .bindings
        .iter()
        .find(|binding| binding.id == format!("binding-{id}"))
        .unwrap()
        .clone();
    store
        .observe_episode(&binding.id, &episode(&binding, "other-episode"), true)
        .unwrap();
}

#[test]
fn server_cooldown_is_service_wide_persistent_and_cannot_be_bypassed_by_retry() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    let mut state = store.state().clone();
    let second = add_delivery_target(&mut state, Service::AniList, 2);
    let other_service = add_delivery_target(&mut state, Service::MyAnimeList, 3);
    store.save(state).unwrap();
    watch(&mut store, 1);
    watch_other(&mut store, 2);
    watch_other(&mut store, 3);
    let first = store.state().targets[0].key.clone();
    let attempt = store.begin_delivery(&first, 0).unwrap().unwrap();
    store
        .finish_delivery(
            &attempt,
            DeliveryOutcome::Failed(DeliveryFailure::RateLimited { retry_at: 100 }),
            0,
        )
        .unwrap();
    store.replace_progress(&first, 0, vec![]).unwrap();
    store.retry_target(&first, 1).unwrap();
    let mut store = Store::load(storage).unwrap();
    assert!(store.begin_delivery(&first, 1).unwrap().is_none());
    assert!(store.begin_delivery(&second.key, 1).unwrap().is_none());
    assert!(
        store
            .begin_delivery(&other_service.key, 1)
            .unwrap()
            .is_some()
    );
    assert!(store.begin_delivery(&second.key, 100).unwrap().is_some());
}

#[test]
fn authentication_failure_pauses_the_account_until_verified_resume() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    let mut state = store.state().clone();
    let second = add_delivery_target(&mut state, Service::AniList, 2);
    store.save(state).unwrap();
    watch(&mut store, 1);
    watch_other(&mut store, 2);
    let first = store.state().targets[0].key.clone();
    let attempt = store.begin_delivery(&first, 0).unwrap().unwrap();
    store
        .finish_delivery(
            &attempt,
            DeliveryOutcome::Failed(DeliveryFailure::AuthenticationRequired),
            0,
        )
        .unwrap();
    let mut store = Store::load(storage).unwrap();
    assert!(store.begin_delivery(&second.key, 0).unwrap().is_none());
    assert!(!store.retry_target(&first, 0).unwrap());
    assert!(matches!(
        store.resume_account(&first.account, rev(2)),
        Err(TrackingError::StaleAccount)
    ));
    store.resume_account(&first.account, rev(1)).unwrap();
    assert!(store.begin_delivery(&first, 0).unwrap().is_some());
    assert!(store.begin_delivery(&second.key, 0).unwrap().is_some());
}

#[test]
fn rejected_values_remain_paused_until_an_explicit_replacement() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 8);
    let target = store.state().targets[0].key.clone();
    let attempt = store.begin_delivery(&target, 0).unwrap().unwrap();
    store
        .finish_delivery(
            &attempt,
            DeliveryOutcome::Failed(DeliveryFailure::Rejected),
            0,
        )
        .unwrap();
    assert!(!store.retry_target(&target, 100).unwrap());
    assert!(store.begin_delivery(&target, 100).unwrap().is_none());
    store.replace_progress(&target, 2, vec![]).unwrap();
    assert_eq!(
        store
            .begin_delivery(&target, 100)
            .unwrap()
            .unwrap()
            .patch()
            .progress,
        2
    );
}

#[test]
fn failed_acknowledgment_commit_retains_the_exact_attempt_for_recovery() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 1);
    let target = store.state().targets[0].key.clone();
    let attempt = store.begin_delivery(&target, 0).unwrap().unwrap();
    storage.0.lock().unwrap().fail = true;
    assert!(
        store
            .finish_delivery(&attempt, DeliveryOutcome::Applied { progress: 1 }, 0)
            .is_err()
    );
    assert_eq!(store.state().outbox.pending()[0], *attempt.patch());
    storage.0.lock().unwrap().fail = false;
    store
        .finish_delivery(&attempt, DeliveryOutcome::Applied { progress: 1 }, 0)
        .unwrap();
    assert!(
        Store::load(storage)
            .unwrap()
            .state()
            .outbox
            .pending()
            .is_empty()
    );
}

#[test]
fn transient_backoff_survives_coalescing_and_is_bounded() {
    let storage = MemoryStorage::default();
    let mut store = delivery_store(&storage);
    watch(&mut store, 1);
    let target = store.state().targets[0].key.clone();
    let attempt = store.begin_delivery(&target, 0).unwrap().unwrap();
    store
        .finish_delivery(
            &attempt,
            DeliveryOutcome::Failed(DeliveryFailure::Transient),
            0,
        )
        .unwrap();
    let next_attempt_at = store.state().outbox.pending()[0].next_attempt_at;
    watch(&mut store, 8);
    assert_eq!(store.state().outbox.pending().len(), 1);
    assert_eq!(
        store.state().outbox.pending()[0].next_attempt_at,
        next_attempt_at
    );
    assert!(
        store
            .begin_delivery(&target, next_attempt_at - 1)
            .unwrap()
            .is_none()
    );
    let mut now = next_attempt_at;
    for _ in 0..15 {
        let attempt = store.begin_delivery(&target, now).unwrap().unwrap();
        store
            .finish_delivery(
                &attempt,
                DeliveryOutcome::Failed(DeliveryFailure::Transient),
                now,
            )
            .unwrap();
        let retry_at = store.state().outbox.pending()[0].next_attempt_at;
        assert!(retry_at > now);
        assert!(retry_at - now <= 4320);
        now = retry_at;
    }
}

#[test]
fn schema_one_upgrades_on_save_and_schema_two_requires_an_outbox() {
    let storage = MemoryStorage::default();
    let state = delivery_state();
    let mut legacy = serde_json::json!({"version":1,"state":state});
    legacy["state"].as_object_mut().unwrap().remove("outbox");
    for projection in legacy["state"]["projections"].as_array_mut().unwrap() {
        projection.as_object_mut().unwrap().remove("mappings");
    }
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(legacy.to_string()))])
        .unwrap();
    let mut store = Store::load(storage.clone()).unwrap();
    assert!(store.state().outbox.pending().is_empty());
    watch(&mut store, 1);
    let current: serde_json::Value =
        serde_json::from_str(&storage.read(TRACKING_STATE_KEY).unwrap().unwrap()).unwrap();
    assert_eq!(current["version"], 2);
    legacy["version"] = serde_json::json!(2);
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(legacy.to_string()))])
        .unwrap();
    assert!(matches!(
        Store::load(storage),
        Err(LoadError::InvalidState(_))
    ));
}

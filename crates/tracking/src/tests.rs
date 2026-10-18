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
        bulk_origin: false,
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
        link_checkpoints: vec![],
        snapshots: vec![],
        automatic_paused: vec![],
        completion: CompletionPrompts::default(),
        active_accounts: vec![AccountKey {
            service: Service::AniList,
            remote_user_id: n(7),
        }],
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
fn scope_revocation_rebuilds_only_surviving_offline_progress() {
    for (explicit_progress, bulk_progress) in [(1, 12), (12, 5)] {
        let mut state = delivery_state();
        let key = state.targets[0].key.clone();
        let explicit = binding(
            &state.targets[0],
            "explicit",
            "a",
            &[("a", explicit_progress)],
        );
        let mut bulk = binding(&state.targets[0], "bulk", "c", &[("c", bulk_progress)]);
        bulk.bulk_origin = true;
        state.bindings = vec![explicit.clone(), bulk.clone()];
        state
            .observe_episode("a", &episode(&explicit, "a"), true)
            .unwrap();
        state
            .observe_episode("c", &episode(&bulk, "c"), true)
            .unwrap();
        state
            .restrict_automatic_sources(&key, std::slice::from_ref(&explicit.source))
            .unwrap();
        assert!(
            state.bindings.iter().all(|b| b.enabled),
            "scope must preserve mappings"
        );
        let pending = state
            .outbox
            .pending()
            .iter()
            .find(|p| p.target == key)
            .unwrap();
        assert_eq!(pending.progress, explicit_progress);
        assert_eq!(pending.intent, ProgressIntent::AutomaticForward);
        assert_eq!(pending.mappings.len(), 1);
        assert_eq!(pending.mappings[0].source, explicit.source);
        assert_eq!(
            state.projections[0]
                .proposal(&state.targets[0])
                .unwrap()
                .progress,
            explicit_progress
        );
        state.validate().unwrap();
    }
}

#[test]
fn scope_revocation_preserves_manual_fields_and_only_authorized_completion_ratings() {
    for completion_source in [None, Some("explicit"), Some("bulk")] {
        let mut state = delivery_state();
        let key = state.targets[0].key.clone();
        let explicit = binding(&state.targets[0], "explicit", "a", &[("a", 1)]);
        let mut bulk = binding(&state.targets[0], "bulk", "c", &[("c", 12)]);
        bulk.bulk_origin = true;
        state.bindings = vec![explicit.clone(), bulk.clone()];
        state.snapshots.push(snapshot(&state));
        state
            .observe_episode("a", &episode(&explicit, "a"), true)
            .unwrap();
        state
            .observe_episode("c", &episode(&bulk, "c"), true)
            .unwrap();
        let mut patch = EntryPatch {
            score_tenths: Some(85),
            ..Default::default()
        };
        if completion_source.is_none() {
            patch.status = Some(ListStatus::OnHold);
            patch.started = Some(ListDate {
                year: Some(2026),
                month: Some(10),
                day: Some(18),
            });
        }
        state.enqueue_edit(&key, patch).unwrap();
        let edit = &mut state.outbox.edits[0];
        edit.attempts = 3;
        edit.next_attempt_at = 77;
        if let Some(source) = completion_source {
            edit.only_if_unrated = true;
            edit.completion_mappings = vec![MappingStamp::from_binding(if source == "explicit" {
                &explicit
            } else {
                &bulk
            })];
        }
        let expected = edit.clone();
        state
            .restrict_automatic_sources(&key, std::slice::from_ref(&explicit.source))
            .unwrap();
        if completion_source == Some("bulk") {
            assert!(
                state.outbox.edits().is_empty(),
                "revoked completion permission must not survive"
            );
        } else {
            assert_eq!(
                state.outbox.edits(),
                &[expected],
                "scope changes must retain independent fields and retry state"
            );
        }
        state.validate().unwrap();
        state.restrict_automatic_sources(&key, &[]).unwrap();
        assert!(
            state.outbox.edits().is_empty(),
            "no remaining allowed link revokes unsent field work"
        );
        assert!(state.outbox.pending().is_empty());
    }
}

#[test]
fn scope_revocation_preserves_remote_baseline_and_checkpoint_allows_rewatch() {
    let target = target(Service::AniList, 1);
    let explicit = binding(&target, "explicit", "a", &[("a", 1)]);
    let bulk = binding(&target, "bulk", "c", &[("c", 12)]);
    let watched = episode(&bulk, "c");
    let mut projection = Projection::new(target.key.clone(), rev(1), 8, vec![]);
    projection.observe(&bulk, &watched, true).unwrap();
    projection
        .retain_authorized_sources(std::slice::from_ref(&explicit.source), true)
        .unwrap();
    assert_eq!(projection.proposal(&target).unwrap().progress, 8);
    projection.checkpoint(&bulk, &watched, false).unwrap();
    assert_eq!(projection.proposal(&target).unwrap().progress, 8);
    assert!(projection.observe(&bulk, &watched, true).unwrap());
    assert_eq!(projection.proposal(&target).unwrap().progress, 12);
}

#[test]
fn legacy_unattributed_accepted_scalar_is_revoked_fail_closed() {
    let target = target(Service::AniList, 1);
    let old = binding(&target, "bulk", "c", &[("c", 12)]);
    let mut projection = Projection::new(target.key.clone(), rev(1), 3, vec![]);
    projection.observe(&old, &episode(&old, "c"), true).unwrap();
    let mut value = serde_json::to_value(&projection).unwrap();
    value.as_object_mut().unwrap().remove("accepted_by_source");
    value["mappings"] = serde_json::json!([]);
    let mut migrated: Projection = serde_json::from_value(value).unwrap();
    assert!(migrated.retain_authorized_sources(&[], true).unwrap());
    assert_eq!(migrated.proposal(&target).unwrap().progress, 3);
}

#[test]
fn source_contribution_schema_rejects_duplicates_invalid_sources_and_missing_current_field() {
    let mut state = delivery_state();
    let b = state.bindings[0].clone();
    state
        .observe_episode(&b.id, &episode(&b, "episode-12"), true)
        .unwrap();
    let value = serde_json::to_value(&state).unwrap();
    for invalid in ["duplicate", "empty"] {
        let mut bad = value.clone();
        if invalid == "duplicate" {
            let row = bad["projections"][0]["accepted_by_source"][0].clone();
            bad["projections"][0]["accepted_by_source"]
                .as_array_mut()
                .unwrap()
                .push(row);
        } else {
            bad["projections"][0]["accepted_by_source"][0]["source"]["source_id"] = "".into();
        }
        let decoded: TrackingState = serde_json::from_value(bad).unwrap();
        assert!(decoded.validate().is_err());
    }
    let mut without = value;
    without["projections"][0]
        .as_object_mut()
        .unwrap()
        .remove("accepted_by_source");
    for version in [4, 5] {
        let storage = MemoryStorage::default();
        storage
            .write(&[(
                TRACKING_STATE_KEY.into(),
                Some(serde_json::json!({"version":version,"state":without}).to_string()),
            )])
            .unwrap();
        assert_eq!(Store::load(storage).is_ok(), version == 4);
    }
    let mut without_owner = serde_json::to_value(&state).unwrap();
    without_owner["bindings"][0]
        .as_object_mut()
        .unwrap()
        .remove("bulk_origin");
    for version in [4, 5] {
        let storage = MemoryStorage::default();
        storage
            .write(&[(
                TRACKING_STATE_KEY.into(),
                Some(serde_json::json!({"version":version,"state":without_owner}).to_string()),
            )])
            .unwrap();
        assert_eq!(Store::load(storage).is_ok(), version == 4);
    }
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
    let raw = r#"{"version":99,"state":"new schema"}"#;
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(raw.into()))])
        .unwrap();
    assert!(matches!(
        Store::load(storage.clone()),
        Err(LoadError::UnsupportedVersion(99))
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
    if !state.active_accounts.contains(&target.key.account) {
        state.active_accounts.push(target.key.account.clone());
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
    assert_eq!(current["version"], 5);
    legacy["version"] = serde_json::json!(2);
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(legacy.to_string()))])
        .unwrap();
    assert!(matches!(
        Store::load(storage),
        Err(LoadError::InvalidState(_))
    ));
}

#[test]
fn link_cursor_suppresses_old_events_and_paired_progress_is_ineligible() {
    let mut state = delivery_state();
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: state.bindings[0].id.clone(),
        sequence: 10,
    });
    let event = |sequence, origin, watched| WatchEvent {
        sequence,
        origin,
        date: None,
        changes: vec![WatchChange {
            series_id: state.bindings[0].source.source_id.clone(),
            episode_id: state.bindings[0].assignments[0].episode_id.clone(),
            watched,
            started: false,
        }],
    };
    let old = event(9, EventOrigin::Local, true);
    let paired = event(11, EventOrigin::PairedDevice, true);
    let same = event(12, EventOrigin::Local, true);
    let unwatch = event(13, EventOrigin::Local, false);
    let rewatch = event(14, EventOrigin::Local, true);
    state.consume_event(&old).unwrap();
    assert!(state.outbox.pending().is_empty());
    state.consume_event(&paired).unwrap();
    assert!(state.outbox.pending().is_empty());
    state.consume_event(&same).unwrap();
    assert!(state.outbox.pending().is_empty());
    state.consume_event(&unwatch).unwrap();
    state.consume_event(&rewatch).unwrap();
    assert_eq!(state.outbox.pending().len(), 1);
}

#[test]
fn event_consumption_and_queue_commit_or_fail_together() {
    let storage = MemoryStorage::default();
    let mut store = Store::load(storage.clone()).unwrap();
    let mut state = delivery_state();
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: state.bindings[0].id.clone(),
        sequence: 0,
    });
    store.save(state.clone()).unwrap();
    let event = WatchEvent {
        sequence: 1,
        origin: EventOrigin::Local,
        date: None,
        changes: vec![WatchChange {
            series_id: state.bindings[0].source.source_id.clone(),
            episode_id: state.bindings[0].assignments[0].episode_id.clone(),
            watched: true,
            started: false,
        }],
    };
    let key = format!("{EVENT_PREFIX}{:020}", 1);
    storage
        .write(&[(key.clone(), Some(serde_json::to_string(&event).unwrap()))])
        .unwrap();
    state.consume_event(&event).unwrap();
    storage.0.lock().unwrap().fail = true;
    assert!(
        store
            .save_with_events(state.clone(), std::slice::from_ref(&key))
            .is_err()
    );
    assert!(store.state().outbox.pending().is_empty());
    assert!(storage.read(&key).unwrap().is_some());
    storage.0.lock().unwrap().fail = false;
    store
        .save_with_events(state, std::slice::from_ref(&key))
        .unwrap();
    assert!(storage.read(&key).unwrap().is_none());
    assert_eq!(
        Store::load(storage).unwrap().state().outbox.pending().len(),
        1
    );
}

fn snapshot(state: &TrackingState) -> TargetSnapshot {
    let target = state.targets[0].key.clone();
    TargetSnapshot {
        media: Media {
            id: target.remote_media_id,
            mal_id: None,
            title: "Anime".into(),
            format: "TV".into(),
            episodes: Some(n(12)),
            finished: true,
            year: Some(2026),
        },
        target,
        remote: None,
        score_format: ScoreFormat::Point10Decimal,
        status_pinned: false,
        dates_pinned: false,
    }
}
#[test]
fn rejected_field_does_not_block_a_new_independent_edit() {
    let mut state = delivery_state();
    state.snapshots.push(snapshot(&state));
    let key = state.targets[0].key.clone();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                score_tenths: Some(85),
                ..Default::default()
            },
        )
        .unwrap();
    let score = state.begin_edit(&key, 0).unwrap().unwrap();
    state
        .finish_edit(&score, Err(DeliveryFailure::Rejected), 0)
        .unwrap();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                status: Some(ListStatus::OnHold),
                ..Default::default()
            },
        )
        .unwrap();
    let status = state
        .begin_edit(&key, 1)
        .unwrap()
        .expect("new status must be deliverable");
    assert_eq!(status.edit().patch.status, Some(ListStatus::OnHold));
    assert_eq!(status.edit().patch.score_tenths, None);
    assert_eq!(state.outbox.edits()[0].state, DeliveryState::Rejected);
    state.validate().unwrap();
}

#[test]
fn field_edits_preserve_independent_fields_and_serialize_with_progress() {
    let mut state = delivery_state();
    state.snapshots.push(snapshot(&state));
    let key = state.targets[0].key.clone();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                status: Some(ListStatus::OnHold),
                score_tenths: Some(85),
                ..Default::default()
            },
        )
        .unwrap();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                status: Some(ListStatus::Dropped),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(state.outbox.edits().len(), 2);
    assert_eq!(state.outbox.edits()[0].patch.status, None);
    assert_eq!(state.outbox.edits()[0].patch.score_tenths, Some(85));
    let attempt = state.begin_edit(&key, 0).unwrap().unwrap();
    state
        .observe_episode(
            &state.bindings[0].id.clone(),
            &episode(&state.bindings[0], "episode-1"),
            true,
        )
        .unwrap();
    assert!(state.begin_delivery(&key, 0).unwrap().is_none());
    state
        .finish_edit(&attempt, Err(DeliveryFailure::Transient), 0)
        .unwrap();
    assert!(state.snapshots[0].status_pinned);
    state.validate().unwrap();
}
#[test]
fn newer_field_edit_survives_failed_old_request_and_account_auth_pause() {
    let mut state = delivery_state();
    state.snapshots.push(snapshot(&state));
    let key = state.targets[0].key.clone();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                score_tenths: Some(80),
                ..Default::default()
            },
        )
        .unwrap();
    let old = state.begin_edit(&key, 0).unwrap().unwrap();
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                score_tenths: Some(90),
                ..Default::default()
            },
        )
        .unwrap();
    state
        .finish_edit(&old, Err(DeliveryFailure::Transient), 0)
        .unwrap();
    assert_eq!(state.outbox.edits().len(), 1);
    assert_eq!(state.outbox.edits()[0].patch.score_tenths, Some(90));
    let current = state.begin_edit(&key, 0).unwrap().unwrap();
    state
        .finish_edit(&current, Err(DeliveryFailure::AuthenticationRequired), 0)
        .unwrap();
    assert!(state.begin_edit(&key, 100).unwrap().is_none());
    state.resume_account(&key.account, rev(1)).unwrap();
    assert!(state.begin_edit(&key, 100).unwrap().is_some());
    state.validate().unwrap();
}

#[test]
fn automatic_fields_preserve_pins_and_unknown_release_completion() {
    let state = delivery_state();
    let mut snapshot = snapshot(&state);
    let mut target = state.targets[0].clone();
    let date = ListDate {
        year: Some(2026),
        month: Some(10),
        day: Some(2),
    };
    let patch = EntryPatch::automatic(&target, &snapshot, None, 12, Some(date), true);
    assert_eq!(patch.status, Some(ListStatus::Completed));
    assert_eq!(patch.started, Some(date));
    assert_eq!(patch.completed, Some(date));
    assert_eq!(patch.score_tenths, None);
    target.release_finished = false;
    assert_eq!(
        EntryPatch::automatic(&target, &snapshot, None, 12, Some(date), true).status,
        Some(ListStatus::Watching)
    );
    target.release_finished = true;
    snapshot.status_pinned = true;
    snapshot.dates_pinned = true;
    let pinned = EntryPatch::automatic(&target, &snapshot, None, 12, Some(date), true);
    assert_eq!(pinned.status, None);
    assert_eq!(pinned.started, None);
    assert_eq!(pinned.completed, None);
    snapshot.status_pinned = false;
    snapshot.dates_pinned = false;
    let held = RemoteEntry {
        account: target.key.account.clone(),
        media_id: target.key.remote_media_id,
        entry_id: Some(n(99)),
        progress: 5,
        status: ListStatus::OnHold,
        score_tenths: 85,
        started: date,
        completed: ListDate::default(),
        repeating: false,
    };
    let patch = EntryPatch::automatic(&target, &snapshot, Some(&held), 12, Some(date), true);
    assert_eq!(patch.status, None);
    assert_eq!(patch.started, None);
    assert_eq!(patch.completed, None);
    assert_eq!(
        EntryPatch::automatic(&target, &snapshot, None, 12, Some(date), false).status,
        Some(ListStatus::Watching)
    );
}

#[test]
fn explicit_recovery_retains_corrupt_state_before_reset() {
    let storage = MemoryStorage::default();
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some("broken-json".into()))])
        .unwrap();
    assert!(Store::load(storage.clone()).is_err());
    let store = Store::reset_retaining_backup(storage.clone()).unwrap();
    assert!(store.state().bindings.is_empty());
    let memory = storage.0.lock().unwrap();
    assert!(
        memory
            .rows
            .iter()
            .any(|(key, value)| key.starts_with("tracking:quarantine:") && value == "broken-json")
    );
}

#[test]
fn inactive_account_events_checkpoint_without_upload() {
    let mut state = delivery_state();
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: state.bindings[0].id.clone(),
        sequence: 0,
    });
    state.active_accounts.clear();
    let event = WatchEvent {
        sequence: 1,
        origin: EventOrigin::Local,
        date: None,
        changes: vec![WatchChange {
            series_id: state.bindings[0].source.source_id.clone(),
            episode_id: "episode-1".into(),
            watched: true,
            started: false,
        }],
    };
    state.consume_event(&event).unwrap();
    assert!(state.outbox.pending().is_empty());
}

#[test]
fn inactive_account_cannot_send_retained_work_until_verified_resume() {
    let mut state = state();
    let key = state.targets[0].key.clone();
    state
        .projections
        .push(Projection::new(key.clone(), rev(1), 0, vec![]));
    state.replace_progress(&key, 3, vec![]).unwrap();
    state.active_accounts.clear();
    assert!(state.begin_delivery(&key, 0).unwrap().is_none());
    assert_eq!(state.outbox.pending()[0].state, DeliveryState::StaleAccount);
    state.active_accounts.push(key.account.clone());
    state.resume_account(&key.account, rev(1)).unwrap();
    assert!(state.begin_delivery(&key, 0).unwrap().is_some());
}

#[test]
fn explicit_reset_retains_journal_and_removes_it_in_the_reset_transaction() {
    let storage = MemoryStorage::default();
    let key = format!("{EVENT_PREFIX}00000000000000000001");
    storage
        .write(&[(key.clone(), Some("original event".into()))])
        .unwrap();
    let store = Store::reset_with_journal_backup(
        storage.clone(),
        &[(key.clone(), "original event".into())],
    )
    .unwrap();
    assert!(store.state().bindings.is_empty());
    assert!(storage.read(&key).unwrap().is_none());
    assert_eq!(
        storage.read(EVENT_COUNTER_KEY).unwrap().as_deref(),
        Some("0")
    );
    assert!(
        storage
            .0
            .lock()
            .unwrap()
            .rows
            .values()
            .any(|value| value.contains("original event"))
    );
}

#[test]
fn automatic_pause_holds_queue_and_checkpoints_new_history() {
    let mut state = delivery_state();
    let key = state.targets[0].key.clone();
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: state.bindings[0].id.clone(),
        sequence: 0,
    });
    state
        .observe_episode("binding", &episode(&state.bindings[0], "episode-1"), true)
        .unwrap();
    state.automatic_paused.push(Service::AniList);
    assert!(state.begin_delivery(&key, 0).unwrap().is_none());
    state
        .consume_event(&WatchEvent {
            sequence: 1,
            origin: EventOrigin::Local,
            date: None,
            changes: vec![WatchChange {
                series_id: "opaque-source".into(),
                episode_id: "episode-8".into(),
                watched: true,
                started: false,
            }],
        })
        .unwrap();
    assert_eq!(state.outbox.pending()[0].progress, 1);
    state.automatic_paused.clear();
    assert_eq!(
        state
            .begin_delivery(&key, 0)
            .unwrap()
            .unwrap()
            .patch()
            .progress,
        1
    );
}

#[test]
fn large_offline_backlog_coalesces_and_round_trips_without_losing_targets() {
    let started = std::time::Instant::now();
    let mut state = state();
    state.targets.clear();
    state.bindings.clear();
    for id in 1..=500 {
        let target = target(Service::AniList, id);
        state.bindings.push(binding(
            &target,
            &format!("source-{id}"),
            &format!("binding-{id}"),
            &[("episode", 1)],
        ));
        state
            .projections
            .push(Projection::new(target.key.clone(), rev(1), 0, vec![]));
        state.link_checkpoints.push(LinkCheckpoint {
            binding_id: format!("binding-{id}"),
            sequence: 0,
        });
        state.targets.push(target);
    }
    let event = WatchEvent {
        sequence: 1,
        origin: EventOrigin::Local,
        date: None,
        changes: (1..=500)
            .map(|id| WatchChange {
                series_id: format!("source-{id}"),
                episode_id: "episode".into(),
                watched: true,
                started: false,
            })
            .collect(),
    };
    state.consume_event(&event).unwrap();
    state.consume_event(&event).unwrap();
    assert_eq!(state.outbox.pending().len(), 500);
    let storage = MemoryStorage::default();
    let mut store = Store::load(storage.clone()).unwrap();
    store.save(state).unwrap();
    let restored = Store::load(storage).unwrap();
    assert_eq!(restored.state().outbox.pending().len(), 500);
    assert!(
        restored
            .state()
            .outbox
            .pending()
            .iter()
            .all(|p| p.progress == 1 && p.state == DeliveryState::Queued)
    );
    eprintln!(
        "500 links and coalesced offline intents: {:?}",
        started.elapsed()
    );
}

#[test]
fn journal_replay_after_unwatch_cannot_restore_a_watched_transition() {
    let mut state = delivery_state();
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: "binding".into(),
        sequence: 0,
    });
    let change = |sequence, watched| WatchEvent {
        sequence,
        origin: EventOrigin::Local,
        date: None,
        changes: vec![WatchChange {
            series_id: "opaque-source".into(),
            episode_id: "episode-1".into(),
            watched,
            started: false,
        }],
    };
    state.consume_event(&change(1, true)).unwrap();
    state.consume_event(&change(2, false)).unwrap();
    state.outbox.cancel_unsent(&state.targets[0].key);
    state.consume_event(&change(1, true)).unwrap();
    assert!(state.outbox.pending().is_empty());
    assert_eq!(state.link_checkpoints[0].sequence, 2);
}

fn completion_state() -> TrackingState {
    let mut state = delivery_state();
    state.snapshots.push(snapshot(&state));
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: state.bindings[0].id.clone(),
        sequence: 0,
    });
    state.configure_completion_prompts(true, 0);
    state
}
fn completion_event(sequence: u64, ordinal: u32, watched: bool) -> WatchEvent {
    WatchEvent {
        sequence,
        origin: EventOrigin::Local,
        date: None,
        changes: vec![WatchChange {
            series_id: "opaque-source".into(),
            episode_id: format!("episode-{ordinal}"),
            watched,
            started: false,
        }],
    }
}
fn completion_remote(key: &TargetKey, score_tenths: u32) -> RemoteEntry {
    RemoteEntry {
        account: key.account.clone(),
        media_id: key.remote_media_id,
        entry_id: None,
        progress: 12,
        status: ListStatus::Completed,
        score_tenths,
        started: ListDate::default(),
        completed: ListDate::default(),
        repeating: false,
    }
}

#[test]
fn completion_requires_a_new_local_finished_known_release_transition() {
    let mut state = completion_state();
    state.consume_event(&completion_event(1, 8, true)).unwrap();
    assert!(state.pending_completions().is_empty());
    state.consume_event(&completion_event(2, 12, true)).unwrap();
    assert_eq!(state.pending_completions().len(), 1);
    // Replaying after a later unwatch is harmless, including across reloads.
    state
        .consume_event(&completion_event(3, 12, false))
        .unwrap();
    state.consume_event(&completion_event(2, 12, true)).unwrap();
    assert_eq!(state.pending_completions().len(), 1);
    let key = state.targets[0].key.clone();
    state.dismiss_completion(&key).unwrap();
    state.replace_progress(&key, 0, vec![]).unwrap();
    state.consume_event(&completion_event(4, 12, true)).unwrap();
    assert!(state.pending_completions().is_empty());
    assert_eq!(state.completion.handled, vec![key]);
    state.validate().unwrap();

    for (finished, total, origin) in [
        (false, Some(n(12)), EventOrigin::Local),
        (true, None, EventOrigin::Local),
        (true, Some(n(12)), EventOrigin::PairedDevice),
    ] {
        let mut state = completion_state();
        state.targets[0].release_finished = finished;
        state.targets[0].final_episode_total = total;
        let mut event = completion_event(1, 12, true);
        event.origin = origin;
        state.consume_event(&event).unwrap();
        assert!(state.pending_completions().is_empty());
    }
    // Linking/importing already watched history establishes a checkpoint only.
    let mut state = completion_state();
    let b = &state.bindings[0];
    state.projections[0] = Projection::new(
        b.target.clone(),
        rev(1),
        0,
        vec![Observation {
            episode: episode(b, "episode-12"),
            watched: true,
        }],
    );
    state.consume_event(&completion_event(1, 12, true)).unwrap();
    assert!(state.pending_completions().is_empty());
}

#[test]
fn completion_enable_watermark_and_disable_never_prompt_old_history() {
    let mut state = completion_state();
    state.configure_completion_prompts(false, 0);
    state.configure_completion_prompts(true, 2);
    state.consume_event(&completion_event(1, 12, true)).unwrap();
    assert!(state.pending_completions().is_empty());
    let key = state.targets[0].key.clone();
    state.replace_progress(&key, 0, vec![]).unwrap();
    state.consume_event(&completion_event(3, 12, true)).unwrap();
    assert_eq!(state.pending_completions().len(), 1);
    state.configure_completion_prompts(false, 3);
    assert!(state.completion.pending.is_empty());
    state.configure_completion_prompts(true, 3);
    state.replace_progress(&key, 0, vec![]).unwrap();
    state.consume_event(&completion_event(4, 12, true)).unwrap();
    assert!(state.pending_completions().is_empty());
    assert!(state.completion.handled.is_empty());
}

#[test]
fn completion_ignores_existing_scores_and_invalidates_stale_account_or_mapping() {
    let mut rated = completion_state();
    rated.snapshots[0].remote = Some(completion_remote(&rated.targets[0].key, 80));
    rated.consume_event(&completion_event(1, 12, true)).unwrap();
    assert!(rated.pending_completions().is_empty());
    for guard in 0..4 {
        let mut state = completion_state();
        state.consume_event(&completion_event(1, 12, true)).unwrap();
        match guard {
            0 => state.active_accounts.clear(),
            1 => state.accounts[0].generation = rev(2),
            2 => state.bindings[0].mapping_revision = rev(2),
            _ => state.snapshots[0].remote = Some(completion_remote(&state.targets[0].key, 90)),
        }
        assert!(state.pending_completions().is_empty());
        state.prune_completion_prompts();
        assert!(state.completion.pending.is_empty());
        assert!(
            state
                .dismiss_completion(&state.targets[0].key.clone())
                .is_err()
        );
    }
}

#[test]
fn completion_pending_and_handled_save_survive_atomic_journal_failures_and_restart() {
    let storage = MemoryStorage::default();
    let mut store = Store::load(storage.clone()).unwrap();
    store.save(completion_state()).unwrap();
    let event = completion_event(1, 12, true);
    let journal_key = format!("{EVENT_PREFIX}{:020}", 1);
    storage
        .write(&[(
            journal_key.clone(),
            Some(serde_json::to_string(&event).unwrap()),
        )])
        .unwrap();
    let mut next = store.state().clone();
    next.consume_event(&event).unwrap();
    storage.0.lock().unwrap().fail = true;
    assert!(
        store
            .save_with_events(next.clone(), std::slice::from_ref(&journal_key))
            .is_err()
    );
    assert!(store.state().pending_completions().is_empty());
    assert!(storage.read(&journal_key).unwrap().is_some());
    storage.0.lock().unwrap().fail = false;
    store
        .save_with_events(next, &[journal_key.clone()])
        .unwrap();
    let mut store = Store::load(storage.clone()).unwrap();
    assert_eq!(store.state().pending_completions().len(), 1);
    assert!(storage.read(&journal_key).unwrap().is_none());
    let key = store.state().targets[0].key.clone();
    store
        .mutate(|state| state.enqueue_completion_rating(&key, 85))
        .unwrap();
    let mut store = Store::load(storage).unwrap();
    assert!(store.state().pending_completions().is_empty());
    assert_eq!(store.state().completion.handled, vec![key.clone()]);
    let edit = store
        .mutate(|state| state.begin_edit(&key, 0))
        .unwrap()
        .unwrap();
    assert!(edit.edit().only_if_unrated);
    assert_eq!(
        edit.edit().patch,
        EntryPatch {
            score_tenths: Some(85),
            ..Default::default()
        }
    );
    // A worker's fresh read can acknowledge without writing over a new score.
    store
        .mutate(|state| state.finish_edit(&edit, Ok(completion_remote(&key, 90)), 0))
        .unwrap();
    assert!(store.state().outbox.edits().is_empty());
    assert_eq!(
        store.state().snapshots[0]
            .remote
            .as_ref()
            .unwrap()
            .score_tenths,
        90
    );
}

#[test]
fn completion_rating_edit_keeps_mapping_guard_and_ordinary_edits_are_unguarded() {
    let mut state = completion_state();
    state.consume_event(&completion_event(1, 12, true)).unwrap();
    let key = state.targets[0].key.clone();
    assert!(state.enqueue_completion_rating(&key, 0).is_err());
    assert!(state.enqueue_completion_rating(&key, 101).is_err());
    state.enqueue_completion_rating(&key, 85).unwrap();
    state.bindings[0].mapping_revision = rev(2);
    assert!(state.begin_edit(&key, 0).unwrap().is_none());
    assert_eq!(state.outbox.edits()[0].state, DeliveryState::NeedsAlignment);
    state
        .enqueue_edit(
            &key,
            EntryPatch {
                score_tenths: Some(90),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!state.outbox.edits()[0].only_if_unrated);
    state.validate().unwrap();
    let mut json = serde_json::to_value(&state).unwrap();
    json["outbox"]["edits"][0]
        .as_object_mut()
        .unwrap()
        .remove("only_if_unrated");
    json["outbox"]["edits"][0]
        .as_object_mut()
        .unwrap()
        .remove("completion_mappings");
    let legacy: TrackingState = serde_json::from_value(json).unwrap();
    assert!(!legacy.outbox.edits()[0].only_if_unrated);
}

#[test]
fn completion_receipts_are_monotonic_and_use_an_additive_peer_domain() {
    let mut a = completion_state();
    let mut b = a.clone();
    a.consume_event(&completion_event(1, 12, true)).unwrap();
    b.consume_event(&completion_event(1, 12, true)).unwrap();
    let key = a.targets[0].key.clone();
    a.dismiss_completion(&key).unwrap();
    let records: Vec<_> = completion_peer_records(&a)
        .unwrap()
        .into_iter()
        .map(|(k, v, _)| (k, v))
        .collect();
    assert!(crate::peer::apply_peer_records(&b, &records, 2, |_| false).is_err());
    let b = apply_completion_peer_records(&b, &records).unwrap();
    assert!(b.pending_completions().is_empty());
    let b = apply_completion_peer_records(&b, &[]).unwrap();
    assert_eq!(b.completion.handled, vec![key]);
    assert_eq!(
        completion_peer_records(&b).unwrap(),
        completion_peer_records(&a).unwrap()
    );
    assert!(
        apply_completion_peer_records(&b, &[("handled:bad".into(), records[0].1.clone())]).is_err()
    );
    let new_device = apply_completion_peer_records(&TrackingState::default(), &records).unwrap();
    assert!(new_device.completion.pending.is_empty());
    assert_eq!(new_device.completion.handled, a.completion.handled);
    assert!(new_device.accounts.is_empty());
}

#[test]
fn schema_three_upgrade_does_not_enable_completion_or_replay_old_events() {
    let storage = MemoryStorage::default();
    let mut json = serde_json::json!({"version":3,"state":completion_state()});
    json["state"].as_object_mut().unwrap().remove("completion");
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(json.to_string()))])
        .unwrap();
    let mut store = Store::load(storage.clone()).unwrap();
    assert!(!store.state().completion.enabled);
    store
        .mutate(|state| {
            state.configure_completion_prompts(true, 10);
            state.consume_event(&completion_event(1, 12, true))
        })
        .unwrap();
    assert!(store.state().pending_completions().is_empty());
    let saved: serde_json::Value =
        serde_json::from_str(&storage.read(TRACKING_STATE_KEY).unwrap().unwrap()).unwrap();
    assert_eq!(saved["version"], 5);
}

#[test]
fn completion_groups_split_bindings_but_keeps_services_independent() {
    let mut state = completion_state();
    let mut split = state.bindings[0].clone();
    split.id = "split".into();
    split.assignments.retain(|a| a.target_episode.get() == 12);
    state.bindings[0]
        .assignments
        .retain(|a| a.target_episode.get() != 12);
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: split.id.clone(),
        sequence: 0,
    });
    state.bindings.push(split);
    let mal = target(Service::MyAnimeList, 42);
    let mut mal_binding = state.bindings[1].clone();
    mal_binding.id = "mal".into();
    mal_binding.target = mal.key.clone();
    state.accounts.push(Account {
        key: mal.key.account.clone(),
        generation: rev(1),
        display_name: "MAL".into(),
    });
    state.active_accounts.push(mal.key.account.clone());
    let mut mal_snapshot = snapshot(&state);
    mal_snapshot.target = mal.key.clone();
    mal_snapshot.media.id = mal.key.remote_media_id;
    mal_snapshot.score_format = ScoreFormat::Point10;
    state.snapshots.push(mal_snapshot);
    state
        .projections
        .push(Projection::new(mal.key.clone(), rev(1), 0, vec![]));
    state.link_checkpoints.push(LinkCheckpoint {
        binding_id: mal_binding.id.clone(),
        sequence: 0,
    });
    state.bindings.push(mal_binding);
    state.targets.push(mal);
    state.validate().unwrap();
    state.consume_event(&completion_event(1, 8, true)).unwrap();
    assert!(state.pending_completions().is_empty());
    state.consume_event(&completion_event(2, 12, true)).unwrap();
    let pending = state.pending_completions();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].mappings.len(), 2);
    assert_eq!(pending[1].mappings.len(), 1);
    state
        .enqueue_completion_rating(&pending[0].target, 85)
        .unwrap();
    state.dismiss_completion(&pending[1].target).unwrap();
    assert_eq!(state.completion.handled.len(), 2);
    assert!(state.pending_completions().is_empty());
    assert_eq!(state.outbox.edits().len(), 1);
    state.validate().unwrap();
}

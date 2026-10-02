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
    let raw = r#"{"version":2,"state":"new schema"}"#;
    storage
        .write(&[(TRACKING_STATE_KEY.into(), Some(raw.into()))])
        .unwrap();
    assert!(matches!(
        Store::load(storage.clone()),
        Err(LoadError::UnsupportedVersion(2))
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

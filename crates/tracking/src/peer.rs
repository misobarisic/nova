//! Portable tracking configuration and shared sign-in. Delivery attempts and
//! history cursors stay local; received bearer tokens require API verification.
use crate::*;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

pub const TRACKING_DOMAIN: &str = "tracking";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Link {
        version: u32,
        source: SourceRef,
        target: Target,
        assignments: Vec<Assignment>,
        enabled: bool,
        account_name: String,
        media: Option<Box<Media>>,
    },
    Preference {
        version: u32,
        service: Service,
        paused: bool,
    },
}
fn link_key(source: &SourceRef, target: &TargetKey) -> Result<String, ValidationError> {
    serde_json::to_string(&(source, target))
        .map(|s| format!("link:{s}"))
        .map_err(|_| invalid())
}
fn preference_key(service: Service) -> String {
    format!(
        "automatic:{}",
        if service == Service::MyAnimeList {
            "mal"
        } else {
            "anilist"
        }
    )
}
fn invalid() -> ValidationError {
    ValidationError("invalid shared tracking configuration".into())
}
pub fn peer_records(state: &TrackingState) -> Result<Vec<(String, String, u64)>, ValidationError> {
    let mut records: Vec<(String, String, u64)> = vec![];
    for binding in &state.bindings {
        let mut target = state
            .targets
            .iter()
            .find(|t| t.key == binding.target)
            .ok_or_else(invalid)?
            .clone();
        target.remote_entry_id = None;
        let mut record = Record::Link {
            version: 1,
            source: binding.source.clone(),
            target,
            assignments: binding.assignments.clone(),
            enabled: binding.enabled,
            account_name: state
                .accounts
                .iter()
                .find(|a| a.key == binding.target.account)
                .map(|a| a.display_name.clone())
                .unwrap_or_default(),
            media: state
                .snapshots
                .iter()
                .find(|s| s.target == binding.target)
                .map(|s| Box::new(s.media.clone())),
        };
        let key = link_key(&binding.source, &binding.target)?;
        if let Some((_, raw, _)) = records.iter_mut().find(|(old, _, _)| *old == key) {
            let mut existing: Record = serde_json::from_str(raw).map_err(|_| invalid())?;
            if let Record::Link {
                assignments,
                enabled,
                ..
            } = &mut existing
            {
                for assignment in &binding.assignments {
                    if !assignments.contains(assignment) {
                        assignments.push(assignment.clone());
                    }
                }
                *enabled &= binding.enabled;
                record = existing;
            }
            if let Record::Link { assignments, .. } = &mut record {
                assignments.sort_by_key(|a| (a.target_episode, a.episode_id.clone()));
            }
            *raw = serde_json::to_string(&record).map_err(|_| invalid())?;
        } else {
            if let Record::Link { assignments, .. } = &mut record {
                assignments.sort_by_key(|a| (a.target_episode, a.episode_id.clone()));
            }
            records.push((
                key,
                serde_json::to_string(&record).map_err(|_| invalid())?,
                0,
            ));
        }
    }
    // Default preferences produce no seed record. Resuming becomes a tombstone,
    // so a fresh installation cannot overwrite an existing peer's choice.
    for &service in &state.automatic_paused {
        records.push((
            preference_key(service),
            serde_json::to_string(&Record::Preference {
                version: 1,
                service,
                paused: true,
            })
            .map_err(|_| invalid())?,
            0,
        ));
    }
    records.sort_by(|a, b| a.0.cmp(&b.0));
    if records.windows(2).any(|r| r[0].0 == r[1].0) {
        return Err(invalid());
    }
    Ok(records)
}
/// Materialize an authoritative domain snapshot using this device's account
/// generations and history. Receipt alone never activates an account or uploads
/// watched history. Conflicting concurrent links are paused for alignment.
pub fn apply_peer_records(
    local: &TrackingState,
    records: &[(String, String)],
    sequence: u64,
    watched: impl Fn(&SourceEpisode) -> bool,
) -> Result<TrackingState, ValidationError> {
    let mut state = local.clone();
    state.active_accounts.clear();
    let mut incoming = vec![];
    let mut paused = vec![];
    let mut keys = std::collections::HashSet::new();
    for (key, raw) in records {
        if raw.len() > 1_048_576 || !keys.insert(key) {
            return Err(invalid());
        }
        if let Some(service) = connection_service(key) {
            let connection =
                crate::credentials::SavedConnection::decode(raw, service).map_err(|_| invalid())?;
            if !state.accounts.iter().any(|a| a.key == connection.account) {
                state.accounts.push(Account {
                    key: connection.account.clone(),
                    display_name: String::new(),
                    generation: NonZeroU64::MIN,
                });
            }
            state.active_accounts.push(connection.account);
            continue;
        }
        let record: Record = serde_json::from_str(raw).map_err(|_| invalid())?;
        match record {
            Record::Preference {
                version: 1,
                service,
                paused: value,
            } if *key == preference_key(service) => {
                if value && !paused.contains(&service) {
                    paused.push(service);
                }
            }
            Record::Link {
                version: 1,
                source,
                mut target,
                assignments,
                enabled,
                account_name,
                media,
            } if *key == link_key(&source, &target.key)? => {
                if assignments.len() > 10000
                    || account_name.len() > 512
                    || [&source.provider_id, &source.source_id, &source.media_type]
                        .iter()
                        .any(|s| s.len() > 4096)
                    || assignments.iter().any(|a| a.episode_id.len() > 4096)
                    || media
                        .as_ref()
                        .is_some_and(|m| m.id != target.key.remote_media_id || m.title.len() > 4096)
                {
                    return Err(invalid());
                }
                let generation = if let Some(a) = state
                    .accounts
                    .iter_mut()
                    .find(|a| a.key == target.key.account)
                {
                    if !account_name.is_empty() {
                        a.display_name = account_name;
                    }
                    a.generation
                } else {
                    state.accounts.push(Account {
                        key: target.key.account.clone(),
                        generation: NonZeroU64::MIN,
                        display_name: account_name,
                    });
                    NonZeroU64::MIN
                };
                if let Some(old) = state.targets.iter_mut().find(|t| t.key == target.key) {
                    target.remote_entry_id = old.remote_entry_id;
                    *old = target.clone();
                } else {
                    target.remote_entry_id = None;
                    state.targets.push(target.clone());
                }
                if let Some(media) = media {
                    let media = *media;
                    if let Some(snapshot) =
                        state.snapshots.iter_mut().find(|s| s.target == target.key)
                    {
                        snapshot.media = media;
                    } else {
                        state.snapshots.push(TargetSnapshot {
                            target: target.key.clone(),
                            media,
                            remote: None,
                            score_format: ScoreFormat::Point10,
                            status_pinned: false,
                            dates_pinned: false,
                        });
                    }
                }
                let old = local
                    .bindings
                    .iter()
                    .find(|b| b.source == source && b.target == target.key);
                let id = if let Some(old) = old {
                    old.id.clone()
                } else {
                    let mut n = local.bindings.len() + incoming.len() + 1;
                    while local
                        .bindings
                        .iter()
                        .chain(incoming.iter())
                        .any(|b: &Binding| b.id == format!("binding-{n}"))
                    {
                        n += 1;
                    }
                    format!("binding-{n}")
                };
                // Serialization order is not a remap. Preserve local ordering
                // when coverage is identical so startup/receipt cannot discard
                // already-authorized queued work or advance its history cursor.
                let assignments = if let Some(old) = old.filter(|old| {
                    old.assignments.len() == assignments.len()
                        && old.assignments.iter().all(|a| assignments.contains(a))
                }) {
                    old.assignments.clone()
                } else {
                    assignments
                };
                incoming.push(Binding {
                    id,
                    source,
                    target: target.key,
                    account_generation: generation,
                    mapping_revision: old.map(|b| b.mapping_revision).unwrap_or(NonZeroU64::MIN),
                    enabled,
                    assignments,
                });
            }
            _ => return Err(invalid()),
        }
    }
    let mut conflicts = std::collections::HashSet::new();
    for (i, a) in incoming.iter().enumerate().filter(|(_, b)| b.enabled) {
        for (j, b) in incoming
            .iter()
            .enumerate()
            .skip(i + 1)
            .filter(|(_, b)| b.enabled)
        {
            if a.source == b.source
                && a.target.account == b.target.account
                && a.assignments.iter().any(|x| {
                    b.assignments.iter().any(|y| {
                        x.episode_id == y.episode_id
                            && (a.target != b.target || x.target_episode != y.target_episode)
                    })
                })
            {
                conflicts.insert(i);
                conflicts.insert(j);
            }
        }
    }
    for index in conflicts {
        incoming[index].enabled = false;
    }
    let mut affected: Vec<(TargetKey, bool)> = vec![];
    for old in &local.bindings {
        if !incoming.iter().any(|new| new == old) {
            let extension = incoming
                .iter()
                .find(|b| b.source == old.source && b.target == old.target)
                .is_some_and(|b| {
                    b.enabled
                        && old.enabled
                        && old.assignments.iter().all(|a| b.assignments.contains(a))
                });
            if let Some((_, preserve)) = affected.iter_mut().find(|(key, _)| *key == old.target) {
                *preserve &= extension;
            } else {
                affected.push((old.target.clone(), extension));
            }
        }
    }
    for binding in &mut incoming {
        let old = local.bindings.iter().find(|b| b.id == binding.id);
        if old != Some(binding) {
            if let Some(old) = old {
                binding.mapping_revision = NonZeroU64::new(
                    old.mapping_revision
                        .get()
                        .checked_add(1)
                        .ok_or_else(invalid)?,
                )
                .ok_or_else(invalid)?;
            }
            state
                .link_checkpoints
                .retain(|c| c.binding_id != binding.id);
            state.link_checkpoints.push(LinkCheckpoint {
                binding_id: binding.id.clone(),
                sequence,
            });
            if !affected.iter().any(|(key, _)| *key == binding.target) {
                affected.push((binding.target.clone(), true));
            }
        }
    }
    state.bindings = incoming;
    state
        .link_checkpoints
        .retain(|c| state.bindings.iter().any(|b| b.id == c.binding_id));
    state.automatic_paused = paused;
    for (key, preserve) in affected {
        let mut observations = vec![];
        for binding in state
            .bindings
            .iter()
            .filter(|b| b.enabled && b.target == key)
        {
            for assignment in &binding.assignments {
                let episode = SourceEpisode {
                    source: binding.source.clone(),
                    episode_id: assignment.episode_id.clone(),
                };
                if !observations
                    .iter()
                    .any(|o: &Observation| o.episode == episode)
                {
                    observations.push(Observation {
                        watched: watched(&episode),
                        episode,
                    });
                }
            }
        }
        if let Some(projection) = state.projections.iter().find(|p| p.target == key) {
            let baseline = projection.remote_baseline();
            state
                .rebase_confirmed_mapping(&key, baseline, observations, preserve)
                .map_err(|_| invalid())?;
        } else {
            let generation = state
                .accounts
                .iter()
                .find(|a| a.key == key.account)
                .ok_or_else(invalid)?
                .generation;
            state
                .projections
                .push(Projection::new(key, generation, 0, observations));
        }
    }
    state.validate()?;
    Ok(state)
}

fn connection_service(key: &str) -> Option<Service> {
    match key {
        "credentials:mal" => Some(Service::MyAnimeList),
        "credentials:anilist" => Some(Service::AniList),
        _ => None,
    }
}
pub fn connection_record(
    connection: &crate::credentials::SavedConnection,
) -> Result<(String, String, u64), crate::credentials::CredentialError> {
    let key = if connection.account.service == Service::MyAnimeList {
        "credentials:mal"
    } else {
        "credentials:anilist"
    };
    Ok((key.into(), connection.encode()?, 0))
}
pub fn peer_connections(
    records: &[(String, String)],
) -> Result<Vec<(Service, crate::credentials::SavedConnection)>, crate::credentials::CredentialError>
{
    records
        .iter()
        .filter_map(|(key, raw)| {
            connection_service(key).map(|service| {
                crate::credentials::SavedConnection::decode(raw, service).map(|c| (service, c))
            })
        })
        .collect()
}

/// Publish only the safety pause caused by conflicting peer coverage. This
/// intentional reconciliation converges both peers; ordinary apply never echoes.
pub fn conflict_pauses(
    state: &TrackingState,
    incoming: &[(String, String)],
) -> Result<Vec<(String, String, u64)>, ValidationError> {
    let mut pauses = vec![];
    for (key, value, ts) in peer_records(state)? {
        let Some((_, old)) = incoming.iter().find(|(k, _)| *k == key) else {
            continue;
        };
        if matches!(
            serde_json::from_str::<Record>(old),
            Ok(Record::Link { enabled: true, .. })
        ) && matches!(
            serde_json::from_str::<Record>(&value),
            Ok(Record::Link { enabled: false, .. })
        ) {
            pauses.push((key, value, ts));
        }
    }
    Ok(pauses)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> TrackingState {
        let account = AccountKey {
            service: Service::MyAnimeList,
            remote_user_id: 7.try_into().unwrap(),
        };
        let key = TargetKey {
            account: account.clone(),
            media_kind: MediaKind::Anime,
            remote_media_id: 12.try_into().unwrap(),
        };
        TrackingState {
            accounts: vec![Account {
                key: account.clone(),
                generation: NonZeroU64::new(3).unwrap(),
                display_name: "fixture".into(),
            }],
            active_accounts: vec![account],
            targets: vec![Target {
                key: key.clone(),
                remote_entry_id: None,
                final_episode_total: Some(12.try_into().unwrap()),
                release_finished: true,
            }],
            bindings: vec![Binding {
                id: "local-1".into(),
                source: SourceRef {
                    provider_id: "nova".into(),
                    source_id: "merged".into(),
                    media_type: "series".into(),
                },
                target: key,
                account_generation: NonZeroU64::new(3).unwrap(),
                mapping_revision: NonZeroU64::new(9).unwrap(),
                enabled: true,
                assignments: vec![Assignment {
                    episode_id: "stable-episode".into(),
                    target_episode: 1.try_into().unwrap(),
                }],
            }],
            automatic_paused: vec![Service::AniList],
            ..Default::default()
        }
    }
    fn snapshot(state: &TrackingState) -> Vec<(String, String)> {
        peer_records(state)
            .unwrap()
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect()
    }
    #[test]
    fn shared_media_keeps_its_existing_json_shape() {
        let source = sample();
        let mut records = snapshot(&source);
        let (_, raw) = records
            .iter_mut()
            .find(|(key, _)| key.starts_with("link:"))
            .unwrap();
        let media = serde_json::json!({
            "id": 12,
            "mal_id": null,
            "title": "Fixture release",
            "format": "TV",
            "episodes": 12,
            "finished": true,
            "year": 2021,
        });
        // Read the original unboxed JSON format, then publish the restored
        // snapshot again: changing the in-memory enum must not change sync.
        let mut record: serde_json::Value = serde_json::from_str(raw).unwrap();
        record["media"] = media.clone();
        *raw = record.to_string();
        let restored =
            apply_peer_records(&TrackingState::default(), &records, 42, |_| false).unwrap();
        assert_eq!(restored.snapshots[0].media.title, "Fixture release");
        let published = snapshot(&restored);
        let (_, raw) = published
            .iter()
            .find(|(key, _)| key.starts_with("link:"))
            .unwrap();
        let record: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(record["version"], 1);
        assert_eq!(record["media"], media);
    }
    #[test]
    fn peer_links_keep_stable_coverage_but_use_local_revisions_and_history_without_uploading() {
        let source = sample();
        let mut result =
            apply_peer_records(&TrackingState::default(), &snapshot(&source), 42, |_| true)
                .unwrap();
        assert_eq!(
            result.bindings[0].assignments,
            source.bindings[0].assignments
        );
        assert_eq!(result.bindings[0].mapping_revision, NonZeroU64::MIN);
        assert_eq!(result.bindings[0].account_generation, NonZeroU64::MIN);
        assert_eq!(result.link_checkpoints[0].sequence, 42);
        assert_eq!(result.automatic_paused, vec![Service::AniList]);
        assert!(result.outbox.pending().is_empty());
        assert!(result.active_accounts.is_empty());
        let binding = result.bindings[0].clone();
        let episode = SourceEpisode {
            source: binding.source.clone(),
            episode_id: "stable-episode".into(),
        };
        assert!(
            !result.projections[0]
                .observe(&binding, &episode, true)
                .unwrap()
        );
        let again = apply_peer_records(&result, &snapshot(&source), 99, |_| true).unwrap();
        assert_eq!(
            again.link_checkpoints[0].sequence, 42,
            "unchanged links retain their cursor"
        );
        assert_eq!(again.bindings[0].mapping_revision, NonZeroU64::MIN);
    }
    #[test]
    fn several_local_ranges_for_one_source_and_release_share_one_complete_link() {
        let mut state = sample();
        let mut second = state.bindings[0].clone();
        second.id = "local-2".into();
        second.assignments = vec![Assignment {
            episode_id: "second-episode".into(),
            target_episode: 2.try_into().unwrap(),
        }];
        state.bindings.push(second);
        state.validate().unwrap();
        let records = snapshot(&state);
        assert_eq!(
            records
                .iter()
                .filter(|(key, _)| key.starts_with("link:"))
                .count(),
            1
        );
        let result = apply_peer_records(&state, &records, 42, |_| true).unwrap();
        assert_eq!(result.bindings.len(), 1);
        assert_eq!(result.bindings[0].assignments.len(), 2);
        assert_eq!(result.bindings[0].id, "local-1");
        assert!(result.outbox.pending().is_empty());
        let mut reordered = result.clone();
        reordered.bindings[0].assignments.reverse();
        let repeated = apply_peer_records(&reordered, &snapshot(&result), 99, |_| true).unwrap();
        assert_eq!(
            repeated.bindings[0].assignments,
            reordered.bindings[0].assignments
        );
        assert_eq!(
            repeated.bindings[0].mapping_revision,
            reordered.bindings[0].mapping_revision
        );
        assert_eq!(repeated.link_checkpoints[0].sequence, 42);
    }
    #[test]
    fn shared_disconnect_and_unlink_remove_choices_and_coverage_without_copying_delivery_work() {
        let local = sample();
        let result = apply_peer_records(&local, &[], 50, |_| false).unwrap();
        assert!(result.bindings.is_empty());
        assert!(result.link_checkpoints.is_empty());
        assert!(result.active_accounts.is_empty());
        assert!(result.automatic_paused.is_empty());
        assert_eq!(result.accounts, local.accounts);
        assert_eq!(result.targets, local.targets);
    }
    #[test]
    fn concurrent_conflicting_links_are_paused_and_remaps_advance_local_revisions() {
        let local = sample();
        let mut remote = local.clone();
        remote.bindings[0].assignments[0].target_episode = 2.try_into().unwrap();
        let remapped = apply_peer_records(&local, &snapshot(&remote), 50, |_| true).unwrap();
        assert_eq!(remapped.bindings[0].id, "local-1");
        assert_eq!(remapped.bindings[0].mapping_revision.get(), 10);
        assert_eq!(remapped.link_checkpoints[0].sequence, 50);
        let mut other = remote.bindings[0].clone();
        other.id = "other".into();
        other.target.remote_media_id = 13.try_into().unwrap();
        remote.targets.push(Target {
            key: other.target.clone(),
            ..remote.targets[0].clone()
        });
        remote.bindings.push(other);
        let conflicted = apply_peer_records(&local, &snapshot(&remote), 51, |_| true).unwrap();
        assert_eq!(conflicted.bindings.len(), 2);
        assert!(conflicted.bindings.iter().all(|b| !b.enabled));
        assert!(conflicted.outbox.pending().is_empty());
        let pauses = conflict_pauses(&conflicted, &snapshot(&remote)).unwrap();
        assert_eq!(pauses.len(), 2);
        let normalized: Vec<_> = pauses.into_iter().map(|(k, v, _)| (k, v)).collect();
        let converged = apply_peer_records(&conflicted, &normalized, 52, |_| false).unwrap();
        assert!(conflict_pauses(&converged, &normalized).unwrap().is_empty());
        let mut repaired = converged.clone();
        repaired.bindings[0].enabled = true;
        assert!(
            apply_peer_records(&converged, &snapshot(&repaired), 53, |_| false)
                .unwrap()
                .bindings[0]
                .enabled
        );
    }
    #[test]
    fn tokens_and_account_choice_round_trip_while_delivery_state_stays_local() {
        let state = sample();
        let connection = crate::credentials::SavedConnection {
            account: state.accounts[0].key.clone(),
            registration: crate::auth::ClientRegistration {
                client_id: "fixture".into(),
                redirect_uri: "http://127.0.0.1:53926/callback".into(),
            },
            tokens: crate::auth::Tokens {
                access: Secret::new("shared-access".into()).unwrap(),
                refresh: Some(Secret::new("shared-refresh".into()).unwrap()),
                expires_at: Some(10000),
            },
        };
        let mut records = snapshot(&state);
        let (key, raw, _) = connection_record(&connection).unwrap();
        records.push((key, raw));
        records.sort_by(|a, b| a.0.cmp(&b.0));
        let copied = peer_connections(&records).unwrap();
        assert_eq!(copied[0].1.tokens.access.expose(), "shared-access");
        assert_eq!(
            copied[0].1.tokens.refresh.as_ref().unwrap().expose(),
            "shared-refresh"
        );
        let applied =
            apply_peer_records(&TrackingState::default(), &records, 9, |_| false).unwrap();
        assert_eq!(applied.active_accounts, state.active_accounts);
        assert!(applied.outbox.pending().is_empty());
        assert_eq!(applied.accounts[0].display_name, "fixture");
        let wrong = vec![(
            "credentials:anilist".into(),
            records
                .iter()
                .find(|(key, _)| key == "credentials:mal")
                .unwrap()
                .1
                .clone(),
        )];
        assert!(peer_connections(&wrong).is_err());
        assert!(apply_peer_records(&applied, &wrong, 10, |_| false).is_err());
        assert!(
            apply_peer_records(&applied, &[("future".into(), "{}".into())], 10, |_| false).is_err()
        );
    }
}

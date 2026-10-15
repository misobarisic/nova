//! Local completion prompts and portable, grow-only handled receipts. Watched
//! history, prompt candidates and enablement watermarks never cross devices.
use crate::*;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

pub const TRACKING_COMPLETION_DOMAIN: &str = "tracking_completion";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCompletion {
    pub target: TargetKey,
    pub account_generation: NonZeroU64,
    pub mappings: Vec<MappingStamp>,
    pub sequence: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionPrompts {
    pub enabled: bool,
    pub watermark: u64,
    pub pending: Vec<PendingCompletion>,
    /// Survives disablement, unlinking and account replacement on this device.
    pub seen: Vec<TargetKey>,
    /// Only explicit Save/Dismiss decisions are portable.
    pub handled: Vec<TargetKey>,
}

impl TrackingState {
    /// Call before consuming the journal. Enabling starts at the current event
    /// counter, so upgrades and old queued history cannot generate prompts.
    pub fn configure_completion_prompts(&mut self, enabled: bool, sequence: u64) {
        if enabled != self.completion.enabled {
            self.completion.watermark = self.completion.watermark.max(sequence);
            self.completion.enabled = enabled;
        }
        if !enabled {
            self.completion.pending.clear();
        }
        self.prune_completion_prompts();
    }

    pub(crate) fn target_may_complete(&self, key: &TargetKey) -> bool {
        self.targets
            .iter()
            .find(|t| t.key == *key)
            .and_then(|target| {
                self.projections
                    .iter()
                    .find(|p| p.target == *key)
                    .and_then(|p| p.proposal(target).ok())
            })
            .is_some_and(|p| p.may_complete)
    }

    pub(crate) fn record_completion(&mut self, key: &TargetKey, sequence: u64) {
        if !self.completion.enabled
            || sequence <= self.completion.watermark
            || self.completion.seen.contains(key)
            || self.completion.handled.contains(key)
        {
            return;
        }
        self.completion.seen.push(key.clone());
        let Some(account) = self.accounts.iter().find(|a| a.key == key.account) else {
            return;
        };
        let pending = PendingCompletion {
            target: key.clone(),
            account_generation: account.generation,
            mappings: self
                .bindings
                .iter()
                .filter(|b| {
                    b.enabled && b.target == *key && b.account_generation == account.generation
                })
                .map(MappingStamp::from_binding)
                .collect(),
            sequence,
        };
        if self.completion_is_current(&pending) {
            self.completion.pending.push(pending);
        }
    }

    fn completion_is_current(&self, pending: &PendingCompletion) -> bool {
        self.completion.enabled
            && !self.completion.handled.contains(&pending.target)
            && self.active_accounts.contains(&pending.target.account)
            && self.accounts.iter().any(|a| {
                a.key == pending.target.account && a.generation == pending.account_generation
            })
            && self.target_may_complete(&pending.target)
            && !pending.mappings.is_empty()
            && pending.mappings.iter().all(|stamp| {
                self.bindings.iter().any(|b| {
                    b.enabled
                        && b.target == pending.target
                        && b.account_generation == pending.account_generation
                        && stamp.matches(b)
                })
            })
            && self.snapshots.iter().any(|s| {
                s.target == pending.target && s.remote.as_ref().is_none_or(|r| r.score_tenths == 0)
            })
            && !self
                .outbox
                .edits()
                .iter()
                .any(|e| e.target == pending.target && e.patch.score_tenths.is_some())
    }

    /// Returned rows are safe to present even before the worker next persists
    /// pruning after an account, mapping or remote-score refresh.
    pub fn pending_completions(&self) -> Vec<PendingCompletion> {
        self.completion
            .pending
            .iter()
            .filter(|p| self.completion_is_current(p))
            .cloned()
            .collect()
    }

    pub fn prune_completion_prompts(&mut self) {
        self.completion.pending = self.pending_completions();
    }

    pub fn dismiss_completion(&mut self, key: &TargetKey) -> Result<(), TrackingError> {
        if !self.pending_completions().iter().any(|p| p.target == *key) {
            return Err(TrackingError::StaleAttempt);
        }
        self.handle_completion(key);
        Ok(())
    }

    fn handle_completion(&mut self, key: &TargetKey) {
        if !self.completion.handled.contains(key) {
            self.completion.handled.push(key.clone());
        }
        self.completion.pending.retain(|p| p.target != *key);
    }

    /// Persist this mutation atomically: acknowledging the prompt and accepting
    /// its score-only intent must survive a crash together.
    pub fn enqueue_completion_rating(
        &mut self,
        key: &TargetKey,
        score_tenths: u32,
    ) -> Result<NonZeroU64, TrackingError> {
        let pending = self
            .pending_completions()
            .into_iter()
            .find(|p| p.target == *key)
            .ok_or(TrackingError::StaleAttempt)?;
        if score_tenths == 0 {
            return Err(TrackingError::InvalidProgress);
        }
        let revision = self.enqueue_edit(
            key,
            EntryPatch {
                score_tenths: Some(score_tenths),
                ..Default::default()
            },
        )?;
        let edit = self
            .outbox
            .edits
            .iter_mut()
            .find(|e| e.revision == revision)
            .ok_or(TrackingError::StaleAttempt)?;
        edit.only_if_unrated = true;
        edit.completion_mappings = pending.mappings;
        self.handle_completion(key);
        Ok(revision)
    }
}

#[derive(Serialize, Deserialize)]
struct HandledRecord {
    version: u32,
    target: TargetKey,
}
fn receipt_key(key: &TargetKey) -> Result<String, ValidationError> {
    serde_json::to_string(key)
        .map(|s| format!("handled:{s}"))
        .map_err(|_| ValidationError("invalid completion receipt".into()))
}

pub fn completion_peer_records(
    state: &TrackingState,
) -> Result<Vec<(String, String, u64)>, ValidationError> {
    let mut records = state
        .completion
        .handled
        .iter()
        .map(|target| {
            Ok((
                receipt_key(target)?,
                serde_json::to_string(&HandledRecord {
                    version: 1,
                    target: target.clone(),
                })
                .map_err(|_| ValidationError("invalid completion receipt".into()))?,
                0,
            ))
        })
        .collect::<Result<Vec<_>, ValidationError>>()?;
    records.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(records)
}

/// Domain snapshots are merged as a union. Missing records and tombstones
/// cannot resurrect a prompt already handled on another paired device.
pub fn apply_completion_peer_records(
    local: &TrackingState,
    records: &[(String, String)],
) -> Result<TrackingState, ValidationError> {
    let mut state = local.clone();
    let mut keys = std::collections::HashSet::new();
    for (key, raw) in records {
        if raw.len() > 4096 || !keys.insert(key) {
            return Err(ValidationError("invalid completion receipt".into()));
        }
        let record: HandledRecord = serde_json::from_str(raw)
            .map_err(|_| ValidationError("invalid completion receipt".into()))?;
        if record.version != 1
            || *key != receipt_key(&record.target)?
            || record.target.remote_media_id.get() > i32::MAX as u32
            || record.target.account.remote_user_id.get() > i32::MAX as u32
        {
            return Err(ValidationError("invalid completion receipt".into()));
        }
        state.handle_completion(&record.target);
    }
    state.prune_completion_prompts();
    state.validate()?;
    Ok(state)
}

impl CompletionPrompts {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        let valid_key = |key: &TargetKey| {
            key.remote_media_id.get() <= i32::MAX as u32
                && key.account.remote_user_id.get() <= i32::MAX as u32
        };
        for keys in [&self.seen, &self.handled] {
            if keys
                .iter()
                .enumerate()
                .any(|(i, key)| !valid_key(key) || keys[i + 1..].contains(key))
            {
                return Err(ValidationError("invalid completion receipt".into()));
            }
        }
        if (!self.enabled && !self.pending.is_empty())
            || self.pending.iter().enumerate().any(|(i, p)| {
                !valid_key(&p.target)
                    || p.mappings.is_empty()
                    || !self.seen.contains(&p.target)
                    || self.handled.contains(&p.target)
                    || p.sequence <= self.watermark
                    || self.pending[i + 1..]
                        .iter()
                        .any(|other| p.target == other.target)
            })
        {
            return Err(ValidationError("invalid pending completion".into()));
        }
        Ok(())
    }
}

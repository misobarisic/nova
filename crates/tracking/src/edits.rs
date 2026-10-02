//! Field-specific manual edits share ordering, cooldowns and authentication
//! with progress delivery. A newer edit never inherits an older acknowledgment.
use crate::*;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSnapshot {
    pub target: TargetKey,
    pub media: Media,
    pub remote: Option<RemoteEntry>,
    pub score_format: ScoreFormat,
    pub status_pinned: bool,
    pub dates_pinned: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEdit {
    pub target: TargetKey,
    pub account_generation: NonZeroU64,
    pub revision: NonZeroU64,
    pub patch: EntryPatch,
    #[serde(default)]
    pub score_format: Option<ScoreFormat>,
    pub state: DeliveryState,
    pub attempts: u32,
    pub next_attempt_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditAttempt {
    edit: PendingEdit,
}
impl EditAttempt {
    pub fn edit(&self) -> &PendingEdit {
        &self.edit
    }
}
impl Outbox {
    pub fn edits(&self) -> &[PendingEdit] {
        &self.edits
    }
}
impl TrackingState {
    pub fn enqueue_edit(
        &mut self,
        target: &TargetKey,
        patch: EntryPatch,
    ) -> Result<NonZeroU64, TrackingError> {
        let snapshot = self
            .snapshots
            .iter_mut()
            .find(|s| s.target == *target)
            .ok_or(TrackingError::MissingTarget)?;
        // Progress is ordered by the projection revision machinery instead.
        if patch.progress.is_some()
            || patch
                .validate(target.account.service, snapshot.score_format)
                .is_err()
        {
            return Err(TrackingError::InvalidProgress);
        }
        let generation = self
            .accounts
            .iter()
            .find(|a| a.key == target.account)
            .ok_or(TrackingError::StaleAccount)?
            .generation;
        let revision =
            NonZeroU64::new(self.outbox.next_revision).ok_or(TrackingError::RevisionOverflow)?;
        self.outbox.next_revision = self
            .outbox
            .next_revision
            .checked_add(1)
            .ok_or(TrackingError::RevisionOverflow)?;
        if patch.status.is_some() {
            snapshot.status_pinned = true;
        }
        if patch.started.is_some() || patch.completed.is_some() {
            snapshot.dates_pinned = true;
        }
        // Retain only superseded fields from older queued edits. Preserve their
        // original order and retry deadline rather than accidentally clearing them.
        for edit in self
            .outbox
            .edits
            .iter_mut()
            .filter(|e| e.target == *target && e.state != DeliveryState::InFlight)
        {
            if patch.status.is_some() {
                edit.patch.status = None;
            }
            if patch.score_tenths.is_some() {
                edit.patch.score_tenths = None;
            }
            if patch.started.is_some() {
                edit.patch.started = None;
            }
            if patch.completed.is_some() {
                edit.patch.completed = None;
            }
        }
        self.outbox.edits.retain(|e| !e.patch.is_empty());
        self.outbox.edits.push(PendingEdit {
            target: target.clone(),
            account_generation: generation,
            revision,
            score_format: if patch.score_tenths.is_some() {
                Some(snapshot.score_format)
            } else {
                None
            },
            patch,
            state: DeliveryState::Queued,
            attempts: 0,
            next_attempt_at: 0,
        });
        Ok(revision)
    }
    pub fn begin_edit(
        &mut self,
        target: &TargetKey,
        now: u64,
    ) -> Result<Option<EditAttempt>, TrackingError> {
        if self
            .outbox
            .pending
            .iter()
            .any(|p| p.target == *target && p.state == DeliveryState::InFlight)
            || self
                .outbox
                .edits
                .iter()
                .any(|e| e.target == *target && e.state == DeliveryState::InFlight)
        {
            return Ok(None);
        }
        let Some(index) = self.outbox.edits.iter().position(|e| e.target == *target) else {
            return Ok(None);
        };
        let edit = &mut self.outbox.edits[index];
        if !self.active_accounts.contains(&target.account)
            || !self
                .accounts
                .iter()
                .any(|a| a.key == target.account && a.generation == edit.account_generation)
        {
            edit.state = DeliveryState::StaleAccount;
            return Ok(None);
        }
        if !self.bindings.iter().any(|b| {
            b.enabled && b.target == *target && b.account_generation == edit.account_generation
        }) {
            edit.state = DeliveryState::NeedsAlignment;
            return Ok(None);
        }
        if self
            .outbox
            .authentication_pauses
            .iter()
            .any(|p| p.account == target.account && p.generation == edit.account_generation)
        {
            edit.state = DeliveryState::AuthenticationRequired;
            return Ok(None);
        }
        if !matches!(edit.state, DeliveryState::Queued | DeliveryState::Uncertain)
            || now < edit.next_attempt_at
            || self
                .outbox
                .cooldowns
                .iter()
                .any(|c| c.service == target.account.service && now < c.until)
        {
            return Ok(None);
        }
        edit.attempts = edit
            .attempts
            .checked_add(1)
            .ok_or(TrackingError::AttemptOverflow)?;
        edit.state = DeliveryState::InFlight;
        Ok(Some(EditAttempt { edit: edit.clone() }))
    }
    pub fn finish_edit(
        &mut self,
        attempt: &EditAttempt,
        outcome: Result<RemoteEntry, DeliveryFailure>,
        now: u64,
    ) -> Result<(), TrackingError> {
        let index = self
            .outbox
            .edits
            .iter()
            .position(|e| *e == attempt.edit)
            .ok_or(TrackingError::StaleAttempt)?;
        match outcome {
            Ok(remote) => {
                if remote.account != attempt.edit.target.account
                    || remote.media_id != attempt.edit.target.remote_media_id
                {
                    return Err(TrackingError::StaleAccount);
                }
                if let Some(snapshot) = self
                    .snapshots
                    .iter_mut()
                    .find(|s| s.target == attempt.edit.target)
                {
                    snapshot.remote = Some(remote);
                }
                self.outbox.edits.remove(index);
            }
            Err(failure) => {
                // A superseded failed field must never run after its replacement.
                let newer: Vec<_> = self.outbox.edits[index + 1..]
                    .iter()
                    .filter(|e| e.target == attempt.edit.target)
                    .map(|e| e.patch.clone())
                    .collect();
                let edit = &mut self.outbox.edits[index];
                for patch in newer {
                    if patch.status.is_some() {
                        edit.patch.status = None;
                    }
                    if patch.score_tenths.is_some() {
                        edit.patch.score_tenths = None;
                    }
                    if patch.started.is_some() {
                        edit.patch.started = None;
                    }
                    if patch.completed.is_some() {
                        edit.patch.completed = None;
                    }
                }
                edit.state = match failure {
                    DeliveryFailure::Transient => {
                        edit.next_attempt_at = now.saturating_add(
                            5u64.saturating_mul(1 << edit.attempts.min(9)).min(3600),
                        );
                        DeliveryState::Uncertain
                    }
                    DeliveryFailure::RateLimited { retry_at } => {
                        edit.next_attempt_at = retry_at;
                        if let Some(c) = self
                            .outbox
                            .cooldowns
                            .iter_mut()
                            .find(|c| c.service == edit.target.account.service)
                        {
                            c.until = c.until.max(retry_at);
                        } else {
                            self.outbox.cooldowns.push(crate::outbox::ServiceCooldown {
                                service: edit.target.account.service,
                                until: retry_at,
                            });
                        }
                        DeliveryState::Uncertain
                    }
                    DeliveryFailure::AuthenticationRequired => {
                        if !self.outbox.authentication_pauses.iter().any(|p| {
                            p.account == edit.target.account
                                && p.generation == edit.account_generation
                        }) {
                            self.outbox
                                .authentication_pauses
                                .push(crate::outbox::AccountPause {
                                    account: edit.target.account.clone(),
                                    generation: edit.account_generation,
                                });
                        }
                        DeliveryState::AuthenticationRequired
                    }
                    DeliveryFailure::Rejected => DeliveryState::Rejected,
                    DeliveryFailure::NeedsAlignment => DeliveryState::NeedsAlignment,
                };
                self.outbox.edits.retain(|e| !e.patch.is_empty());
            }
        }
        Ok(())
    }
    pub fn retry_edits(&mut self, target: &TargetKey, now: u64) {
        for e in self.outbox.edits.iter_mut().filter(|e| {
            e.target == *target
                && matches!(e.state, DeliveryState::Queued | DeliveryState::Uncertain)
        }) {
            e.next_attempt_at = now;
        }
    }
}

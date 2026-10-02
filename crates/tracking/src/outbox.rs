//! Durable set-style progress intents. Network adapters must read remote state
//! before using an attempt, then persist the outcome before selecting more work.
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use crate::{
    AccountKey, Assignment, Binding, LoadError, Observation, ProjectionError, Service,
    SourceEpisode, SourceRef, TargetKey, TrackingState, ValidationError,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingStamp {
    pub binding_id: String,
    pub revision: NonZeroU64,
    pub source: SourceRef,
    pub assignments: Vec<Assignment>,
}
impl MappingStamp {
    pub(crate) fn from_binding(binding: &Binding) -> Self {
        Self {
            binding_id: binding.id.clone(),
            revision: binding.mapping_revision,
            source: binding.source.clone(),
            assignments: binding.assignments.clone(),
        }
    }
    pub(crate) fn matches(&self, binding: &Binding) -> bool {
        *self == Self::from_binding(binding)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressIntent {
    AutomaticForward,
    ExplicitReplacement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    InFlight,
    Uncertain,
    AuthenticationRequired,
    Rejected,
    NeedsAlignment,
    StaleAccount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryFailure {
    Transient,
    RateLimited { retry_at: u64 },
    AuthenticationRequired,
    Rejected,
    NeedsAlignment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProgress {
    pub target: TargetKey,
    pub account_generation: NonZeroU64,
    pub projection_revision: NonZeroU64,
    pub intent_revision: NonZeroU64,
    pub mappings: Vec<MappingStamp>,
    pub progress: u32,
    pub remote_baseline: u32,
    pub intent: ProgressIntent,
    pub state: DeliveryState,
    pub attempts: u32,
    pub next_attempt_at: u64,
    pub last_failure: Option<DeliveryFailure>,
    #[serde(default)]
    pub observed_date: Option<crate::ListDate>,
    #[serde(default)]
    pub playback_start: bool,
    #[serde(default)]
    pub first_date: Option<crate::ListDate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServiceCooldown {
    pub(crate) service: Service,
    pub(crate) until: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountPause {
    pub(crate) account: AccountKey,
    pub(crate) generation: NonZeroU64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outbox {
    pub(crate) next_revision: u64,
    pub(crate) pending: Vec<PendingProgress>,
    pub(crate) cooldowns: Vec<ServiceCooldown>,
    #[serde(default)]
    pub(crate) authentication_pauses: Vec<AccountPause>,
    #[serde(default)]
    pub(crate) edits: Vec<crate::PendingEdit>,
}
impl Default for Outbox {
    fn default() -> Self {
        Self {
            next_revision: 1,
            pending: vec![],
            cooldowns: vec![],
            authentication_pauses: vec![],
            edits: vec![],
        }
    }
}

/// Immutable lease for one exact attempt. A later patch or retry cannot be
/// acknowledged using an older lease, even when it targets the same media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryAttempt {
    patch: PendingProgress,
}
impl DeliveryAttempt {
    pub fn patch(&self) -> &PendingProgress {
        &self.patch
    }
    /// Progress-only writes leave every unrelated remote field untouched.
    /// Explicit replacements preserve a deliberate decrease through retries.
    pub fn progress_for_remote(&self, remote_progress: u32) -> u32 {
        match self.patch.intent {
            ProgressIntent::AutomaticForward => self.patch.progress.max(remote_progress),
            ProgressIntent::ExplicitReplacement => self.patch.progress,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Applied { progress: u32 },
    Failed(DeliveryFailure),
}

#[derive(Debug)]
pub enum TrackingError {
    MissingBinding,
    MissingTarget,
    MissingProjection,
    NoActiveMapping,
    StaleAccount,
    InvalidProgress,
    RevisionOverflow,
    AttemptOverflow,
    StaleAttempt,
    Projection(ProjectionError),
    Persistence(LoadError),
}
impl std::fmt::Display for TrackingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for TrackingError {}
impl From<ProjectionError> for TrackingError {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}
impl From<LoadError> for TrackingError {
    fn from(error: LoadError) -> Self {
        Self::Persistence(error)
    }
}

impl Outbox {
    pub fn pending(&self) -> &[PendingProgress] {
        &self.pending
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        for (index, patch) in self.pending.iter().enumerate() {
            if patch.intent_revision.get() >= self.next_revision
                || self.pending[index + 1..]
                    .iter()
                    .any(|other| other.intent_revision == patch.intent_revision)
                || patch.mappings.is_empty()
                || patch.mappings.iter().enumerate().any(|(index, mapping)| {
                    mapping.binding_id.is_empty()
                        || mapping.assignments.is_empty()
                        || patch.mappings[index + 1..]
                            .iter()
                            .any(|other| other.binding_id == mapping.binding_id)
                })
            {
                return Err(ValidationError(
                    "invalid tracking outbox revision or mapping snapshot".into(),
                ));
            }
            if patch.state == DeliveryState::InFlight
                && (patch.attempts == 0
                    || self.pending[index + 1..].iter().any(|other| {
                        other.target == patch.target && other.state == DeliveryState::InFlight
                    }))
            {
                return Err(ValidationError(
                    "invalid or concurrent delivery attempts for one target".into(),
                ));
            }
        }
        if self.next_revision == 0
            || self.cooldowns.iter().enumerate().any(|(index, cooldown)| {
                self.cooldowns[index + 1..]
                    .iter()
                    .any(|other| other.service == cooldown.service)
            })
        {
            return Err(ValidationError(
                "invalid outbox sequence or service cooldown".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn recover_interrupted(&mut self) {
        for patch in &mut self.pending {
            if patch.state == DeliveryState::InFlight {
                patch.state = DeliveryState::Uncertain;
            }
        }
        for edit in &mut self.edits {
            if edit.state == DeliveryState::InFlight {
                edit.state = DeliveryState::Uncertain;
            }
        }
    }

    fn enqueue(&mut self, mut patch: PendingProgress) -> Result<NonZeroU64, TrackingError> {
        let next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(TrackingError::RevisionOverflow)?;
        let revision =
            NonZeroU64::new(self.next_revision).ok_or(TrackingError::RevisionOverflow)?;
        patch.intent_revision = revision;
        if patch.intent == ProgressIntent::ExplicitReplacement {
            // An already transmitted write must finish before the replacement.
            // All unsent progress for this target is superseded by the edit.
            self.pending
                .retain(|old| old.target != patch.target || old.state == DeliveryState::InFlight);
        } else if let Some(old) = self
            .pending
            .iter_mut()
            .rev()
            .find(|old| old.target == patch.target)
            && old.intent == ProgressIntent::AutomaticForward
            && matches!(old.state, DeliveryState::Queued | DeliveryState::Uncertain)
            && old.account_generation == patch.account_generation
            && old.projection_revision == patch.projection_revision
            && old.mappings == patch.mappings
        {
            old.progress = old.progress.max(patch.progress);
            old.remote_baseline = old.remote_baseline.max(patch.remote_baseline);
            old.intent_revision = revision;
            self.next_revision = next_revision;
            return Ok(revision);
        }
        self.pending.push(patch);
        self.next_revision = next_revision;
        Ok(revision)
    }
}

impl TrackingState {
    pub(crate) fn enqueue_progress(
        &mut self,
        target_key: &TargetKey,
        intent: ProgressIntent,
    ) -> Result<NonZeroU64, TrackingError> {
        let target = self
            .targets
            .iter()
            .find(|target| target.key == *target_key)
            .ok_or(TrackingError::MissingTarget)?;
        let projection = self
            .projections
            .iter()
            .find(|projection| projection.target == *target_key)
            .ok_or(TrackingError::MissingProjection)?;
        if !self.accounts.iter().any(|account| {
            account.key == target_key.account && account.generation == projection.account_generation
        }) {
            return Err(TrackingError::StaleAccount);
        }
        let mappings: Vec<_> = self
            .bindings
            .iter()
            .filter(|binding| {
                binding.enabled
                    && binding.target == *target_key
                    && binding.account_generation == projection.account_generation
            })
            .map(MappingStamp::from_binding)
            .collect();
        if mappings.is_empty() {
            return Err(TrackingError::NoActiveMapping);
        }
        let progress = projection.proposal(target)?.progress;
        self.outbox.enqueue(PendingProgress {
            target: target_key.clone(),
            account_generation: projection.account_generation,
            projection_revision: projection.revision,
            intent_revision: NonZeroU64::MIN,
            mappings,
            progress,
            remote_baseline: projection.remote_baseline(),
            intent,
            state: DeliveryState::Queued,
            attempts: 0,
            next_attempt_at: 0,
            last_failure: None,
            observed_date: None,
            playback_start: false,
            first_date: None,
        })
    }

    pub(crate) fn observe_episode(
        &mut self,
        binding_id: &str,
        episode: &SourceEpisode,
        watched: bool,
    ) -> Result<Option<NonZeroU64>, TrackingError> {
        let binding = self
            .bindings
            .iter()
            .find(|binding| binding.id == binding_id)
            .ok_or(TrackingError::MissingBinding)?;
        let projection = self
            .projections
            .iter_mut()
            .find(|projection| projection.target == binding.target)
            .ok_or(TrackingError::MissingProjection)?;
        let accepted = projection.observe(binding, episode, watched)?;
        let target = binding.target.clone();
        if accepted {
            return self
                .enqueue_progress(&target, ProgressIntent::AutomaticForward)
                .map(Some);
        }
        Ok(None)
    }

    pub fn replace_progress(
        &mut self,
        target_key: &TargetKey,
        progress: u32,
        observations: Vec<Observation>,
    ) -> Result<NonZeroU64, TrackingError> {
        let target = self
            .targets
            .iter()
            .find(|target| target.key == *target_key)
            .ok_or(TrackingError::MissingTarget)?;
        if progress > i32::MAX as u32
            || target
                .final_episode_total
                .is_some_and(|total| progress > total.get())
        {
            return Err(TrackingError::InvalidProgress);
        }
        let projection = self
            .projections
            .iter_mut()
            .find(|projection| projection.target == *target_key)
            .ok_or(TrackingError::MissingProjection)?;
        projection.replace(progress, observations)?;
        self.enqueue_progress(target_key, ProgressIntent::ExplicitReplacement)
    }

    pub(crate) fn begin_delivery(
        &mut self,
        target: &TargetKey,
        now: u64,
    ) -> Result<Option<DeliveryAttempt>, TrackingError> {
        if self
            .outbox
            .edits
            .iter()
            .any(|edit| edit.target == *target && edit.state == DeliveryState::InFlight)
            || self
                .outbox
                .pending
                .iter()
                .any(|patch| patch.target == *target && patch.state == DeliveryState::InFlight)
        {
            return Ok(None);
        }
        let Some(index) = self
            .outbox
            .pending
            .iter()
            .position(|patch| patch.target == *target)
        else {
            return Ok(None);
        };
        let patch = &self.outbox.pending[index];
        if patch.intent == ProgressIntent::AutomaticForward
            && self.automatic_paused.contains(&target.account.service)
        {
            return Ok(None);
        }
        let account_current = self.active_accounts.contains(&target.account)
            && self.accounts.iter().any(|account| {
                account.key == target.account && account.generation == patch.account_generation
            });
        let mapping_current = patch.mappings.iter().all(|stamp| {
            self.bindings.iter().any(|binding| {
                binding.enabled
                    && binding.target == *target
                    && binding.account_generation == patch.account_generation
                    && stamp.matches(binding)
            })
        });
        let projection_current = self.projections.iter().any(|projection| {
            projection.target == *target
                && projection.account_generation == patch.account_generation
                && projection.revision == patch.projection_revision
        });
        if !account_current {
            self.outbox.pending[index].state = DeliveryState::StaleAccount;
            return Ok(None);
        }
        if !mapping_current || !projection_current {
            self.outbox.pending[index].state = DeliveryState::NeedsAlignment;
            return Ok(None);
        }
        if self.outbox.authentication_pauses.iter().any(|pause| {
            pause.account == target.account && pause.generation == patch.account_generation
        }) {
            self.outbox.pending[index].state = DeliveryState::AuthenticationRequired;
            return Ok(None);
        }
        if !matches!(
            patch.state,
            DeliveryState::Queued | DeliveryState::Uncertain
        ) || now < patch.next_attempt_at
            || self
                .outbox
                .cooldowns
                .iter()
                .any(|cooldown| cooldown.service == target.account.service && now < cooldown.until)
        {
            return Ok(None);
        }
        let patch = &mut self.outbox.pending[index];
        patch.attempts = patch
            .attempts
            .checked_add(1)
            .ok_or(TrackingError::AttemptOverflow)?;
        patch.state = DeliveryState::InFlight;
        Ok(Some(DeliveryAttempt {
            patch: patch.clone(),
        }))
    }

    pub fn finish_delivery(
        &mut self,
        attempt: &DeliveryAttempt,
        outcome: DeliveryOutcome,
        now: u64,
    ) -> Result<(), TrackingError> {
        let index = self
            .outbox
            .pending
            .iter()
            .position(|patch| *patch == attempt.patch)
            .ok_or(TrackingError::StaleAttempt)?;
        match outcome {
            DeliveryOutcome::Applied { progress } => {
                if let Some(projection) = self
                    .projections
                    .iter_mut()
                    .find(|projection| projection.target == attempt.patch.target)
                {
                    projection.acknowledge(
                        attempt.patch.account_generation,
                        attempt.patch.projection_revision,
                        progress,
                    );
                }
                self.outbox.pending.remove(index);
            }
            DeliveryOutcome::Failed(failure) => {
                let superseded = self.outbox.pending.iter().any(|patch| {
                    patch.target == attempt.patch.target
                        && patch.intent_revision > attempt.patch.intent_revision
                        && patch.intent == ProgressIntent::ExplicitReplacement
                });
                if superseded {
                    self.outbox.pending.remove(index);
                } else {
                    let patch = &mut self.outbox.pending[index];
                    patch.last_failure = Some(failure);
                    match failure {
                        DeliveryFailure::Transient => {
                            patch.state = DeliveryState::Uncertain;
                            let delay = (5_u64
                                .saturating_mul(1_u64 << patch.attempts.saturating_sub(1).min(10)))
                            .min(3600);
                            let jitter = patch
                                .intent_revision
                                .get()
                                .wrapping_mul(0x9e37_79b9)
                                .wrapping_add(now)
                                % (delay / 5 + 1);
                            patch.next_attempt_at =
                                now.saturating_add(delay).saturating_add(jitter);
                        }
                        DeliveryFailure::RateLimited { retry_at } => {
                            patch.state = DeliveryState::Uncertain;
                            patch.next_attempt_at = retry_at;
                        }
                        DeliveryFailure::AuthenticationRequired => {
                            patch.state = DeliveryState::AuthenticationRequired
                        }
                        DeliveryFailure::Rejected => patch.state = DeliveryState::Rejected,
                        DeliveryFailure::NeedsAlignment => {
                            patch.state = DeliveryState::NeedsAlignment
                        }
                    }
                }
                if failure == DeliveryFailure::AuthenticationRequired {
                    let pause = AccountPause {
                        account: attempt.patch.target.account.clone(),
                        generation: attempt.patch.account_generation,
                    };
                    if !self.outbox.authentication_pauses.contains(&pause) {
                        self.outbox.authentication_pauses.push(pause);
                    }
                }
                // A superseding local edit cannot bypass a service cooldown.
                if let DeliveryFailure::RateLimited { retry_at } = failure {
                    let service = attempt.patch.target.account.service;
                    if let Some(cooldown) = self
                        .outbox
                        .cooldowns
                        .iter_mut()
                        .find(|cooldown| cooldown.service == service)
                    {
                        cooldown.until = cooldown.until.max(retry_at);
                    } else {
                        self.outbox.cooldowns.push(ServiceCooldown {
                            service,
                            until: retry_at,
                        });
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn retry_target(&mut self, target: &TargetKey, now: u64) -> bool {
        let Some(patch) = self
            .outbox
            .pending
            .iter_mut()
            .find(|patch| patch.target == *target)
        else {
            return false;
        };
        if !matches!(
            patch.state,
            DeliveryState::Queued | DeliveryState::Uncertain
        ) {
            return false;
        }
        // begin_delivery still enforces the service cooldown and account pause.
        patch.next_attempt_at = now;
        true
    }

    pub(crate) fn resume_account(
        &mut self,
        account: &AccountKey,
        generation: NonZeroU64,
    ) -> Result<(), TrackingError> {
        if !self
            .accounts
            .iter()
            .any(|current| current.key == *account && current.generation == generation)
        {
            return Err(TrackingError::StaleAccount);
        }
        self.outbox
            .authentication_pauses
            .retain(|pause| pause.account != *account || pause.generation != generation);
        for patch in &mut self.outbox.pending {
            if patch.target.account == *account
                && patch.account_generation == generation
                && matches!(
                    patch.state,
                    DeliveryState::AuthenticationRequired | DeliveryState::StaleAccount
                )
            {
                patch.state = DeliveryState::Uncertain;
            }
        }
        for edit in &mut self.outbox.edits {
            if edit.target.account == *account
                && edit.account_generation == generation
                && matches!(
                    edit.state,
                    DeliveryState::AuthenticationRequired | DeliveryState::StaleAccount
                )
            {
                edit.state = DeliveryState::Uncertain;
            }
        }
        Ok(())
    }
}

impl Outbox {
    pub fn cancel_unsent(&mut self, target: &TargetKey) {
        self.pending
            .retain(|p| p.target != *target || p.state == DeliveryState::InFlight);
        self.edits
            .retain(|e| e.target != *target || e.state == DeliveryState::InFlight);
    }
}

impl Outbox {
    pub fn pause_alignment(&mut self, target: &TargetKey) {
        for pending in self
            .pending
            .iter_mut()
            .filter(|p| p.target == *target && p.state != DeliveryState::InFlight)
        {
            pending.state = DeliveryState::NeedsAlignment;
        }
        for edit in self
            .edits
            .iter_mut()
            .filter(|e| e.target == *target && e.state != DeliveryState::InFlight)
        {
            edit.state = DeliveryState::NeedsAlignment;
        }
    }
}

impl Outbox {
    pub fn defer_service(&mut self, service: Service, retry_at: u64) {
        if let Some(cooldown) = self.cooldowns.iter_mut().find(|c| c.service == service) {
            cooldown.until = cooldown.until.max(retry_at);
        } else {
            self.cooldowns.push(ServiceCooldown {
                service,
                until: retry_at,
            });
        }
    }
}

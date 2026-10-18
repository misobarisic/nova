use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use crate::{Binding, SourceEpisode, Target, TargetKey};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub episode: SourceEpisode,
    pub watched: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub target: TargetKey,
    pub account_generation: NonZeroU64,
    pub revision: NonZeroU64,
    remote_progress: u32,
    acknowledged_progress: u32,
    accepted_progress: u32,
    #[serde(default)]
    accepted_by_source: Vec<AcceptedSource>,
    observations: Vec<Observation>,
    #[serde(default)]
    mappings: Vec<crate::MappingStamp>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AcceptedSource {
    source: crate::SourceRef,
    progress: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressProposal {
    pub progress: u32,
    pub may_complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    WrongTarget,
    StaleAccount,
    DisabledBinding,
    UnmappedEpisode,
    RevisionOverflow,
    StaleMapping,
}

impl Projection {
    pub(crate) fn validate(&self) -> Result<(), crate::ValidationError> {
        for (index, accepted) in self.accepted_by_source.iter().enumerate() {
            if accepted.progress > i32::MAX as u32
                || [
                    &accepted.source.provider_id,
                    &accepted.source.source_id,
                    &accepted.source.media_type,
                ]
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 4096)
                || self.accepted_by_source[index + 1..]
                    .iter()
                    .any(|a| a.source == accepted.source)
            {
                return Err(crate::ValidationError(
                    "invalid accepted source contribution".into(),
                ));
            }
        }
        for (index, observation) in self.observations.iter().enumerate() {
            let episode = &observation.episode;
            if episode.episode_id.trim().is_empty()
                || episode.source.provider_id.trim().is_empty()
                || episode.source.source_id.trim().is_empty()
                || episode.source.media_type.trim().is_empty()
                || self.observations[index + 1..]
                    .iter()
                    .any(|other| other.episode == *episode)
            {
                return Err(crate::ValidationError(
                    "invalid or duplicate progress checkpoint".into(),
                ));
            }
        }
        Ok(())
    }

    /// Linking observes current flags without authorizing upload of old history.
    pub fn new(
        target: TargetKey,
        account_generation: NonZeroU64,
        remote_progress: u32,
        observations: Vec<Observation>,
    ) -> Self {
        Self {
            target,
            account_generation,
            revision: NonZeroU64::MIN,
            remote_progress,
            acknowledged_progress: remote_progress,
            accepted_progress: 0,
            accepted_by_source: vec![],
            observations,
            mappings: vec![],
        }
    }

    /// Only false→true observations contribute. Position updates and unwatch
    /// never lower remote progress; an unwatch followed by rewatch is a new event.
    pub fn observe(
        &mut self,
        binding: &Binding,
        episode: &SourceEpisode,
        watched: bool,
    ) -> Result<bool, ProjectionError> {
        if binding.target != self.target {
            return Err(ProjectionError::WrongTarget);
        }
        if binding.account_generation != self.account_generation {
            return Err(ProjectionError::StaleAccount);
        }
        if !binding.enabled {
            return Err(ProjectionError::DisabledBinding);
        }
        let assignment = binding
            .assignments
            .iter()
            .find(|assignment| {
                binding.source == episode.source && assignment.episode_id == episode.episode_id
            })
            .ok_or(ProjectionError::UnmappedEpisode)?;
        if let Some(stamp) = self
            .mappings
            .iter()
            .find(|stamp| stamp.binding_id == binding.id)
        {
            if !stamp.matches(binding) {
                return Err(ProjectionError::StaleMapping);
            }
        } else {
            self.mappings
                .push(crate::MappingStamp::from_binding(binding));
        }
        let previous = if let Some(observation) =
            self.observations.iter_mut().find(|o| o.episode == *episode)
        {
            let previous = observation.watched;
            observation.watched = watched;
            previous
        } else {
            self.observations.push(Observation {
                episode: episode.clone(),
                watched,
            });
            false
        };
        if watched && !previous {
            self.accepted_progress = self.accepted_progress.max(assignment.target_episode.get());
            if let Some(accepted) = self
                .accepted_by_source
                .iter_mut()
                .find(|a| a.source == episode.source)
            {
                accepted.progress = accepted.progress.max(assignment.target_episode.get());
            } else {
                self.accepted_by_source.push(AcceptedSource {
                    source: episode.source.clone(),
                    progress: assignment.target_episode.get(),
                });
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn remote_baseline(&self) -> u32 {
        self.remote_progress.max(self.acknowledged_progress)
    }

    pub fn proposal(&self, target: &Target) -> Result<ProgressProposal, ProjectionError> {
        if target.key != self.target {
            return Err(ProjectionError::WrongTarget);
        }
        let progress = self
            .remote_progress
            .max(self.acknowledged_progress)
            .max(self.accepted_progress);
        Ok(ProgressProposal {
            progress,
            may_complete: target.release_finished
                && target
                    .final_episode_total
                    .is_some_and(|total| progress >= total.get()),
        })
    }

    /// Record an ineligible origin without accepting progress from it.
    pub fn checkpoint(
        &mut self,
        binding: &Binding,
        episode: &SourceEpisode,
        watched: bool,
    ) -> Result<(), ProjectionError> {
        let accepted = self.accepted_progress;
        let sources = self.accepted_by_source.clone();
        let result = self.observe(binding, episode, watched);
        self.accepted_progress = accepted;
        self.accepted_by_source = sources;
        result.map(|_| ())
    }

    /// A late response to an earlier explicit edit cannot restore old progress.
    pub fn acknowledge(
        &mut self,
        account_generation: NonZeroU64,
        revision: NonZeroU64,
        progress: u32,
    ) -> bool {
        if account_generation != self.account_generation || revision != self.revision {
            return false;
        }
        self.acknowledged_progress = self.acknowledged_progress.max(progress);
        true
    }

    pub fn refresh_remote(
        &mut self,
        account_generation: NonZeroU64,
        revision: NonZeroU64,
        progress: u32,
    ) -> bool {
        if account_generation != self.account_generation || revision != self.revision {
            return false;
        }
        self.remote_progress = progress;
        true
    }

    /// Explicit replacement starts a new run with fresh watched checkpoints.
    /// The caller must serialize delivery and submit this replacement intent.
    pub fn replace(
        &mut self,
        progress: u32,
        observations: Vec<Observation>,
    ) -> Result<(), ProjectionError> {
        let revision = self
            .revision
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(ProjectionError::RevisionOverflow)?;
        self.revision = revision;
        self.remote_progress = progress;
        self.acknowledged_progress = progress;
        self.accepted_progress = 0;
        self.accepted_by_source.clear();
        self.observations = observations;
        Ok(())
    }
}

impl Projection {
    /// Revoking one source cannot leak its unsent maximum into another
    /// source's later event. Remote/acknowledged progress remains authoritative.
    /// Older schemas have no contribution ledger: retain their accepted scalar
    /// only when every contributing mapping remains allowed.
    pub fn retain_authorized_sources(
        &mut self,
        allowed: &[crate::SourceRef],
        revoked_mapping: bool,
    ) -> Result<bool, ProjectionError> {
        if !(revoked_mapping && self.accepted_by_source.is_empty() && self.accepted_progress > 0)
            && self.mappings.iter().all(|m| allowed.contains(&m.source))
            && self
                .accepted_by_source
                .iter()
                .all(|a| allowed.contains(&a.source))
        {
            return Ok(false);
        }
        let before = self.accepted_progress;
        let old_len = self.accepted_by_source.len();
        let legacy = self.accepted_by_source.is_empty();
        self.accepted_by_source
            .retain(|a| allowed.contains(&a.source));
        if legacy {
            if revoked_mapping || self.mappings.iter().any(|m| !allowed.contains(&m.source)) {
                self.accepted_progress = 0;
            }
        } else {
            self.accepted_progress = self
                .accepted_by_source
                .iter()
                .map(|a| a.progress)
                .max()
                .unwrap_or(0);
        }
        let changed = before != self.accepted_progress || old_len != self.accepted_by_source.len();
        if changed {
            self.revision = NonZeroU64::new(
                self.revision
                    .get()
                    .checked_add(1)
                    .ok_or(ProjectionError::RevisionOverflow)?,
            )
            .ok_or(ProjectionError::RevisionOverflow)?;
        }
        Ok(changed)
    }
    /// A confirmed mapping repair starts a revision without claiming old
    /// history. Outstanding attempts from the previous coverage stay stale.
    pub fn repair(
        &mut self,
        progress: u32,
        observations: Vec<Observation>,
    ) -> Result<(), ProjectionError> {
        self.replace(progress, observations)?;
        self.mappings.clear();
        Ok(())
    }
}

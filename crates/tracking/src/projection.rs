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
    observations: Vec<Observation>,
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
}

impl Projection {
    pub(crate) fn validate(&self) -> Result<(), crate::ValidationError> {
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
            observations,
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
            return Ok(true);
        }
        Ok(false)
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
        self.observations = observations;
        Ok(())
    }
}

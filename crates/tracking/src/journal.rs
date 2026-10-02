//! Semantic, local-only progress journal committed alongside playback history.
use crate::*;
use serde::{Deserialize, Serialize};
pub const EVENT_COUNTER_KEY: &str = "tracking:event_counter";
pub const JOURNAL_ERROR_KEY: &str = "tracking:journal_error";
pub const EVENT_PREFIX: &str = "tracking:events:";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventOrigin {
    Local,
    PairedDevice,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchChange {
    pub series_id: String,
    pub episode_id: String,
    pub watched: bool,
    #[serde(default)]
    pub started: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEvent {
    pub sequence: u64,
    pub origin: EventOrigin,
    pub changes: Vec<WatchChange>,
    #[serde(default)]
    pub date: Option<ListDate>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCheckpoint {
    pub binding_id: String,
    pub sequence: u64,
}
impl TrackingState {
    /// Paired-device observations move the checkpoint but never authorize a
    /// tracker upload. A later local unwatch/rewatch can still be accepted.
    pub fn consume_event(&mut self, event: &WatchEvent) -> Result<(), TrackingError> {
        for change in &event.changes {
            let bindings: Vec<_> = self
                .bindings
                .iter()
                .filter(|b| {
                    b.enabled
                        && b.source.source_id == change.series_id
                        && b.assignments
                            .iter()
                            .any(|a| a.episode_id == change.episode_id)
                        && self
                            .link_checkpoints
                            .iter()
                            .any(|c| c.binding_id == b.id && event.sequence > c.sequence)
                })
                .cloned()
                .collect();
            for binding in bindings {
                let episode = SourceEpisode {
                    source: binding.source.clone(),
                    episode_id: change.episode_id.clone(),
                };
                if event.origin == EventOrigin::Local
                    && self.active_accounts.contains(&binding.target.account)
                    && !self
                        .automatic_paused
                        .contains(&binding.target.account.service)
                {
                    let revision = match self.observe_episode(&binding.id, &episode, change.watched)
                    {
                        Ok(revision) => revision,
                        Err(TrackingError::Projection(
                            ProjectionError::StaleMapping
                            | ProjectionError::UnmappedEpisode
                            | ProjectionError::StaleAccount,
                        )) => {
                            if let Some(current) =
                                self.bindings.iter_mut().find(|b| b.id == binding.id)
                            {
                                current.enabled = false;
                            }
                            self.outbox.pause_alignment(&binding.target);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let start_revision =
                        if change.started && revision.is_none() {
                            Some(self.enqueue_progress(
                                &binding.target,
                                ProgressIntent::AutomaticForward,
                            )?)
                        } else {
                            None
                        };
                    if let Some(revision) = revision.or(start_revision)
                        && let Some(pending) = self
                            .outbox
                            .pending
                            .iter_mut()
                            .find(|p| p.intent_revision == revision)
                    {
                        pending.observed_date = event.date;
                        pending.first_date = pending.first_date.or(event.date);
                        pending.playback_start =
                            start_revision.is_some() && pending.progress == pending.remote_baseline;
                    }
                } else if let Some(projection) = self
                    .projections
                    .iter_mut()
                    .find(|p| p.target == binding.target)
                {
                    projection.checkpoint(&binding, &episode, change.watched)?;
                }
            }
        }
        // Advance only after all changes in the event: a bulk watched action
        // can contain several episodes belonging to the same binding. This
        // also makes a replay harmless after a later unwatch or replacement.
        for cursor in &mut self.link_checkpoints {
            cursor.sequence = cursor.sequence.max(event.sequence);
        }
        Ok(())
    }
}

use std::collections::HashSet;

use crate::{Binding, Target, TrackingState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError(pub String);
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ValidationError {}

fn invalid(message: &str) -> ValidationError {
    ValidationError(message.into())
}

pub fn validate_bindings(bindings: &[Binding], targets: &[Target]) -> Result<(), ValidationError> {
    let mut ids = HashSet::new();
    for binding in bindings {
        if binding.id.trim().is_empty() || !ids.insert(&binding.id) {
            return Err(invalid("empty or duplicate binding ID"));
        }
        if binding.source.provider_id.trim().is_empty()
            || binding.source.source_id.trim().is_empty()
            || binding.source.media_type.trim().is_empty()
        {
            return Err(invalid("incomplete source identity"));
        }
        let target = targets
            .iter()
            .find(|target| target.key == binding.target)
            .ok_or_else(|| invalid("binding has no target"))?;
        let mut episodes = HashSet::new();
        for assignment in &binding.assignments {
            if assignment.episode_id.trim().is_empty() || !episodes.insert(&assignment.episode_id) {
                return Err(invalid("empty or duplicate source episode assignment"));
            }
            if assignment.target_episode.get() > i32::MAX as u32
                || target
                    .final_episode_total
                    .is_some_and(|total| assignment.target_episode > total)
            {
                return Err(invalid("assignment exceeds target episode total"));
            }
        }
        if binding.enabled && binding.assignments.is_empty() {
            return Err(invalid("active binding has no confirmed assignments"));
        }
    }
    // Same source episode may contribute independently to MAL and AniList.
    // Within one account it must not silently update two releases or ordinals.
    for (index, a) in bindings.iter().enumerate().filter(|(_, b)| b.enabled) {
        for b in bindings[index + 1..].iter().filter(|b| b.enabled) {
            if a.source != b.source || a.target.account != b.target.account {
                continue;
            }
            for assignment in &a.assignments {
                if let Some(other) = b
                    .assignments
                    .iter()
                    .find(|other| other.episode_id == assignment.episode_id)
                    && (a.target != b.target || assignment.target_episode != other.target_episode)
                {
                    return Err(invalid("source episode has conflicting active coverage"));
                }
            }
        }
    }
    Ok(())
}

impl TrackingState {
    pub fn validate(&self) -> Result<(), ValidationError> {
        for (index, active) in self.active_accounts.iter().enumerate() {
            if !self.accounts.iter().any(|a| a.key == *active)
                || self.active_accounts[index + 1..]
                    .iter()
                    .any(|a| a.service == active.service)
            {
                return Err(invalid("invalid active tracking account"));
            }
        }
        for (index, account) in self.accounts.iter().enumerate() {
            if account.key.remote_user_id.get() > i32::MAX as u32 {
                return Err(invalid("account ID exceeds tracker limit"));
            }
            if self.accounts[index + 1..]
                .iter()
                .any(|other| other.key == account.key)
            {
                return Err(invalid("duplicate account"));
            }
        }
        for (index, target) in self.targets.iter().enumerate() {
            if target.key.remote_media_id.get() > i32::MAX as u32
                || target
                    .remote_entry_id
                    .is_some_and(|id| id.get() > i32::MAX as u32)
                || target
                    .final_episode_total
                    .is_some_and(|total| total.get() > i32::MAX as u32)
            {
                return Err(invalid("target exceeds tracker limits"));
            }
            if self.targets[index + 1..]
                .iter()
                .any(|other| other.key == target.key)
            {
                return Err(invalid("duplicate target"));
            }
            if !self
                .accounts
                .iter()
                .any(|account| account.key == target.key.account)
            {
                return Err(invalid("target has no account"));
            }
        }
        validate_bindings(&self.bindings, &self.targets)?;
        for binding in self.bindings.iter().filter(|binding| binding.enabled) {
            if !self.accounts.iter().any(|account| {
                account.key == binding.target.account
                    && account.generation == binding.account_generation
            }) {
                return Err(invalid(
                    "active binding belongs to a stale account generation",
                ));
            }
        }
        for (index, projection) in self.projections.iter().enumerate() {
            projection.validate()?;
            if !self
                .targets
                .iter()
                .any(|target| target.key == projection.target)
            {
                return Err(invalid("projection has no target"));
            }
            if self.projections[index + 1..]
                .iter()
                .any(|other| other.target == projection.target)
            {
                return Err(invalid("duplicate target projection"));
            }
            if !self.accounts.iter().any(|account| {
                account.key == projection.target.account
                    && account.generation == projection.account_generation
            }) {
                return Err(invalid("projection belongs to a stale account generation"));
            }
        }
        for (index, checkpoint) in self.link_checkpoints.iter().enumerate() {
            if !self.bindings.iter().any(|b| b.id == checkpoint.binding_id)
                || self.link_checkpoints[index + 1..]
                    .iter()
                    .any(|c| c.binding_id == checkpoint.binding_id)
            {
                return Err(invalid("invalid link journal checkpoint"));
            }
        }
        for (index, snapshot) in self.snapshots.iter().enumerate() {
            if snapshot.media.id != snapshot.target.remote_media_id
                || !self.targets.iter().any(|t| t.key == snapshot.target)
                || self.snapshots[index + 1..]
                    .iter()
                    .any(|s| s.target == snapshot.target)
                || snapshot.remote.as_ref().is_some_and(|r| {
                    r.account != snapshot.target.account
                        || r.media_id != snapshot.target.remote_media_id
                        || !snapshot.score_format.valid(r.score_tenths)
                        || !r.started.valid()
                        || !r.completed.valid()
                })
            {
                return Err(invalid("invalid target snapshot"));
            }
        }
        for (index, edit) in self.outbox.edits().iter().enumerate() {
            let snapshot = self
                .snapshots
                .iter()
                .find(|s| s.target == edit.target)
                .ok_or_else(|| invalid("edit has no target snapshot"))?;
            if edit.patch.progress.is_some()
                || edit
                    .patch
                    .validate(
                        edit.target.account.service,
                        edit.score_format.unwrap_or(snapshot.score_format),
                    )
                    .is_err()
                || edit.revision.get() >= self.outbox.next_revision
                || self.outbox.edits()[index + 1..].iter().any(|e| {
                    e.revision == edit.revision
                        || (e.target == edit.target
                            && e.state == crate::DeliveryState::InFlight
                            && edit.state == crate::DeliveryState::InFlight)
                })
            {
                return Err(invalid("invalid manual field edit"));
            }
        }
        self.outbox.validate()?;
        for patch in self.outbox.pending() {
            if !self.targets.iter().any(|target| target.key == patch.target) {
                return Err(invalid("outbox patch has no target"));
            }
        }
        Ok(())
    }
}

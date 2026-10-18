//! The actor owns prompt membership and score drafts. Slint only returns an
//! opaque token and destination ID; neither can authorize a stale mapping.
use super::*;

#[derive(Clone, PartialEq, Eq)]
struct DestinationDraft {
    pending: PendingCompletion,
    format: ScoreFormat,
    value: String,
}

pub(super) struct PromptDraft {
    token: String,
    destinations: Vec<DestinationDraft>,
}

fn destination_id(key: &TargetKey) -> String {
    let raw = serde_json::to_string(key).expect("target key serializes");
    format!("{:016x}", fnv1a(raw.as_bytes()))
}

/// Matching titles, seasons or episode counts alone are never identity proof.
fn same_unit(state: &TrackingState, a: &TargetKey, b: &TargetKey) -> bool {
    let final_count = |key: &TargetKey| {
        state
            .targets
            .iter()
            .find(|t| t.key == *key)
            .filter(|t| t.release_finished)
            .and_then(|t| t.final_episode_total)
    };
    let mal_id = |key: &TargetKey| match key.account.service {
        Service::MyAnimeList => Some(key.remote_media_id),
        Service::AniList => state
            .snapshots
            .iter()
            .find(|s| s.target == *key)
            .and_then(|s| s.media.mal_id),
    };
    a.media_kind == b.media_kind
        && final_count(a).is_some()
        && final_count(a) == final_count(b)
        && mal_id(a).is_some()
        && mal_id(a) == mal_id(b)
}

fn prompt_destinations(state: &TrackingState) -> Vec<DestinationDraft> {
    let pending = state.pending_completions();
    let Some(first) = pending.first() else {
        return vec![];
    };
    pending
        .iter()
        .filter(|p| p.target == first.target || same_unit(state, &first.target, &p.target))
        .filter_map(|pending| {
            state
                .snapshots
                .iter()
                .find(|s| s.target == pending.target)
                .map(|s| DestinationDraft {
                    pending: pending.clone(),
                    format: s.score_format,
                    value: String::new(),
                })
        })
        .collect()
}

fn current_draft(state: &TrackingState, draft: &PromptDraft, token: &str) -> bool {
    draft.token == token
        && !draft.destinations.is_empty()
        && prompt_destinations(state)
            .iter()
            .zip(&draft.destinations)
            .all(|(a, b)| a.pending == b.pending && a.format == b.format)
        && prompt_destinations(state).len() == draft.destinations.len()
}

fn score_value(value: &str, format: ScoreFormat) -> Option<u32> {
    let score = value.trim().replace(',', ".").parse::<f64>().ok()?;
    if !score.is_finite() || score <= 0.0 || score > f64::from(format.maximum()) / 10.0 {
        return None;
    }
    let tenths = (score * 10.0).round() as u32;
    (format.valid(tenths) && (f64::from(tenths) - score * 10.0).abs() < 0.001).then_some(tenths)
}

/// Build the whole decision before any persistence. An invalid destination
/// cannot partially acknowledge its siblings or enqueue one of their scores.
fn prompt_decision(
    state: &TrackingState,
    draft: &PromptDraft,
    token: &str,
    save: bool,
) -> Result<TrackingState, String> {
    if !current_draft(state, draft, token) {
        return Err(input_message());
    }
    if save
        && !draft
            .destinations
            .iter()
            .any(|d| !d.value.trim().is_empty())
    {
        return Err(input_message());
    }
    let mut next = state.clone();
    for row in &draft.destinations {
        if save && !row.value.trim().is_empty() {
            let score = score_value(&row.value, row.format).ok_or_else(input_message)?;
            next.enqueue_completion_rating(&row.pending.target, score)
                .map_err(|_| input_message())?;
        } else {
            next.dismiss_completion(&row.pending.target)
                .map_err(|_| input_message())?;
        }
    }
    Ok(next)
}

pub(super) fn keep_remote_rating(edit: &PendingEdit, remote: &RemoteEntry) -> bool {
    edit.only_if_unrated && remote.score_tenths > 0
}

fn apply_prompt_policy(
    state: &mut TrackingState,
    applied_revision: Option<u64>,
    policy: CompletionPromptPolicy,
) {
    // An off/on interval may finish before this owner processes either UI
    // command. Its revision still invalidates the previous interval's queue.
    // The first observation preserves valid prompts retained across restart.
    if applied_revision.is_some_and(|revision| revision != policy.revision)
        && state.completion.enabled
        && policy.enabled
    {
        state.configure_completion_prompts(false, policy.watermark);
    }
    state.configure_completion_prompts(policy.enabled, policy.watermark);
}

impl Coordinator {
    pub(super) fn configure_completion(&mut self, _enabled: bool) -> Result<(), String> {
        // Capture the policy at the UI notification, before later watched
        // events can advance the journal. Actor queue latency must not move
        // the enablement floor past an action made after the toggle.
        let policy = self.bridge.tracking_completion_policy()?;
        let mut state = self.store.state().clone();
        apply_prompt_policy(&mut state, self.completion_policy_revision, policy);
        if state != *self.store.state() {
            self.store.save(state).map_err(|_| storage_message())?;
        }
        self.completion_policy_revision = Some(policy.revision);
        self.refresh_completion_prompt();
        Ok(())
    }

    pub(super) fn refresh_completion_prompt(&mut self) {
        if self
            .completion_prompt
            .as_ref()
            .is_some_and(|draft| current_draft(self.store.state(), draft, &draft.token))
        {
            return;
        }
        let destinations = prompt_destinations(self.store.state());
        self.completion_prompt_revision = self.completion_prompt_revision.wrapping_add(1);
        self.completion_prompt_error.clear();
        self.completion_prompt = (!destinations.is_empty()).then(|| PromptDraft {
            token: self.completion_prompt_revision.to_string(),
            destinations,
        });
    }

    pub(super) fn completion_score(
        &mut self,
        token: &str,
        id: &str,
        value: String,
    ) -> Result<(), String> {
        self.apply_completion_shared(false)?;
        let enabled = self
            .bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .completion_rating_prompts_enabled;
        self.configure_completion(enabled)?;
        self.refresh_completion_prompt();
        let Some(draft) = self.completion_prompt.as_mut().filter(|d| d.token == token) else {
            return Ok(());
        };
        if let Some(row) = draft
            .destinations
            .iter_mut()
            .find(|d| destination_id(&d.pending.target) == id)
        {
            row.value = value;
            self.completion_prompt_error.clear();
        }
        Ok(())
    }

    pub(super) fn finish_completion_prompt(
        &mut self,
        token: &str,
        save: bool,
    ) -> Result<(), String> {
        let result = self.persist_completion_decision(token, save);
        if let Err(error) = &result {
            self.completion_prompt_error = error.clone();
        }
        result
    }

    fn persist_completion_decision(&mut self, token: &str, save: bool) -> Result<(), String> {
        self.apply_completion_shared(false)?;
        let enabled = self
            .bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .completion_rating_prompts_enabled;
        self.configure_completion(enabled)?;
        self.refresh_completion_prompt();
        let Some(draft) = self.completion_prompt.as_ref().filter(|d| d.token == token) else {
            return Ok(());
        };
        let state = prompt_decision(self.store.state(), draft, token, save)?;
        self.store.save(state).map_err(|_| storage_message())?;
        self.completion_prompt = None;
        self.completion_prompt_error.clear();
        // The rating and handled receipt are already durable. Failure to
        // publish merely leaves receipt synchronization for the next tick.
        if let Err(error) = self.publish_completion_shared() {
            self.notice = error;
        }
        self.refresh_completion_prompt();
        Ok(())
    }

    pub(super) fn publish_completion_prompt(&self) {
        let state = self.store.state();
        let title = self
            .completion_prompt
            .as_ref()
            .and_then(|d| d.destinations.first())
            .and_then(|d| {
                state
                    .snapshots
                    .iter()
                    .find(|s| s.target == d.pending.target)
            })
            .map(|s| s.media.title.clone())
            .unwrap_or_default();
        let subtitle = self
            .completion_prompt
            .as_ref()
            .and_then(|draft| draft.destinations.first())
            .and_then(|row| {
                state
                    .targets
                    .iter()
                    .find(|target| target.key == row.pending.target)
            })
            .and_then(|target| target.final_episode_total)
            .map(|count| text::completion_release_subtitle(count.get()))
            .unwrap_or_default();
        let token = self
            .completion_prompt
            .as_ref()
            .map(|d| d.token.clone())
            .unwrap_or_default();
        let sources: Vec<SourceRef> = self
            .completion_prompt
            .as_ref()
            .map(|draft| {
                state
                    .bindings
                    .iter()
                    .filter(|b| {
                        draft
                            .destinations
                            .iter()
                            .any(|d| d.pending.target == b.target)
                    })
                    .map(|b| b.source.clone())
                    .collect()
            })
            .unwrap_or_default();
        let rows: Vec<crate::CompletionDestination> = self
            .completion_prompt
            .as_ref()
            .map(|draft| {
                draft
                    .destinations
                    .iter()
                    .map(|row| {
                        let account = state
                            .accounts
                            .iter()
                            .find(|a| a.key == row.pending.target.account)
                            .map(|a| a.display_name.as_str())
                            .unwrap_or("");
                        crate::CompletionDestination {
                            id: destination_id(&row.pending.target).into(),
                            label: if account.is_empty() {
                                service_name(row.pending.target.account.service).into()
                            } else {
                                format!(
                                    "{} · {account}",
                                    service_name(row.pending.target.account.service)
                                )
                                .into()
                            },
                            max: row.format.maximum() as f32 / 10.0,
                            step: if row.format == ScoreFormat::Point10Decimal {
                                0.1
                            } else {
                                1.0
                            },
                            score: row.value.clone().into(),
                            selected: !row.value.trim().is_empty(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let can_save = self.completion_prompt.as_ref().is_some_and(|draft| {
            draft
                .destinations
                .iter()
                .any(|r| !r.value.trim().is_empty())
                && draft
                    .destinations
                    .iter()
                    .all(|r| r.value.trim().is_empty() || score_value(&r.value, r.format).is_some())
        });
        let weak = self.bridge.app.clone();
        let publication = self.completion_publication.clone();
        let revision = publication.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let error = self.completion_prompt_error.clone();
        let _ = crate::app::profiles::invoke_from_event_loop(move || {
            if publication.load(Ordering::Acquire) != revision {
                return;
            }
            if let Some(app) = weak.upgrade() {
                // Reuse an image already painted by Nova. Prompt publication
                // never starts an artwork fetch or trusts a title-only match.
                let poster = [
                    app.get_library(),
                    app.get_catalog(),
                    app.get_search_results(),
                ]
                .iter()
                .flat_map(|rows| rows.iter())
                .find(|row| {
                    row.is_loaded
                        && sources.iter().any(|s| {
                            s.source_id == row.id.as_str()
                                && s.media_type == row.media_type.as_str()
                        })
                })
                .map(|row| row.poster)
                .unwrap_or_default();
                app.set_completion_prompt_open(!token.is_empty());
                app.set_completion_prompt_token(token.into());
                app.set_completion_prompt_title(title.into());
                app.set_completion_prompt_subtitle(subtitle.into());
                app.set_completion_prompt_poster(poster);
                if let Some(model) = update_rows(app.get_completion_prompt_destinations(), rows) {
                    app.set_completion_prompt_destinations(model);
                }
                app.set_completion_prompt_can_save(can_save);
                app.set_completion_prompt_error(error.into());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn n(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }
    fn fixture() -> TrackingState {
        let mut state = TrackingState::default();
        state.configure_completion_prompts(true, 0);
        for (i, service) in [Service::MyAnimeList, Service::AniList]
            .into_iter()
            .enumerate()
        {
            let key = TargetKey {
                account: AccountKey {
                    service,
                    remote_user_id: n(7),
                },
                media_kind: MediaKind::Anime,
                remote_media_id: n(if i == 0 { 42 } else { 99 }),
            };
            let binding = Binding {
                id: format!("binding-{i}"),
                source: SourceRef {
                    provider_id: "addon".into(),
                    source_id: "native".into(),
                    media_type: "series".into(),
                },
                target: key.clone(),
                account_generation: NonZeroU64::MIN,
                mapping_revision: NonZeroU64::MIN,
                enabled: true,
                assignments: vec![Assignment {
                    episode_id: "ep".into(),
                    target_episode: n(1),
                }],
            };
            state.accounts.push(Account {
                key: key.account.clone(),
                generation: NonZeroU64::MIN,
                display_name: "Viewer".into(),
            });
            state.active_accounts.push(key.account.clone());
            state.targets.push(Target {
                key: key.clone(),
                remote_entry_id: None,
                final_episode_total: Some(n(1)),
                release_finished: true,
            });
            state.snapshots.push(TargetSnapshot {
                target: key.clone(),
                media: Media {
                    id: key.remote_media_id,
                    mal_id: Some(n(42)),
                    title: "Identical title".into(),
                    format: "TV".into(),
                    episodes: Some(n(1)),
                    finished: true,
                    year: None,
                },
                remote: None,
                score_format: if i == 0 {
                    ScoreFormat::Point10
                } else {
                    ScoreFormat::Point10Decimal
                },
                status_pinned: false,
                dates_pinned: false,
            });
            state
                .projections
                .push(Projection::new(key.clone(), NonZeroU64::MIN, 1, vec![]));
            state.completion.seen.push(key.clone());
            state.completion.pending.push(PendingCompletion {
                target: key,
                account_generation: NonZeroU64::MIN,
                mappings: vec![MappingStamp {
                    binding_id: binding.id.clone(),
                    revision: binding.mapping_revision,
                    source: binding.source.clone(),
                    assignments: binding.assignments.clone(),
                }],
                sequence: 1,
            });
            state.bindings.push(binding);
        }
        state
    }
    fn draft(state: &TrackingState) -> PromptDraft {
        PromptDraft {
            token: "current".into(),
            destinations: prompt_destinations(state),
        }
    }

    #[test]
    fn grouping_requires_cross_id_and_equal_finished_release_count() {
        let mut state = fixture();
        assert_eq!(prompt_destinations(&state).len(), 2);
        state.snapshots[1].media.mal_id = None;
        assert_eq!(prompt_destinations(&state).len(), 1);
        state.snapshots[1].media.mal_id = Some(n(43));
        assert_eq!(prompt_destinations(&state).len(), 1);
        state.snapshots[1].media.mal_id = Some(n(42));
        state.targets[1].final_episode_total = Some(n(2));
        assert!(!same_unit(
            &state,
            &state.targets[0].key,
            &state.targets[1].key
        ));
    }

    #[test]
    fn stale_token_account_and_mapping_cannot_authorize_a_decision() {
        let mut state = fixture();
        let mut draft = draft(&state);
        draft.destinations[0].value = "8".into();
        assert!(prompt_decision(&state, &draft, "old", true).is_err());
        state.bindings[0].mapping_revision = NonZeroU64::new(2).unwrap();
        assert!(prompt_decision(&state, &draft, "current", true).is_err());
        state = fixture();
        state.accounts[0].generation = NonZeroU64::new(2).unwrap();
        assert!(prompt_decision(&state, &draft, "current", true).is_err());
        state = fixture();
        state.snapshots[0].score_format = ScoreFormat::Point5;
        assert!(prompt_decision(&state, &draft, "current", true).is_err());
    }

    #[test]
    fn received_handled_receipt_invalidates_the_visible_group() {
        let state = fixture();
        let draft = draft(&state);
        let mut peer = state.clone();
        peer.dismiss_completion(&state.targets[0].key).unwrap();
        let records = completion_peer_records(&peer)
            .unwrap()
            .into_iter()
            .map(|(key, raw, _)| (key, raw))
            .collect::<Vec<_>>();
        let applied = apply_completion_peer_records(&state, &records).unwrap();
        assert!(!current_draft(&applied, &draft, "current"));
        let next = prompt_destinations(&applied);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].pending.target, state.targets[1].key);
    }

    #[test]
    fn group_save_is_score_only_and_dismisses_untouched_destinations() {
        let state = fixture();
        let mut draft = draft(&state);
        draft.destinations[0].value = "8".into();
        let next = prompt_decision(&state, &draft, "current", true).unwrap();
        assert_eq!(next.completion.handled.len(), 2);
        assert!(next.pending_completions().is_empty());
        let edits = next.outbox.edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].patch,
            EntryPatch {
                score_tenths: Some(80),
                ..Default::default()
            }
        );
        assert!(edits[0].only_if_unrated);
        assert!(state.completion.handled.is_empty());
        draft.destinations[1].value = "invalid".into();
        assert!(prompt_decision(&state, &draft, "current", true).is_err());
        assert!(state.outbox.edits().is_empty());
    }

    #[derive(Default)]
    struct MemoryStorage {
        raw: RefCell<Option<String>>,
        fail: Cell<bool>,
    }
    impl StateStorage for &MemoryStorage {
        fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
            Ok((key == TRACKING_STATE_KEY)
                .then(|| self.raw.borrow().clone())
                .flatten())
        }
        fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
            if self.fail.get() {
                return Err(nova_storage::Error::new(
                    nova_storage::ErrorKind::Transaction,
                    "test failure",
                ));
            }
            for (key, raw) in entries {
                if key == TRACKING_STATE_KEY {
                    *self.raw.borrow_mut() = raw.clone();
                }
            }
            Ok(())
        }
    }

    #[test]
    fn failed_storage_keeps_every_destination_pending_for_retry() {
        let storage = MemoryStorage::default();
        let mut store = Store::load(&storage).unwrap();
        store.save(fixture()).unwrap();
        let original = store.state().clone();
        let mut draft = draft(&original);
        draft.destinations[0].value = "9".into();
        storage.fail.set(true);
        let next = prompt_decision(store.state(), &draft, "current", true).unwrap();
        assert!(store.save(next).is_err());
        assert_eq!(*store.state(), original);
        assert_eq!(store.state().pending_completions().len(), 2);
    }

    #[test]
    fn fresh_remote_score_guard_preserves_existing_rating_for_prompt_only() {
        let state = fixture();
        let mut draft = draft(&state);
        draft.destinations[0].value = "8".into();
        let mut next = prompt_decision(&state, &draft, "current", true).unwrap();
        let mut edit = next.outbox.edits()[0].clone();
        let mut remote = RemoteEntry {
            account: edit.target.account.clone(),
            media_id: edit.target.remote_media_id,
            entry_id: None,
            progress: 1,
            status: ListStatus::Completed,
            score_tenths: 90,
            started: ListDate::default(),
            completed: ListDate::default(),
            repeating: false,
        };
        assert!(keep_remote_rating(&edit, &remote));
        remote.score_tenths = 0;
        assert!(!keep_remote_rating(&edit, &remote));
        remote.score_tenths = 90;
        edit.only_if_unrated = false;
        assert!(!keep_remote_rating(&edit, &remote));
        let attempt = next.begin_edit(&edit.target, 0).unwrap().unwrap();
        next.finish_edit(&attempt, Ok(remote.clone()), 0).unwrap();
        assert!(next.outbox.edits().is_empty());
        assert_eq!(next.snapshots[0].remote, Some(remote));
    }

    #[test]
    fn fractional_scores_follow_the_destination_format_and_locale() {
        assert_eq!(score_value("8,5", ScoreFormat::Point10Decimal), Some(85));
        assert_eq!(score_value("8.5", ScoreFormat::Point10), None);
        assert_eq!(score_value("0", ScoreFormat::Point10), None);
        assert_eq!(score_value("NaN", ScoreFormat::Point100), None);
        assert_eq!(score_value("101", ScoreFormat::Point100), None);
    }

    #[test]
    fn policy_restart_preserves_pending_but_coalesced_toggle_starts_a_new_interval() {
        let mut state = fixture();
        let policy = CompletionPromptPolicy {
            enabled: true,
            watermark: 10,
            revision: 1,
        };
        apply_prompt_policy(&mut state, None, policy);
        assert_eq!(state.pending_completions().len(), 2);
        assert_eq!(state.completion.watermark, 0);

        let next_interval = CompletionPromptPolicy {
            revision: 3,
            ..policy
        };
        apply_prompt_policy(&mut state, Some(1), next_interval);
        assert!(state.pending_completions().is_empty());
        assert!(state.completion.enabled);
        assert_eq!(state.completion.watermark, 10);
        assert_eq!(state.completion.seen.len(), 2);
    }

    #[test]
    fn delayed_actor_uses_the_captured_enablement_floor() {
        let mut state = fixture();
        state.completion = CompletionPrompts::default();
        state.projections = state
            .targets
            .iter()
            .map(|target| Projection::new(target.key.clone(), NonZeroU64::MIN, 0, vec![]))
            .collect();
        state.link_checkpoints = state
            .bindings
            .iter()
            .map(|binding| LinkCheckpoint {
                binding_id: binding.id.clone(),
                sequence: 0,
            })
            .collect();
        let captured = CompletionPromptPolicy {
            enabled: true,
            watermark: 5,
            revision: 2,
        };
        // Further events can already be queued when the actor catches up;
        // applying the policy never resamples their later counter.
        apply_prompt_policy(&mut state, Some(1), captured);
        assert_eq!(state.completion.watermark, 5);
        state
            .consume_event(&WatchEvent {
                sequence: 6,
                origin: EventOrigin::Local,
                changes: vec![WatchChange {
                    series_id: "native".into(),
                    episode_id: "ep".into(),
                    watched: true,
                    started: false,
                }],
                date: None,
            })
            .unwrap();
        assert_eq!(state.pending_completions().len(), 2);
        apply_prompt_policy(&mut state, Some(2), captured);
        assert_eq!(state.completion.watermark, 5);
        assert_eq!(state.pending_completions().len(), 2);
    }
}

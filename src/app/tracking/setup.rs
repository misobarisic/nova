//! Automatic drafts and explicit batch acceptance share the existing delivery guards.
use super::*;
impl Coordinator {
    pub(super) fn service_result<T>(
        &mut self,
        service: Service,
        result: Result<T, ApiError>,
    ) -> Result<T, String> {
        if let Err(ApiError::RateLimited { retry_at }) = result.as_ref() {
            let mut state = self.store.state().clone();
            state.outbox.defer_service(service, *retry_at);
            self.store.save(state).map_err(|_| storage_message())?;
        }
        result.map_err(api_message)
    }
    fn setup_details(
        &mut self,
        service: Service,
        id: NonZeroU32,
        fresh: bool,
    ) -> Result<ReleaseDetails, String> {
        if let Some(until) = self
            .store
            .state()
            .outbox
            .service_retry_at(service, now_secs())
        {
            return Err(api_message(ApiError::RateLimited { retry_at: until }));
        }
        if !fresh && let Some(details) = self.cache.release(service, id, now_secs()) {
            return Ok(details);
        }
        let result = self
            .session(service)?
            .client
            .release_details(id, now_secs());
        let details = self.service_result(service, result)?;
        self.cache
            .insert_release(service, details.clone(), now_secs());
        if let Ok(raw) = serde_json::to_string(&self.cache) {
            let _ = storage::try_set_cached_str(CATALOG_CACHE_KEY, &raw);
        }
        Ok(details)
    }
    pub(super) fn suggest_seed(&mut self, service: Service, generation: u64) -> Result<(), String> {
        let context = self.context.clone().ok_or_else(input_message)?;
        let account = self
            .session(service)?
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        // Confirmed links are stronger anchors than a new title search.
        let existing = self
            .store
            .state()
            .bindings
            .iter()
            .filter(|b| b.enabled && b.source == context.source && b.target.account == account)
            .min_by_key(|b| {
                b.assignments
                    .iter()
                    .filter_map(|a| {
                        context
                            .episode_info
                            .iter()
                            .find(|e| e.id == a.episode_id)
                            .map(|e| (e.season.unwrap_or(u32::MAX), e.number.unwrap_or(u32::MAX)))
                    })
                    .min()
            })
            .map(|b| b.target.remote_media_id);
        // The first ranked result is the default draft. Acceptance remains
        // explicit, and the review exposes alternatives for a wrong match.
        let seed = existing.or_else(|| self.candidates.first().map(|m| m.id));
        if let Some(seed) = seed {
            self.build_setup(service, seed, generation, 32)?;
        }
        Ok(())
    }
    pub(super) fn build_setup(
        &mut self,
        service: Service,
        seed: NonZeroU32,
        generation: u64,
        budget: usize,
    ) -> Result<(), String> {
        let context = self.context.clone().ok_or_else(input_message)?;
        let mut pending = std::collections::VecDeque::from([seed]);
        let mut details = vec![];
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = pending.pop_front() {
            if !self.current(generation) {
                return Ok(());
            }
            if !visited.insert(id) {
                continue;
            }
            if details.len() >= budget {
                break;
            }
            let release = self.setup_details(service, id, false)?;
            if context.source.media_type != "movie" {
                // Alternatives are discovered to find TV versions, but the
                // coverage engine only traverses the episodic sequel chain.
                for related in &release.relations {
                    if matches!(
                        related.relation,
                        ReleaseRelation::Prequel
                            | ReleaseRelation::Sequel
                            | ReleaseRelation::Alternative
                    ) && !visited.contains(&related.id)
                    {
                        pending.push_back(related.id);
                    }
                }
            }
            details.push(release);
            if context.source.media_type != "movie"
                && details
                    .iter()
                    .any(|d| d.media.id == seed && episodic_format(&d.media.format))
            {
                let chain = release_chain(seed, &details);
                let prequels_known = details.iter().all(|d| {
                    d.relations
                        .iter()
                        .filter(|r| r.relation == ReleaseRelation::Prequel)
                        .all(|r| details.iter().any(|known| known.media.id == r.id))
                });
                let proposal = propose_coverage(&context.episode_info, &chain);
                let regular = context
                    .episode_info
                    .iter()
                    .filter(|e| e.season.is_some_and(|s| s > 0) && e.number.is_some_and(|n| n > 0))
                    .count();
                if prequels_known
                    && regular > 0
                    && proposal
                        .releases
                        .iter()
                        .map(|r| r.assignments.len())
                        .sum::<usize>()
                        == regular
                {
                    break;
                }
            }
        }
        let Some(anchor) = details.iter().find(|d| d.media.id == seed).cloned() else {
            return Ok(());
        };
        let proposal = if context.source.media_type == "movie" {
            if !anchor.media.format.eq_ignore_ascii_case("movie") {
                MappingProposal {
                    releases: vec![],
                    unresolved: context.episode_info.iter().map(|e| e.id.clone()).collect(),
                }
            } else {
                MappingProposal {
                    releases: vec![ProposedRelease {
                        details: anchor,
                        season: 1,
                        first: 1,
                        last: 1,
                        assignments: vec![Assignment {
                            episode_id: context.source.source_id.clone(),
                            target_episode: NonZeroU32::MIN,
                        }],
                        check_split: false,
                    }],
                    unresolved: vec![],
                }
            }
        } else {
            let anchor_id = if episodic_format(&anchor.media.format) {
                Some(seed)
            } else {
                let alternatives: Vec<_> = anchor
                    .relations
                    .iter()
                    .filter(|r| r.relation == ReleaseRelation::Alternative)
                    .filter_map(|r| {
                        details
                            .iter()
                            .find(|d| d.media.id == r.id && episodic_format(&d.media.format))
                    })
                    .collect();
                if alternatives.len() == 1 {
                    Some(alternatives[0].media.id)
                } else {
                    None
                }
            };
            let chain = anchor_id
                .map(|id| release_chain(id, &details))
                .unwrap_or_default();
            propose_coverage(&context.episode_info, &chain)
        };
        let session = self.session(service)?;
        let mut draft = SetupDraft {
            service,
            account: session
                .client
                .viewer()
                .map_err(api_message)?
                .account
                .clone(),
            epoch: session.epoch,
            proposal,
            revision: 0,
        };
        // Existing identical coverage is already confirmed. A conflicting
        // release is left unresolved instead of replacing it automatically.
        for release in &mut draft.proposal.releases {
            let mut conflicts = vec![];
            release.assignments.retain(|a| {
                let links: Vec<_> = self
                    .store
                    .state()
                    .bindings
                    .iter()
                    .filter(|b| {
                        b.enabled && b.source == context.source && b.target.account == draft.account
                    })
                    .flat_map(|b| {
                        b.assignments
                            .iter()
                            .filter(move |old| old.episode_id == a.episode_id)
                            .map(move |old| (b, old))
                    })
                    .collect();
                if links.iter().any(|(b, old)| {
                    b.target.remote_media_id != release.details.media.id
                        || old.target_episode != a.target_episode
                }) {
                    conflicts.push(a.episode_id.clone());
                    return false;
                }
                !links.iter().any(|_| true)
            });
            draft.proposal.unresolved.extend(conflicts);
        }
        draft
            .proposal
            .releases
            .retain(|r| !r.assignments.is_empty());
        self.setup_revision = self
            .setup_revision
            .checked_add(1)
            .ok_or_else(input_message)?;
        draft.revision = self.setup_revision;
        self.validate_setup(&draft)?;
        self.setup = Some(draft);
        self.choice = None;
        self.busy = false;
        self.notice = text::tr("Review the suggested releases, then start tracking.").into();
        Ok(())
    }
    pub(super) fn refresh_setup_coverage(&self, draft: &mut SetupDraft) {
        if let Some(context) = self.context.as_ref() {
            let mut covered: std::collections::HashSet<_> = draft
                .proposal
                .releases
                .iter()
                .flat_map(|r| r.assignments.iter().map(|a| a.episode_id.as_str()))
                .collect();
            for binding in self.store.state().bindings.iter().filter(|b| {
                b.enabled && b.source == context.source && b.target.account == draft.account
            }) {
                covered.extend(binding.assignments.iter().map(|a| a.episode_id.as_str()));
            }
            draft.proposal.unresolved = context
                .episode_info
                .iter()
                .filter(|e| !covered.contains(e.id.as_str()))
                .map(|e| e.id.clone())
                .collect();
        }
    }
    pub(super) fn validate_setup(&self, draft: &SetupDraft) -> Result<(), String> {
        let source = &self.context.as_ref().ok_or_else(input_message)?.source;
        let mut state = self.store.state().clone();
        for (index, release) in draft.proposal.releases.iter().enumerate() {
            let key = TargetKey {
                account: draft.account.clone(),
                media_kind: MediaKind::Anime,
                remote_media_id: release.details.media.id,
            };
            let old = state
                .bindings
                .iter()
                .find(|b| b.enabled && b.source == *source && b.target == key)
                .cloned();
            let mut assignments = release.assignments.clone();
            if let Some(old) = &old {
                for assignment in &old.assignments {
                    if !assignments
                        .iter()
                        .any(|a| a.episode_id == assignment.episode_id)
                    {
                        assignments.push(assignment.clone());
                    }
                }
                state.bindings.retain(|b| b.id != old.id);
            }
            if !state.targets.iter().any(|t| t.key == key) {
                state.targets.push(Target {
                    key: key.clone(),
                    remote_entry_id: None,
                    final_episode_total: release.details.media.episodes,
                    release_finished: release.details.media.finished,
                });
            }
            state.bindings.push(Binding {
                id: format!("setup-preview-{index}"),
                source: source.clone(),
                target: key,
                account_generation: self.session(draft.service)?.generation,
                mapping_revision: NonZeroU64::MIN,
                enabled: true,
                assignments,
            });
        }
        validate_bindings(&state.bindings, &state.targets).map_err(|_| {
            text::tr("This alignment overlaps another active release. Edit that link first.")
                .to_string()
        })
    }
    pub(super) fn adjust_setup(&mut self, index: usize, generation: u64) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let draft = self.setup.as_ref().ok_or_else(input_message)?;
        let release = draft
            .proposal
            .releases
            .get(index)
            .ok_or_else(input_message)?;
        let repair_id = self.context.as_ref().and_then(|c| {
            self.store
                .state()
                .bindings
                .iter()
                .find(|b| {
                    b.source == c.source
                        && b.target.account == draft.account
                        && b.target.remote_media_id == release.details.media.id
                })
                .map(|b| b.id.clone())
        });
        self.choice = Some(Choice {
            service: draft.service,
            media: release.details.media.clone(),
            remote: None,
            assignments: release.assignments.clone(),
            repair_id,
            validated: true,
            proposal_index: Some(index),
        });
        self.notice = text::tr("Adjust this release, then save the change to your review.").into();
        Ok(())
    }
    pub(super) fn accept_setup(
        &mut self,
        revision: String,
        include_history: bool,
        generation: u64,
    ) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let draft = self.setup.as_ref().ok_or_else(input_message)?;
        let session = self.session(draft.service)?;
        if revision != draft.revision.to_string()
            || session.epoch != draft.epoch
            || session.client.viewer().map_err(api_message)?.account != draft.account
        {
            return Err(input_message());
        }
        if draft.proposal.releases.is_empty() {
            return Err(input_message());
        }
        let context = self.context.as_ref().ok_or_else(input_message)?;
        let mut choices = vec![];
        for release in &draft.proposal.releases {
            let old = self.store.state().bindings.iter().find(|b| {
                b.enabled
                    && b.source == context.source
                    && b.target.account == draft.account
                    && b.target.remote_media_id == release.details.media.id
            });
            let mut assignments = release.assignments.clone();
            if let Some(old) = old {
                for a in &old.assignments {
                    if !assignments.iter().any(|n| n.episode_id == a.episode_id) {
                        assignments.push(a.clone());
                    }
                }
            }
            choices.push(Choice {
                service: draft.service,
                media: release.details.media.clone(),
                remote: None,
                assignments,
                repair_id: old.map(|b| b.id.clone()),
                validated: true,
                proposal_index: None,
            });
        }
        let originals: Vec<_> = draft
            .proposal
            .releases
            .iter()
            .map(|r| r.details.clone())
            .collect();
        let service = draft.service;
        self.busy = true;
        self.publish();
        for old in originals {
            let fresh = self.setup_details(service, old.media.id, true)?;
            if fresh.media.episodes != old.media.episodes
                || fresh.start != old.start
                || fresh.end != old.end
                || fresh.media.format != old.media.format
            {
                self.setup = None;
                return Err(text::tr(
                    "Release metadata changed. Reload suggestions before linking.",
                )
                .into());
            }
            if !self.current(generation) {
                return Ok(());
            }
        }
        self.commit_links(choices, include_history, generation)?;
        self.setup = None;
        self.busy = false;
        Ok(())
    }
}

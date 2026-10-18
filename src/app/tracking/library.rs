//! Whole-library discovery is independent of the currently open Detail editor.
//! Only explicit bulk acceptance creates new targets; recurring work can extend
//! an already approved ongoing release without changing any old assignment.
use super::*;
use crate::app::tracking::library_policy::{self, Origin};
const LIBRARY_LIMIT: usize = 2_000;
const DISCOVERY_REQUEST_LIMIT: usize = 72;
const LIBRARY_KEY: &str = "tracking:library:v1:";
#[derive(Clone)]
pub(super) struct LibraryReview {
    service: Service,
    account: AccountKey,
    epoch: u64,
    generation: u64,
    revision: String,
    decisions: Vec<LibraryDecision>,
    activate_full: bool,
}
#[derive(Default)]
pub(super) struct LibraryRuntime {
    pub modes: Vec<LibraryTracking>,
    pub review: Option<LibraryReview>,
    pub busy: Option<Service>,
    next_scan: u64,
    prompt: Option<(Service, SourceRef, String, String)>,
    deferred: [bool; 2],
}
fn library_key(account: &AccountKey) -> String {
    format!("{LIBRARY_KEY}{}", serde_json::to_string(account).unwrap())
}
fn library_request(budget: &mut usize) -> bool {
    if *budget == 0 {
        false
    } else {
        *budget -= 1;
        true
    }
}
fn library_context(entry: &LibraryEntry) -> SourceContext {
    let evidence = read_json_result::<SourceEvidence>(&evidence_key(&entry.type_, &entry.id))
        .ok()
        .flatten();
    let ids = evidence.as_ref().map(|e| e.ids.clone()).unwrap_or_else(|| {
        providers::normalize_external_ids(&HashMap::new(), &entry.id, &entry.type_)
    });
    let videos = read_episodes_cache_for(&entry.type_, &entry.id).unwrap_or_default();
    let episode_info = if entry.type_ == "movie" {
        vec![EpisodeInfo {
            id: entry.id.clone(),
            season: Some(1),
            number: Some(1),
            title: entry.name.clone(),
            released: ListDate::default(),
        }]
    } else {
        tracking_episode_info(&videos)
    };
    SourceContext {
        source: SourceRef {
            provider_id: "nova".into(),
            source_id: entry.id.clone(),
            media_type: entry.type_.clone(),
        },
        title: entry.name.clone(),
        ids,
        historical_ids: vec![],
        aliases: evidence
            .as_ref()
            .map(|e| e.aliases.clone())
            .unwrap_or_default(),
        year: evidence
            .as_ref()
            .and_then(|e| e.year)
            .or_else(|| entry.year.get(..4).and_then(|y| y.parse().ok())),
        episodes: episode_info
            .iter()
            .map(|e| (e.id.clone(), e.title.clone()))
            .collect(),
        episode_info,
    }
}
fn library_fingerprint(
    context: &SourceContext,
    entry: &LibraryEntry,
    account: &AccountKey,
    state: &TrackingState,
) -> String {
    let bindings: Vec<_> = state
        .bindings
        .iter()
        .filter(|b| b.source == context.source && b.target.account == *account)
        .collect();
    let raw = serde_json::to_vec(&(
        &context.source,
        &context.ids,
        &context.title,
        &context.aliases,
        context.year,
        &entry.genres,
        &context.episode_info,
        bindings,
    ))
    .unwrap_or_default();
    format!("{:016x}:{}", fnv1a(&raw), raw.len())
}
fn anime_claim(context: &SourceContext) -> bool {
    [
        providers::IdNamespace::MalAnime,
        providers::IdNamespace::AnilistAnime,
    ]
    .into_iter()
    .any(|n| !matches!(context.ids.resolve_id(n), providers::IdResolution::Missing))
}
fn library_prompt_revision(decision: &LibraryDecision) -> String {
    let candidate = serde_json::to_vec(&decision.proposal).unwrap_or_default();
    format!("{}:{:016x}", decision.fingerprint, fnv1a(&candidate))
}
impl Coordinator {
    fn library_guard(
        &self,
        service: Service,
        generation: u64,
        account: &AccountKey,
        epoch: u64,
    ) -> Result<(), String> {
        if !self.live()
            || self.bridge.tracking().library_generation[service_index(service)]
                .load(Ordering::Acquire)
                != generation
        {
            return Err(input_message());
        }
        let session = self.session(service)?;
        if session.epoch != epoch
            || session.client.viewer().map_err(api_message)?.account != *account
        {
            return Err(input_message());
        }
        Ok(())
    }
    fn library_load(&mut self, account: &AccountKey) -> Result<usize, String> {
        if let Some(i) = self
            .library
            .modes
            .iter()
            .position(|m| m.account == *account)
        {
            let policy = library_policy::read(account)?;
            self.library.modes[i].enabled = policy.enabled;
            self.library.modes[i].include_history = policy.include_history;
            self.library.modes[i].excluded = policy.excluded;
            return Ok(i);
        }
        let loaded = read_json_result::<LibraryTracking>(&library_key(account))
            .map_err(|_| storage_message())?;
        let created = loaded.is_none();
        let boundary = if created {
            let owner = nova_sync::local_store().map_err(|_| storage_message())?;
            owner
                .lock()
                .unwrap()
                .extra_value(&profiles::storage_key(EVENT_COUNTER_KEY))
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<u64>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or(0)
        } else {
            0
        };
        let mut mode = loaded.unwrap_or_else(|| LibraryTracking {
            account: account.clone(),
            enabled: false,
            include_history: false,
            event_boundary: boundary,
            excluded: vec![],
            decisions: vec![],
        });
        if mode.account != *account || mode.decisions.len() > LIBRARY_LIMIT {
            return Err(input_message());
        }
        let policy = library_policy::read(account)?;
        mode.enabled = policy.enabled;
        mode.include_history = policy.include_history;
        mode.excluded = policy.excluded;
        self.library.modes.push(mode);
        let index = self.library.modes.len() - 1;
        if created {
            self.library_save(index)?;
        }
        Ok(index)
    }
    fn library_save(&self, index: usize) -> Result<(), String> {
        let mode = &self.library.modes[index];
        let raw = serde_json::to_string(mode).map_err(|_| storage_message())?;
        storage::try_set_str(&library_key(&mode.account), &raw).map_err(|_| storage_message())
    }
    pub(super) fn library_action(
        &mut self,
        service: Service,
        action: i32,
        revision: String,
        include_history: bool,
        generation: u64,
    ) -> Result<(), String> {
        let session = self.session(service)?;
        let account = session
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        let epoch = session.epoch;
        self.library_guard(service, generation, &account, epoch)?;
        let index = self.library_load(&account)?;
        match action {
            0 | 3 => {
                self.library.review = None;
                self.library.busy = Some(service);
                self.publish();
                let result = self.library_scan(service, action == 3, true);
                self.library.busy = None;
                result?;
                self.library_guard(service, generation, &account, epoch)?;
                let decisions = self.library.modes[index]
                    .decisions
                    .iter()
                    .filter(|d| {
                        !d.proposal.releases.is_empty()
                            && !self.library.modes[index].excluded.contains(&d.source)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let raw = serde_json::to_vec(&decisions).map_err(|_| storage_message())?;
                let revision = format!("{}:{generation}:{:016x}", now_secs(), fnv1a(&raw));
                self.library.review = Some(LibraryReview {
                    service,
                    account,
                    epoch,
                    generation,
                    revision,
                    decisions,
                    activate_full: true,
                });
            }
            1 => {
                let review = self.library.review.clone().ok_or_else(input_message)?;
                if review.service != service
                    || review.account != account
                    || review.epoch != epoch
                    || review.generation != generation
                    || review.revision != revision
                {
                    return Err(input_message());
                }
                // A new click generation cancels earlier work; the revision is
                // bound to the authenticated draft, not to a transient UI row.
                self.library.busy = Some(service);
                self.publish();
                let result = self.library_commit(
                    service,
                    &review.decisions,
                    include_history,
                    generation,
                    false,
                    review.activate_full,
                );
                self.library.busy = None;
                let boundary = result?;
                self.library_guard(service, generation, &account, epoch)?;
                if review.activate_full {
                    self.library.modes[index].enabled = true;
                    self.library.modes[index].include_history = include_history;
                    self.library.modes[index].event_boundary = boundary;
                    library_policy::write_mode(&account, true, include_history)?;
                }
                self.library_save(index)?;
                self.library.review = None;
                self.library.next_scan = 0;
            }
            2 => {
                library_policy::write_mode(
                    &account,
                    false,
                    self.library.modes[index].include_history,
                )?;
                self.library.modes[index].enabled = false;
                self.library_save(index)?;
                self.library.review = None;
                self.library.prompt = None;
                self.library_reconcile_policy()?;
            }
            4 => {
                self.library.review = None;
            }
            5 | 6 => {
                if let Some((s, source, fingerprint, _)) = self.library.prompt.take()
                    && s == service
                    && fingerprint == revision
                {
                    for decision in &mut self.library.modes[index].decisions {
                        if decision.source == source
                            && library_prompt_revision(decision) == fingerprint
                        {
                            decision.dismissed = true;
                        }
                    }
                    self.library_save(index)?;
                    if action == 5 {
                        self.library.busy = Some(service);
                        self.publish();
                        let result = self.library_scan(service, false, false);
                        self.library.busy = None;
                        result?;
                        let decisions = self.library.modes[index]
                            .decisions
                            .iter()
                            .filter(|d| d.source == source && !d.proposal.releases.is_empty())
                            .cloned()
                            .collect::<Vec<_>>();
                        let raw = serde_json::to_vec(&decisions).map_err(|_| storage_message())?;
                        self.library.review = Some(LibraryReview {
                            service,
                            account: account.clone(),
                            epoch,
                            generation,
                            revision: format!("source:{generation}:{:016x}", fnv1a(&raw)),
                            decisions,
                            activate_full: false,
                        });
                    }
                }
            }
            7 => {
                let source = self
                    .context
                    .as_ref()
                    .ok_or_else(input_message)?
                    .source
                    .clone();
                if serde_json::to_string(&source).map_err(|_| input_message())? != revision {
                    return Err(input_message());
                }
                let excluded = self.library.modes[index].excluded.contains(&source);
                self.library_exclude_source(&account, &source, !excluded)?;
                self.library.next_scan = 0;
            }
            _ => return Err(input_message()),
        }
        Ok(())
    }
    pub(super) fn library_tick(&mut self) {
        if now_secs() < self.library.next_scan {
            return;
        }
        self.library.next_scan = now_secs().saturating_add(30);
        for service in [Service::MyAnimeList, Service::AniList] {
            let Ok(session) = self.session(service) else {
                continue;
            };
            let Ok(viewer) = session.client.viewer() else {
                continue;
            };
            let account = viewer.account.clone();
            let Ok(index) = self.library_load(&account) else {
                continue;
            };
            let whole = self.library.modes[index].enabled
                || self
                    .library
                    .review
                    .as_ref()
                    .is_some_and(|r| r.service == service && r.activate_full);
            if self.store.state().automatic_paused.contains(&service)
                || (!whole
                    && !self
                        .store
                        .state()
                        .bindings
                        .iter()
                        .any(|b| b.enabled && b.target.account == account))
            {
                continue;
            }
            if let Err(error) = self.library_scan(service, false, whole) {
                self.notice = error;
            }
        }
    }
    fn library_scan(&mut self, service: Service, fresh: bool, whole: bool) -> Result<(), String> {
        if let Some(retry_at) = self
            .store
            .state()
            .outbox
            .service_retry_at(service, now_secs())
        {
            return Err(api_message(ApiError::RateLimited { retry_at }));
        }
        let session = self.session(service)?;
        let account = session
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        let epoch = session.epoch;
        let generation = self.bridge.tracking().library_generation[service_index(service)]
            .load(Ordering::Acquire);
        let index = self.library_load(&account)?;
        let entries = self
            .bridge
            .personal_state()
            .ok_or_else(input_message)?
            .entries
            .clone();
        self.library.deferred[service_index(service)] = false;
        if entries.len() > LIBRARY_LIMIT {
            return Err(text::tr("Library is too large for one tracking review.").into());
        }
        self.library.modes[index].decisions.retain(|d| {
            entries
                .iter()
                .any(|e| e.id == d.source.source_id && e.type_ == d.source.media_type)
        });
        let mut budget = DISCOVERY_REQUEST_LIMIT;
        for entry in entries {
            self.library_guard(service, generation, &account, epoch)?;
            let context = library_context(&entry);
            let linked = self.store.state().bindings.iter().any(|b| {
                b.enabled
                    && b.source == context.source
                    && b.target.account == account
                    && self.library_binding_allowed(b).unwrap_or(false)
            });
            if !whole && !linked {
                continue;
            }
            if self.library.modes[index].excluded.contains(&context.source) {
                continue;
            }
            let base = library_fingerprint(&context, &entry, &account, self.store.state());
            let fingerprint = self.library_discovery_fingerprint(
                &context,
                &entry,
                &account,
                self.library.modes[index].event_boundary,
            )?;
            if !fresh
                && self.library.modes[index]
                    .decisions
                    .iter()
                    .any(|d| d.source == context.source && d.fingerprint == fingerprint)
            {
                continue;
            }
            let eligibility =
                library_eligibility(&entry.type_, &entry.genres, anime_claim(&context) || linked);
            let watched_rescan = self.library.modes[index].decisions.iter().any(|d| {
                d.source == context.source
                    && d.fingerprint.starts_with(&format!("{base}:watch:"))
                    && d.fingerprint != fingerprint
            });
            let (proposal, proven) = if eligibility == LibraryEligibility::NonAnime {
                (MappingProposal::default(), false)
            } else {
                let Some(discovered) =
                    self.library_discover(service, &context, fresh || watched_rescan, &mut budget)?
                else {
                    // Deferred sources do not become negative decisions. The
                    // next tick skips completed fingerprints and resumes here.
                    self.library.deferred[service_index(service)] = true;
                    break;
                };
                discovered
            };
            if !library_preserves_bindings(
                &context.source,
                &account,
                &proposal,
                &self.store.state().bindings,
            ) {
                return Err(input_message());
            }
            self.library.modes[index]
                .decisions
                .retain(|d| d.source != context.source);
            self.library.modes[index].decisions.push(LibraryDecision {
                source: context.source.clone(),
                fingerprint,
                title: entry.name,
                eligibility,
                proposal,
                proven_identity: proven,
                dismissed: false,
            });
            self.library_save(index)?;
        }
        // An open bulk review grows only through another visible revision;
        // accepting an older draft can never silently include deferred titles.
        if let Some(review) = self
            .library
            .review
            .as_mut()
            .filter(|r| r.service == service && r.activate_full)
        {
            let decisions = self.library.modes[index]
                .decisions
                .iter()
                .filter(|d| {
                    !d.proposal.releases.is_empty()
                        && !self.library.modes[index].excluded.contains(&d.source)
                })
                .cloned()
                .collect::<Vec<_>>();
            let raw = serde_json::to_vec(&decisions).map_err(|_| storage_message())?;
            let revision = format!("bulk:{generation}:{:016x}", fnv1a(&raw));
            review.decisions = decisions;
            review.revision = revision;
        }
        // New targets always wait for review. The sole automatic case is a
        // dated extension of the SAME approved ongoing release.
        if self.library.modes[index].enabled {
            let mut remaining_extensions = 8;
            let extensions = self.library.modes[index]
                .decisions
                .iter()
                .filter(|d| {
                    d.proven_identity
                        && !d.proposal.releases.is_empty()
                        && !self.library.modes[index].excluded.contains(&d.source)
                })
                .filter_map(|d| {
                    let mut d = d.clone();
                    d.proposal.releases.retain(|r| {
                        !r.check_split
                            && !r.details.media.finished
                            && self.store.state().bindings.iter().any(|b| {
                                b.enabled
                                    && b.source == d.source
                                    && b.target.account == account
                                    && b.target.remote_media_id == r.details.media.id
                            })
                    });
                    d.proposal.releases.truncate(remaining_extensions);
                    remaining_extensions -= d.proposal.releases.len();
                    (!d.proposal.releases.is_empty()).then_some(d)
                })
                .collect::<Vec<_>>();
            if !extensions.is_empty() {
                self.library_commit(
                    service,
                    &extensions,
                    self.library.modes[index].include_history,
                    generation,
                    true,
                    false,
                )?;
            }
        }
        self.library_new_release_prompt(service, index);
        Ok(())
    }
    fn library_discover(
        &mut self,
        service: Service,
        context: &SourceContext,
        fresh_metadata: bool,
        budget: &mut usize,
    ) -> Result<Option<(MappingProposal, bool)>, String> {
        let account = self
            .session(service)?
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        let anchor = self
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
                            .position(|e| e.id == a.episode_id)
                    })
                    .min()
            })
            .map(|b| b.target.remote_media_id);
        let direct = match context.ids.resolve_id(tracker_namespace(service)) {
            providers::IdResolution::Unique(id) => numeric(id),
            _ => None,
        };
        let seed = if let Some(seed) = anchor.or(direct) {
            Some(seed)
        } else {
            match service {
                Service::AniList => {
                    match context.ids.resolve_id(providers::IdNamespace::MalAnime) {
                        providers::IdResolution::Unique(providers::ExternalId::MalAnime(id)) => {
                            if !library_request(budget) {
                                return Ok(None);
                            }
                            let reply = self.session(service)?.client.media_by_mal(id, now_secs());
                            self.service_result(service, reply)?.map(|m| m.id)
                        }
                        _ => None,
                    }
                }
                Service::MyAnimeList => match context
                    .ids
                    .resolve_id(providers::IdNamespace::AnilistAnime)
                {
                    providers::IdResolution::Unique(providers::ExternalId::AnilistAnime(id)) => {
                        if let Some(retry_at) = self
                            .store
                            .state()
                            .outbox
                            .service_retry_at(Service::AniList, now_secs())
                        {
                            return Err(api_message(ApiError::RateLimited { retry_at }));
                        }
                        if !library_request(budget) {
                            return Ok(None);
                        }
                        let reply =
                            Client::public_anilist(self.transport.clone()).media(id, now_secs());
                        self.service_result(Service::AniList, reply)?.mal_id
                    }
                    _ => None,
                },
            }
        };
        let proven_identity = seed.is_some();
        let seed = if let Some(seed) = seed {
            seed
        } else {
            // Conflicting current IDs require the title editor. Search hits
            // and misses share the complete persistent inventory fingerprint;
            // a title result is review-only and cannot enable automatic links.
            if [
                providers::IdNamespace::MalAnime,
                providers::IdNamespace::AnilistAnime,
            ]
            .into_iter()
            .any(|n| {
                matches!(
                    context.ids.resolve_id(n),
                    providers::IdResolution::Conflict(_)
                )
            }) {
                return Ok(Some((MappingProposal::default(), false)));
            }
            let mut titles = vec![context.title.clone()];
            titles.extend(context.aliases.iter().take(2).cloned());
            let mut candidates = Vec::new();
            for title in &titles {
                if !library_request(budget) {
                    return Ok(None);
                }
                let reply = self.session(service)?.client.search(title, now_secs());
                for media in self.service_result(service, reply)?.into_iter().take(20) {
                    if candidates.len() < 60 && !candidates.iter().any(|m: &Media| m.id == media.id)
                    {
                        candidates.push(media);
                    }
                }
            }
            let Some(seed) = library_title_candidate(
                &candidates,
                &titles,
                context.year,
                context.source.media_type == "movie",
            ) else {
                return Ok(Some((MappingProposal::default(), false)));
            };
            seed
        };
        let mut pending = std::collections::VecDeque::from([seed]);
        if let Some(id) = direct.filter(|id| *id != seed) {
            pending.push_back(id);
        }
        let epoch = self.session(service)?.epoch;
        let generation = self.bridge.tracking().library_generation[service_index(service)]
            .load(Ordering::Acquire);
        let mut details = Vec::new();
        while let Some(id) = pending.pop_front() {
            if details.iter().any(|r: &ReleaseDetails| r.media.id == id) {
                continue;
            }
            if details.len() >= 32 {
                break;
            }
            self.library_guard(service, generation, &account, epoch)?;
            if !library_request(budget) {
                return Ok(None);
            }
            let release = self.setup_details(service, id, fresh_metadata)?;
            for edge in &release.relations {
                if matches!(
                    edge.relation,
                    ReleaseRelation::Prequel
                        | ReleaseRelation::Sequel
                        | ReleaseRelation::Alternative
                ) {
                    pending.push_back(edge.id);
                }
            }
            details.push(release);
            if context.source.media_type == "movie" {
                break;
            }
        }
        let mut proposal = if context.source.media_type == "movie" {
            let media = details
                .iter()
                .find(|d| d.media.id == seed)
                .ok_or_else(input_message)?;
            if media.media.format.eq_ignore_ascii_case("movie") {
                MappingProposal {
                    releases: vec![ProposedRelease {
                        details: media.clone(),
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
            } else {
                MappingProposal::default()
            }
        } else {
            propose_coverage(&context.episode_info, &release_chain(seed, &details))
        };
        if !library_preserves_bindings(
            &context.source,
            &account,
            &proposal,
            &self.store.state().bindings,
        ) {
            return Ok(Some((MappingProposal::default(), false)));
        }
        for release in &mut proposal.releases {
            release.assignments.retain(|a| {
                !self
                    .store
                    .state()
                    .bindings
                    .iter()
                    .filter(|b| {
                        b.enabled && b.source == context.source && b.target.account == account
                    })
                    .any(|b| {
                        b.assignments.contains(a)
                            && b.target.remote_media_id == release.details.media.id
                    })
            });
        }
        proposal.releases.retain(|r| !r.assignments.is_empty());
        Ok(Some((proposal, proven_identity)))
    }
    #[allow(clippy::too_many_arguments)]
    fn library_commit(
        &mut self,
        service: Service,
        decisions: &[LibraryDecision],
        include_history: bool,
        generation: u64,
        extensions_only: bool,
        activate_full: bool,
    ) -> Result<u64, String> {
        let session = self.session(service)?;
        let viewer = session.client.viewer().map_err(api_message)?.clone();
        let epoch = session.epoch;
        let account_generation = session.generation;
        let mode_index = self.library_load(&viewer.account)?;
        let mut prepared = Vec::new();
        let mut provenance = Vec::new();
        for decision in decisions {
            self.library_guard(service, generation, &viewer.account, epoch)?;
            let entry = self
                .bridge
                .personal_state()
                .ok_or_else(input_message)?
                .entries
                .iter()
                .find(|e| {
                    e.id == decision.source.source_id && e.type_ == decision.source.media_type
                })
                .cloned()
                .ok_or_else(input_message)?;
            let context = library_context(&entry);
            if self.library_discovery_fingerprint(
                &context,
                &entry,
                &viewer.account,
                self.library.modes[mode_index].event_boundary,
            )? != decision.fingerprint
                || extensions_only && !decision.proven_identity
            {
                return Err(text::tr("Library metadata changed. Review the library again.").into());
            }
            let mut choices = Vec::new();
            let policy = library_policy::read(&viewer.account)?;
            if policy.excluded.contains(&decision.source) {
                return Err(input_message());
            }
            for release in &decision.proposal.releases {
                let fresh = self.setup_details(service, release.details.media.id, true)?;
                if fresh.media.episodes != release.details.media.episodes
                    || fresh.start != release.details.start
                    || fresh.end != release.details.end
                    || fresh.media.format != release.details.media.format
                {
                    return Err(text::tr(
                        "Release metadata changed. Reload suggestions before linking.",
                    )
                    .into());
                }
                let reply = self
                    .session(service)?
                    .client
                    .read(fresh.media.id, now_secs());
                let remote = self.service_result(service, reply)?;
                let old = self.store.state().bindings.iter().find(|b| {
                    b.enabled
                        && b.source == decision.source
                        && b.target.account == viewer.account
                        && b.target.remote_media_id == fresh.media.id
                });
                if extensions_only && old.is_none() {
                    return Err(input_message());
                }
                let mut assignments = release.assignments.clone();
                if let Some(old) = old {
                    for a in &old.assignments {
                        if !assignments.contains(a) {
                            assignments.push(a.clone());
                        }
                    }
                    if !library_extension_preserves(&old.assignments, &assignments) {
                        return Err(input_message());
                    }
                }
                let choice = Choice {
                    service,
                    media: fresh.media.clone(),
                    remote: None,
                    assignments,
                    repair_id: old.map(|b| b.id.clone()),
                    validated: true,
                    proposal_index: None,
                };
                // Publish stable provenance before the mapping can enter the
                // outbox or be shared. A Bulk write never replaces Explicit.
                if old.is_none() {
                    let explicit_source = (!activate_full && !policy.enabled)
                        || self
                            .store
                            .state()
                            .bindings
                            .iter()
                            .filter(|b| {
                                b.enabled
                                    && b.source == decision.source
                                    && b.target.account == viewer.account
                            })
                            .any(|b| {
                                library_policy::origin(
                                    &viewer.account,
                                    &decision.source,
                                    b.target.remote_media_id.get(),
                                )
                                .is_ok_and(|origin| !b.bulk_origin && origin != Some(Origin::Bulk))
                            });
                    library_policy::write_origin(
                        &viewer.account,
                        &decision.source,
                        fresh.media.id.get(),
                        if explicit_source {
                            Origin::Explicit
                        } else {
                            Origin::Bulk
                        },
                    )?;
                }
                let bulk = old.is_some_and(|b| b.bulk_origin)
                    || library_policy::origin(
                        &viewer.account,
                        &decision.source,
                        fresh.media.id.get(),
                    )? == Some(Origin::Bulk);
                provenance.push((decision.source.clone(), fresh.media.id, bulk));
                choices.push((choice, remote, fresh.media));
            }
            prepared.push((decision.source.clone(), choices));
        }
        self.library_guard(service, generation, &viewer.account, epoch)?;
        // Earlier sources may have changed while later HTTP reads were in
        // flight. Recheck every complete inventory before the atomic batch.
        for decision in decisions {
            let entry = self
                .bridge
                .personal_state()
                .ok_or_else(input_message)?
                .entries
                .iter()
                .find(|e| {
                    e.id == decision.source.source_id && e.type_ == decision.source.media_type
                })
                .cloned()
                .ok_or_else(input_message)?;
            if self.library_discovery_fingerprint(
                &library_context(&entry),
                &entry,
                &viewer.account,
                self.library.modes[mode_index].event_boundary,
            )? != decision.fingerprint
            {
                return Err(text::tr("Library metadata changed. Review the library again.").into());
            }
        }
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let mut history = owner.lock().unwrap();
        history.save().map_err(|_| storage_message())?;
        let sequence = history
            .extra_value(&profiles::storage_key(EVENT_COUNTER_KEY))
            .map_err(|_| storage_message())?
            .map(|s| serde_json::from_str::<u64>(&s))
            .transpose()
            .map_err(|_| storage_message())?
            .unwrap_or(0);
        let mut map: HashMap<String, EpisodeProgress> = history
            .extra_value(&profiles::storage_key(EPISODE_PROGRESS_KEY))
            .map_err(|_| storage_message())?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| storage_message())?
            .unwrap_or_default();
        let boundary = if self.library.modes[mode_index].enabled || !activate_full {
            self.library.modes[mode_index].event_boundary
        } else {
            sequence
        };
        // Only semantic local watches after whole-library opt-in may be
        // imported without the explicit existing-history checkbox. Old and
        // paired-device observations stay checkpoint-only.
        if !include_history {
            for progress in map.values_mut() {
                let key = format!(
                    "list_services:watched:v1:{}",
                    serde_json::to_string(&(&progress.series_id, &progress.episode_id))
                        .map_err(|_| storage_message())?
                );
                let event = history
                    .extra_value(&profiles::storage_key(&key))
                    .map_err(|_| storage_message())?
                    .map(|s| serde_json::from_str::<u64>(&s))
                    .transpose()
                    .map_err(|_| storage_message())?
                    .unwrap_or(0);
                if event <= boundary {
                    progress.watched = false;
                }
            }
        }
        let mut state = self.store.state().clone();
        for (source, fresh) in prepared {
            state = prepare_link_state(
                &state,
                &source,
                &viewer,
                account_generation,
                sequence,
                &map,
                fresh,
                true,
            )?;
        }
        for binding in &mut state.bindings {
            if binding.target.account == viewer.account
                && let Some((_, _, bulk)) = provenance.iter().find(|(source, id, _)| {
                    *source == binding.source && *id == binding.target.remote_media_id
                })
            {
                binding.bulk_origin = *bulk;
            }
        }
        self.library_guard(service, generation, &viewer.account, epoch)?;
        self.store.save(state).map_err(|_| storage_message())?;
        Ok(boundary)
    }
    fn library_new_release_prompt(&mut self, service: Service, index: usize) {
        if self.library.prompt.is_some() {
            return;
        }
        let history = read_progress_map();
        for decision in &self.library.modes[index].decisions {
            if decision.dismissed
                || self.library.modes[index]
                    .excluded
                    .contains(&decision.source)
                || !decision.proven_identity
                || decision.proposal.releases.is_empty()
            {
                continue;
            }
            let new_release = decision.proposal.releases.iter().any(|r| {
                !self.store.state().bindings.iter().any(|b| {
                    b.enabled
                        && b.source == decision.source
                        && b.target.account == self.library.modes[index].account
                        && b.target.remote_media_id == r.details.media.id
                }) && r.assignments.iter().any(|a| {
                    history.values().any(|p| {
                        p.series_id == decision.source.source_id
                            && p.episode_id == a.episode_id
                            && p.watched
                            && list_services::local_watched_sequence(&p.series_id, &p.episode_id)
                                .unwrap_or(0)
                                > self.library.modes[index].event_boundary
                    })
                })
            });
            if new_release {
                self.library.prompt = Some((
                    service,
                    decision.source.clone(),
                    library_prompt_revision(decision),
                    decision.title.clone(),
                ));
                break;
            }
        }
    }
    pub(super) fn library_exclude_source(
        &mut self,
        account: &AccountKey,
        source: &SourceRef,
        excluded: bool,
    ) -> Result<(), String> {
        let index = self.library_load(account)?;
        library_policy::write_exclusion(account, source, excluded)?;
        self.library.modes[index].excluded.retain(|s| s != source);
        if excluded {
            self.library.modes[index].excluded.push(source.clone());
        }
        self.library_save(index)?;
        if self
            .library
            .prompt
            .as_ref()
            .is_some_and(|(_, s, _, _)| s == source)
        {
            self.library.prompt = None;
        }
        self.library.review = None;
        self.library_reconcile_policy()?;
        Ok(())
    }
    pub(super) fn library_account_fields(
        &self,
        service: Service,
    ) -> (bool, bool, bool, String, String, String) {
        let account = self
            .sessions
            .iter()
            .find(|s| s.service == service)
            .and_then(|s| s.client.viewer().ok())
            .map(|v| &v.account);
        let mode = account.and_then(|a| self.library.modes.iter().find(|m| m.account == *a));
        let review = self
            .library
            .review
            .as_ref()
            .filter(|r| r.service == service && account == Some(&r.account));
        let ready = mode.map_or(0, |m| {
            m.decisions
                .iter()
                .filter(|d| !d.proposal.releases.is_empty() && !m.excluded.contains(&d.source))
                .count()
        });
        let pending = mode.map_or(0, |m| {
            m.decisions
                .iter()
                .filter(|d| {
                    d.eligibility != LibraryEligibility::NonAnime
                        && d.proposal.releases.is_empty()
                        && !m.excluded.contains(&d.source)
                })
                .count()
        });
        let mut summary = text::tracking_library_summary(ready, pending);
        if self.library.deferred[service_index(service)] {
            summary.push('\n');
            summary.push_str(text::tr("More titles will be reviewed on the next scan."));
        }
        let details = review
            .map(|r| {
                r.decisions
                    .iter()
                    .flat_map(|d| {
                        d.proposal.releases.iter().map(move |p| {
                            text::tracking_library_release(
                                &d.title,
                                &p.details.media.title,
                                p.season,
                                p.first,
                                p.last,
                                p.check_split,
                                !d.proven_identity,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        (
            mode.is_some_and(|m| m.enabled),
            self.library.busy == Some(service),
            review.is_some(),
            summary,
            details,
            review.map(|r| r.revision.clone()).unwrap_or_default(),
        )
    }
    pub(super) fn library_scoped_review(&self, service: Service) -> bool {
        self.library
            .review
            .as_ref()
            .is_some_and(|r| r.service == service && !r.activate_full)
    }
    pub(super) fn library_publish_prompt(&self) {
        let prompt = self.library.prompt.clone();
        let bridge = self.bridge.clone();
        let _ = profiles::invoke_from_event_loop(move || {
            if let Some(app) = bridge.app() {
                if let Some((service, _, revision, title)) = prompt {
                    app.set_tracking_library_prompt_service(service_index(service) as i32);
                    app.set_tracking_library_prompt_revision(revision.into());
                    app.set_tracking_library_prompt_title(title.into());
                    app.set_tracking_library_prompt_open(true);
                } else {
                    app.set_tracking_library_prompt_open(false);
                }
            }
        });
    }
    pub(super) fn library_title_fields(&self) -> (bool, bool, String) {
        let Some(context) = self.context.as_ref() else {
            return (false, false, String::new());
        };
        let account = self
            .sessions
            .iter()
            .find(|s| s.service == self.candidate_service)
            .and_then(|s| s.client.viewer().ok())
            .map(|v| &v.account);
        let mode = account.and_then(|a| self.library.modes.iter().find(|m| m.account == *a));
        (
            mode.is_some_and(|m| m.enabled),
            mode.is_some_and(|m| m.excluded.contains(&context.source)),
            serde_json::to_string(&context.source).unwrap_or_default(),
        )
    }
    pub(super) fn library_manual_source(&mut self, source: &SourceRef) {
        for mode in &mut self.library.modes {
            mode.decisions.retain(|d| d.source != *source);
        }
        self.library.next_scan = 0;
    }
    pub(super) fn library_reset(&mut self) -> Result<(), String> {
        library_policy::clear_all()?;
        let entries = storage::try_scan_prefix(LIBRARY_KEY)
            .map_err(|_| storage_message())?
            .into_iter()
            .map(|(key, _)| (key, None))
            .collect::<Vec<_>>();
        storage::try_write_batch(&entries).map_err(|_| storage_message())?;
        self.library = LibraryRuntime::default();
        Ok(())
    }
    pub(super) fn library_binding_allowed(&self, binding: &Binding) -> Result<bool, String> {
        let policy = library_policy::read(&binding.target.account)?;
        Ok(library_scope_allows(
            policy.enabled,
            policy.excluded.contains(&binding.source),
            binding.bulk_origin
                || library_policy::origin(
                    &binding.target.account,
                    &binding.source,
                    binding.target.remote_media_id.get(),
                )? == Some(Origin::Bulk),
        ))
    }
    fn library_discovery_fingerprint(
        &self,
        context: &SourceContext,
        entry: &LibraryEntry,
        account: &AccountKey,
        boundary: u64,
    ) -> Result<String, String> {
        let base = library_fingerprint(context, entry, account, self.store.state());
        let history = read_progress_map();
        let mut seasons = Vec::new();
        for episode in &context.episode_info {
            if self
                .store
                .state()
                .bindings
                .iter()
                .filter(|b| b.enabled && b.source == context.source && b.target.account == *account)
                .any(|b| b.assignments.iter().any(|a| a.episode_id == episode.id))
            {
                continue;
            }
            let watched = history.values().any(|p| {
                p.series_id == context.source.source_id && p.episode_id == episode.id && p.watched
            });
            if watched
                && library_watch_authorized(
                    false,
                    boundary,
                    list_services::local_watched_sequence(&context.source.source_id, &episode.id)
                        .map_err(|_| storage_message())?,
                )
            {
                seasons.push(
                    episode
                        .season
                        .map(|s| format!("season:{s}"))
                        .unwrap_or_else(|| format!("episode:{}", episode.id)),
                );
            }
        }
        seasons.sort();
        seasons.dedup();
        Ok(format!(
            "{base}:watch:{:016x}",
            fnv1a(&serde_json::to_vec(&seasons).map_err(|_| storage_message())?)
        ))
    }
    pub(super) fn library_target_allowed(&self, target: &TargetKey) -> Result<bool, String> {
        for binding in self
            .store
            .state()
            .bindings
            .iter()
            .filter(|b| b.enabled && b.target == *target)
        {
            if self.library_binding_allowed(binding)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn library_mappings_allowed(
        &self,
        target: &TargetKey,
        mappings: &[MappingStamp],
    ) -> Result<bool, String> {
        for stamp in mappings {
            let Some(binding) = self.store.state().bindings.iter().find(|b| {
                b.enabled
                    && b.id == stamp.binding_id
                    && b.source == stamp.source
                    && b.target == *target
            }) else {
                return Ok(false);
            };
            if !self.library_binding_allowed(binding)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) fn library_reconcile_policy(&mut self) -> Result<(), String> {
        let accounts = self
            .store
            .state()
            .accounts
            .iter()
            .map(|a| a.key.clone())
            .collect::<Vec<_>>();
        for account in accounts {
            self.library_load(&account)?;
        }
        let mut state = self.store.state().clone();
        let blocked = state
            .bindings
            .iter()
            .filter(|b| b.enabled)
            .map(|b| Ok((b.id.clone(), !self.library_binding_allowed(b)?)))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter_map(|(id, blocked)| blocked.then_some(id))
            .collect::<Vec<_>>();
        self.library_prune_projections(&mut state, &blocked)?;
        for target in &state.targets {
            let pending_revoked = state
                .outbox
                .pending()
                .iter()
                .filter(|p| p.target == target.key)
                .map(|p| self.library_mappings_allowed(&target.key, &p.mappings))
                .collect::<Result<Vec<_>, _>>()?
                .contains(&false);
            if !self.library_target_allowed(&target.key)? || pending_revoked {
                state.outbox.cancel_unsent(&target.key);
            }
        }
        if state != *self.store.state() {
            self.store.save(state).map_err(|_| storage_message())?;
        }
        self.library.review = None;
        self.library.prompt = None;
        self.library.next_scan = 0;
        Ok(())
    }
    pub(super) fn library_prune_projections(
        &self,
        state: &mut TrackingState,
        blocked: &[String],
    ) -> Result<(), String> {
        let targets = state
            .targets
            .iter()
            .map(|t| t.key.clone())
            .collect::<Vec<_>>();
        for target in targets {
            let allowed = state
                .bindings
                .iter()
                .filter(|b| b.enabled && b.target == target && !blocked.contains(&b.id))
                .map(|b| b.source.clone())
                .collect::<Vec<_>>();
            state
                .restrict_automatic_sources(&target, &allowed)
                .map_err(|_| input_message())?;
        }
        Ok(())
    }
    pub(super) fn library_explicit_source(
        &mut self,
        source: &SourceRef,
        targets: &[TargetKey],
    ) -> Result<(), String> {
        for target in targets {
            library_policy::write_origin(
                &target.account,
                source,
                target.remote_media_id.get(),
                Origin::Explicit,
            )?;
            library_policy::write_exclusion(&target.account, source, false)?;
        }
        self.library.next_scan = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_context() -> SourceContext {
        SourceContext {
            source: SourceRef {
                provider_id: "nova".into(),
                source_id: "native".into(),
                media_type: "series".into(),
            },
            title: "Anime".into(),
            ids: providers::normalize_external_ids(&HashMap::new(), "mal:20", "series"),
            historical_ids: vec![],
            aliases: vec![],
            year: Some(2026),
            episodes: vec![("native-s1e1".into(), "First".into())],
            episode_info: vec![EpisodeInfo {
                id: "native-s1e1".into(),
                season: Some(1),
                number: Some(1),
                title: "First".into(),
                released: ListDate::default(),
            }],
        }
    }
    fn entry() -> LibraryEntry {
        serde_json::from_value(serde_json::json!({"id":"native","type_":"series","name":"Anime","genres":["Animation"]})).unwrap()
    }
    fn account() -> AccountKey {
        AccountKey {
            service: Service::AniList,
            remote_user_id: 1.try_into().unwrap(),
        }
    }
    #[test]
    fn persistent_discovery_key_changes_with_identity_inventory_and_manual_mapping() {
        let state = TrackingState::default();
        let entry = entry();
        let mut context = make_context();
        let account = account();
        let first = library_fingerprint(&context, &entry, &account, &state);
        assert_eq!(
            first,
            library_fingerprint(&context, &entry, &account, &state)
        );
        context.episode_info.push(EpisodeInfo {
            id: "native-s2e1".into(),
            season: Some(2),
            number: Some(1),
            title: "New season".into(),
            released: ListDate::default(),
        });
        assert_ne!(
            first,
            library_fingerprint(&context, &entry, &account, &state)
        );
        context = make_context();
        context.ids = providers::normalize_external_ids(&HashMap::new(), "mal:21", "series");
        assert_ne!(
            first,
            library_fingerprint(&context, &entry, &account, &state)
        );
        context = make_context();
        let mut mapped = state.clone();
        mapped.bindings.push(Binding {
            id: "approved".into(),
            source: context.source.clone(),
            target: TargetKey {
                account: account.clone(),
                media_kind: MediaKind::Anime,
                remote_media_id: 20.try_into().unwrap(),
            },
            account_generation: 1.try_into().unwrap(),
            mapping_revision: 1.try_into().unwrap(),
            enabled: true,
            bulk_origin: false,
            assignments: vec![Assignment {
                episode_id: "native-s1e1".into(),
                target_episode: 1.try_into().unwrap(),
            }],
        });
        assert_ne!(
            first,
            library_fingerprint(&context, &entry, &account, &mapped)
        );
    }
    #[test]
    fn dismissal_token_includes_candidate_and_episode_inventory() {
        let mut decision = LibraryDecision {
            source: make_context().source,
            fingerprint: "source-inventory".into(),
            title: "Anime".into(),
            eligibility: LibraryEligibility::Anime,
            proposal: MappingProposal::default(),
            proven_identity: true,
            dismissed: false,
        };
        let original = library_prompt_revision(&decision);
        decision
            .proposal
            .unresolved
            .push("new-season-episode".into());
        assert_ne!(original, library_prompt_revision(&decision));
        decision.proposal.unresolved.clear();
        decision.fingerprint = "new-inventory".into();
        assert_ne!(original, library_prompt_revision(&decision));
    }
}

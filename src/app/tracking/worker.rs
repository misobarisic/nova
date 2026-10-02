//! One owner for local state, authenticated sessions and per-target delivery.
//! UI callbacks only enqueue commands; no network call runs on the UI thread.
use super::*;
use nova_tracking::{
    api::{Client, HttpsTransport},
    auth::{AuthReturn, Authorization, ClientRegistration, Tokens},
    *,
};
use std::num::{NonZeroU32, NonZeroU64};
const CLIENTS_KEY: &str = "tracking:clients:v1";
#[derive(Clone, Serialize, Deserialize)]
struct Registrations {
    mal: ClientRegistration,
    anilist: ClientRegistration,
}
impl Default for Registrations {
    fn default() -> Self {
        Self {
            mal: ClientRegistration {
                client_id: "16888e1f2e47945363e81e7a19d16cc8".into(),
                redirect_uri: "http://127.0.0.1:53926/callback".into(),
            },
            anilist: ClientRegistration {
                client_id: "52547".into(),
                redirect_uri: "https://anilist.co/api/v2/oauth/pin".into(),
            },
        }
    }
}
impl Registrations {
    // Older settings may contain an empty card for the service that was never
    // configured. Fill only those cards; keep explicit registrations intact.
    fn fill_defaults(mut self) -> Self {
        let defaults = Self::default();
        if self.mal.client_id.is_empty() {
            self.mal = defaults.mal;
        }
        if self.anilist.client_id.is_empty() {
            self.anilist = defaults.anilist;
        }
        self
    }
    fn get(&self, s: Service) -> &ClientRegistration {
        match s {
            Service::MyAnimeList => &self.mal,
            Service::AniList => &self.anilist,
        }
    }
    fn set(&mut self, s: Service, value: ClientRegistration) {
        match s {
            Service::MyAnimeList => self.mal = value,
            Service::AniList => self.anilist = value,
        }
    }
}
pub(super) enum Command {
    Reset,
    Automatic {
        service: Service,
        enabled: bool,
    },
    Connect {
        service: Service,
        registration: ClientRegistration,
        epoch: u64,
    },
    Complete {
        service: Service,
        value: AuthReturn,
        epoch: u64,
    },
    Cancel {
        service: Service,
        epoch: u64,
    },
    Disconnect {
        service: Service,
        epoch: u64,
    },
    Metadata {
        source: SourceRef,
        episode_ids: Vec<String>,
    },
    Show {
        context: SourceContext,
        generation: u64,
    },
    Close,
    Search {
        service: Service,
        query: String,
        generation: u64,
    },
    Pick {
        index: usize,
        generation: u64,
    },
    Preview {
        first: String,
        last: String,
        start: String,
        generation: u64,
    },
    Assign {
        row: usize,
        value: String,
        generation: u64,
    },
    Confirm {
        generation: u64,
    },
    Edit {
        id: String,
        progress: String,
        status: i32,
        score: String,
        started: Option<String>,
        completed: Option<String>,
    },
    Action {
        id: String,
        action: i32,
    },
}
struct Login {
    service: Service,
    epoch: u64,
    auth: Authorization,
    cancel: Arc<AtomicBool>,
    expires_at: u64,
}
struct Session {
    service: Service,
    epoch: u64,
    generation: NonZeroU64,
    tokens: Tokens,
    client: Client,
    registration: ClientRegistration,
    paused: bool,
    refresh_after: u64,
}
struct Choice {
    service: Service,
    media: Media,
    remote: Option<RemoteEntry>,
    assignments: Vec<Assignment>,
    repair_id: Option<String>,
    validated: bool,
}
struct Coordinator {
    bridge: Bridge,
    store: Store,
    transport: HttpsTransport,
    registrations: Registrations,
    sessions: Vec<Session>,
    logins: Vec<Login>,
    context: Option<SourceContext>,
    generation: u64,
    candidates: Vec<Media>,
    candidate_service: Service,
    choice: Option<Choice>,
    notice: String,
    busy: bool,
    completion: Option<Completion>,
    cache: CatalogCache,
}
enum Completion {
    Progress(DeliveryAttempt, DeliveryOutcome, Option<RemoteEntry>),
    Edit(EditAttempt, Result<RemoteEntry, DeliveryFailure>),
}
pub(super) fn run(bridge: Bridge, rx: Receiver<Command>) {
    let mut loaded = Store::load(KvStorage);
    while loaded.is_err() {
        let weak = bridge.app.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                app.set_tracking_notice(
                    text::tr("Tracking data needs recovery. Playback remains available.").into(),
                );
            }
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Reset) => {
                loaded = reset_store();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            _ => {
                if !bridge.tracking.alive.load(Ordering::Acquire) {
                    return;
                }
            }
        }
    }
    let store = loaded.unwrap();
    let transport = match HttpsTransport::new() {
        Ok(t) => t,
        Err(_) => return,
    };
    let registrations = match read_json_result::<Registrations>(CLIENTS_KEY) {
        Ok(Some(r)) => r.fill_defaults(),
        Ok(None) => Registrations::default(),
        Err(_) => {
            if let Ok(Some(raw)) = storage::try_get_str(CLIENTS_KEY) {
                let _ = storage::try_set_str(
                    &format!("tracking:clients:quarantine:{:016x}", fnv1a(raw.as_bytes())),
                    &raw,
                );
            }
            Registrations::default()
        }
    };
    let mut coordinator = Coordinator {
        bridge,
        store,
        transport,
        registrations,
        sessions: vec![],
        logins: vec![],
        context: None,
        generation: 0,
        candidates: vec![],
        candidate_service: Service::AniList,
        choice: None,
        notice: String::new(),
        busy: false,
        completion: None,
        cache: read_json_result(CATALOG_CACHE_KEY)
            .ok()
            .flatten()
            .unwrap_or_default(),
    };
    coordinator.publish();
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(command) => {
                if let Err(error) = coordinator.command(command) {
                    coordinator.notice = error;
                    coordinator.busy = false;
                }
                coordinator.publish();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !coordinator.bridge.tracking.alive.load(Ordering::Acquire) {
            break;
        }
        coordinator.logins.retain(|login| {
            let live = now_secs() < login.expires_at
                && coordinator.bridge.tracking.login_generation[service_index(login.service)]
                    .load(Ordering::Acquire)
                    == login.epoch;
            if !live {
                login.cancel.store(true, Ordering::Release);
            }
            live
        });
        if let Err(error) = coordinator.consume_events() {
            coordinator.notice = error;
            coordinator.publish();
            continue;
        }
        coordinator.deliver();
        coordinator.publish();
    }
    for login in coordinator.logins {
        login.cancel.store(true, Ordering::Release);
    }
}
impl Coordinator {
    fn current(&self, generation: u64) -> bool {
        self.generation == generation
            && self
                .bridge
                .tracking
                .context_generation
                .load(Ordering::Acquire)
                == generation
    }
    fn session(&self, service: Service) -> Result<&Session, String> {
        self.sessions
            .iter()
            .find(|s| {
                s.service == service
                    && !s.paused
                    && self.bridge.tracking.login_generation[service_index(service)]
                        .load(Ordering::Acquire)
                        == s.epoch
            })
            .ok_or_else(|| text::tr("Connect this service in Settings → Tracking first.").into())
    }
    fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Automatic { service, enabled } => {
                self.consume_events()?;
                let mut state = self.store.state().clone();
                state.automatic_paused.retain(|s| *s != service);
                if !enabled {
                    state.automatic_paused.push(service);
                }
                self.store.save(state).map_err(|_| storage_message())?;
                Ok(())
            }
            Command::Reset => {
                self.cancel(Service::MyAnimeList);
                self.cancel(Service::AniList);
                let replacement = reset_store().map_err(|_| storage_message())?;
                self.sessions.clear();
                self.store = replacement;
                self.choice = None;
                self.candidates.clear();
                self.notice = text::tr(
                    "Tracking reset. The previous local state is retained in a recovery backup.",
                )
                .into();
                Ok(())
            }
            Command::Connect {
                service,
                registration,
                epoch,
            } => {
                let result = self.connect(service, registration, epoch);
                if result.is_err() {
                    self.cancel(service);
                    self.restore_session_epoch(service, epoch);
                }
                result
            }
            Command::Complete {
                service,
                value,
                epoch,
            } => {
                let result = self.complete(service, value, epoch);
                if result.is_err() {
                    self.restore_session_epoch(service, epoch);
                }
                result
            }
            Command::Cancel { service, epoch } => {
                self.cancel(service);
                for session in self.sessions.iter_mut().filter(|s| s.service == service) {
                    session.epoch = epoch;
                }
                self.notice = text::tr("Sign-in canceled.").into();
                Ok(())
            }
            Command::Disconnect { service, epoch } => {
                if self.bridge.tracking.login_generation[service_index(service)]
                    .load(Ordering::Acquire)
                    != epoch
                {
                    return Ok(());
                }
                self.cancel(service);
                self.sessions.retain(|s| s.service != service);
                self.notice =
                    text::tr("Disconnected. Links and queued updates are retained.").into();
                Ok(())
            }
            Command::Metadata {
                source,
                episode_ids,
            } => {
                if episode_ids.is_empty() {
                    return Ok(());
                }
                let mut state = self.store.state().clone();
                let mut targets = vec![];
                for binding in state
                    .bindings
                    .iter_mut()
                    .filter(|b| b.enabled && b.source == source)
                {
                    if binding
                        .assignments
                        .iter()
                        .any(|a| !episode_ids.contains(&a.episode_id))
                    {
                        binding.enabled = false;
                        targets.push(binding.target.clone());
                    }
                }
                for target in &targets {
                    state.outbox.pause_alignment(target);
                }
                if !targets.is_empty() {
                    self.store.save(state).map_err(|_| storage_message())?;
                    self.notice =
                        text::tr("Source episodes changed. Review the paused alignment.").into();
                }
                if let Some(context) = self.context.as_ref().filter(|c| c.source == source)
                    && context
                        .episodes
                        .iter()
                        .map(|(id, _)| id)
                        .collect::<Vec<_>>()
                        != episode_ids.iter().collect::<Vec<_>>()
                {
                    self.choice = None;
                    self.candidates.clear();
                    self.notice =
                        text::tr("Source episodes changed. Reopen Tracking to review alignment.")
                            .into();
                }
                Ok(())
            }
            Command::Show {
                context,
                generation,
            } => {
                self.context = Some(context);
                self.busy = false;
                self.generation = generation;
                self.choice = None;
                self.candidates.clear();
                let preferred = [Service::MyAnimeList, Service::AniList]
                    .into_iter()
                    .find(|service| self.session(*service).is_ok());
                if let Some(service) = preferred {
                    self.suggest(service, generation)
                } else {
                    self.notice =
                        text::tr("Connect a service in Settings → Tracking to see suggestions.")
                            .into();
                    Ok(())
                }
            }
            Command::Close => {
                self.context = None;
                self.busy = false;
                self.choice = None;
                self.candidates.clear();
                Ok(())
            }
            Command::Search {
                service,
                query,
                generation,
            } => {
                if query.trim().is_empty() {
                    self.suggest(service, generation)
                } else {
                    self.search(service, query, generation)
                }
            }
            Command::Pick { index, generation } => self.pick(index, generation),
            Command::Preview {
                first,
                last,
                start,
                generation,
            } => self.preview(first, last, start, generation),
            Command::Confirm { generation } => self.confirm(generation),
            Command::Assign {
                row,
                value,
                generation,
            } => self.assign(row, value, generation),
            Command::Edit {
                id,
                progress,
                status,
                score,
                started,
                completed,
            } => self.edit(id, progress, status, score, started, completed),
            Command::Action { id, action } => self.action(id, action),
        }
    }
    fn restore_session_epoch(&mut self, service: Service, epoch: u64) {
        if self.bridge.tracking.login_generation[service_index(service)].load(Ordering::Acquire)
            == epoch
        {
            for session in self.sessions.iter_mut().filter(|s| s.service == service) {
                session.epoch = epoch;
            }
        }
    }
    fn cancel(&mut self, service: Service) {
        self.logins.retain(|l| {
            if l.service == service {
                l.cancel.store(true, Ordering::Release);
                false
            } else {
                true
            }
        });
    }
    fn connect(
        &mut self,
        service: Service,
        registration: ClientRegistration,
        epoch: u64,
    ) -> Result<(), String> {
        registration.validate(service).map_err(api_message)?;
        let auth = Authorization::begin(service, registration.clone()).map_err(api_message)?;
        self.cancel(service);
        let receiver = if registration.redirect_uri.starts_with("http://127.0.0.1:") {
            Some(
                nova_tracking::callback::CallbackReceiver::bind(service, &registration)
                    .map_err(api_message)?,
            )
        } else {
            None
        };
        let mut registrations = self.registrations.clone();
        registrations.set(service, registration);
        let raw = serde_json::to_string(&registrations).map_err(|_| storage_message())?;
        storage::try_set_str(CLIENTS_KEY, &raw).map_err(|_| storage_message())?;
        self.registrations = registrations;
        let cancel = Arc::new(AtomicBool::new(false));
        let url = auth.url().map_err(api_message)?;
        self.logins.push(Login {
            service,
            epoch,
            auth,
            cancel: cancel.clone(),
            expires_at: now_secs().saturating_add(300),
        });
        if let Some(receiver) = receiver {
            let tx = self.bridge.tracking.tx.clone();
            thread::spawn(move || {
                if let Ok(callback) = receiver.receive(cancel, Duration::from_secs(300))
                    && let Ok(value) = AuthReturn::new(callback.to_string())
                {
                    let _ = tx.send(Command::Complete {
                        service,
                        value,
                        epoch,
                    });
                }
            });
        }
        if crate::player::open_browser(&url).is_err() {
            return Err(text::tr(
                "Could not open the sign-in browser. Check your browser settings and reconnect.",
            )
            .into());
        }
        self.notice = text::tr("Finish sign-in in your browser.").into();
        Ok(())
    }
    fn complete(&mut self, service: Service, value: AuthReturn, epoch: u64) -> Result<(), String> {
        if self.bridge.tracking.login_generation[service_index(service)].load(Ordering::Acquire)
            != epoch
        {
            return Ok(());
        }
        let Some(index) = self
            .logins
            .iter()
            .position(|l| l.service == service && l.epoch == epoch)
        else {
            return Ok(());
        };
        let login = self.logins.remove(index);
        login.cancel.store(true, Ordering::Release);
        if now_secs() >= login.expires_at {
            return Err(api_message(ApiError::Authentication));
        }
        let registration = login.auth.registration.clone();
        let tokens = login
            .auth
            .finish_return(value, &self.transport, now_secs())
            .map_err(api_message)?;
        let mut client = Client::new(service, tokens.access.duplicate(), self.transport.clone());
        let viewer = client.verify(now_secs()).map_err(api_message)?;
        if self.bridge.tracking.login_generation[service_index(service)].load(Ordering::Acquire)
            != epoch
        {
            return Ok(());
        }
        let mut state = self.store.state().clone();
        let generation =
            if let Some(account) = state.accounts.iter_mut().find(|a| a.key == viewer.account) {
                account.display_name = viewer.name.clone();
                account.generation
            } else {
                state.accounts.push(Account {
                    key: viewer.account.clone(),
                    generation: NonZeroU64::MIN,
                    display_name: viewer.name.clone(),
                });
                NonZeroU64::MIN
            };
        state.active_accounts.retain(|key| key.service != service);
        state.active_accounts.push(viewer.account.clone());
        self.store.save(state).map_err(|_| storage_message())?;
        self.store
            .resume_account(&viewer.account, generation)
            .map_err(|_| storage_message())?;
        self.sessions.retain(|s| s.service != service);
        self.choice = None;
        self.candidates.clear();
        self.sessions.push(Session {
            service,
            epoch,
            generation,
            tokens,
            client,
            registration,
            paused: false,
            refresh_after: 0,
        });
        self.notice =
            text::tr("Connected for this session. Retained updates for this account can resume.")
                .into();
        Ok(())
    }
    fn suggest(&mut self, service: Service, generation: u64) -> Result<(), String> {
        self.search(service, String::new(), generation)?;
        if !self.current(generation) {
            return Ok(());
        }
        let account = self
            .session(service)?
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        if let Some(context) = self.context.as_ref() {
            filter_linked_candidates(
                self.store.state(),
                &context.source,
                &account,
                &mut self.candidates,
            );
        }
        self.notice = text::tr(if self.candidates.is_empty() {
            "No new suggestions. Search a title or enter a tracker ID to link another release."
        } else {
            "Suggested releases. Check the match and confirm episode alignment before linking."
        })
        .into();
        Ok(())
    }
    fn search(&mut self, service: Service, query: String, generation: u64) -> Result<(), String> {
        if self
            .bridge
            .tracking
            .context_generation
            .load(Ordering::Acquire)
            != generation
        {
            return Ok(());
        }
        self.generation = generation;
        self.candidate_service = service;
        self.choice = None;
        self.candidates.clear();
        self.busy = true;
        self.publish();
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| text::tr("Open a title first.").to_string())?;
        let namespace = match service {
            Service::MyAnimeList => providers::IdNamespace::MalAnime,
            Service::AniList => providers::IdNamespace::AnilistAnime,
        };
        let input = query.trim();
        self.session(service)?;
        let cache_key = if input.is_empty() {
            format!(
                "source:{}",
                serde_json::to_string(&(
                    &context.source,
                    &context.title,
                    &context.ids,
                    &context.aliases,
                    context.year,
                    context.episodes.len()
                ))
                .unwrap_or_default()
            )
        } else {
            format!("search:{input}")
        };
        if let Some(cached) = self.cache.get(service, &cache_key, now_secs()) {
            self.candidate_service = service;
            self.candidates = cached;
            self.busy = false;
            self.notice = text::tr("Select a release, then confirm its episode alignment.").into();
            return Ok(());
        }
        let explicit = if input.bytes().all(|b| b.is_ascii_digit()) && !input.is_empty() {
            input
                .parse::<u32>()
                .ok()
                .filter(|id| *id <= i32::MAX as u32)
                .and_then(NonZeroU32::new)
        } else {
            providers::ExternalId::parse(input).and_then(|id| {
                if id.namespace() == namespace {
                    numeric(id)
                } else {
                    None
                }
            })
        };
        let supplied = if input.is_empty() {
            match context.ids.resolve_id(namespace) {
                providers::IdResolution::Unique(id) => numeric(id),
                providers::IdResolution::Conflict(_) => {
                    return Err(text::tr(
                        "Conflicting source IDs. Enter a tracker ID or search manually.",
                    )
                    .into());
                }
                providers::IdResolution::Missing => None,
            }
        } else {
            None
        };
        let cross = if input.is_empty() && explicit.or(supplied).is_none() {
            match service {
                Service::AniList => {
                    match context.ids.resolve_id(providers::IdNamespace::MalAnime) {
                        providers::IdResolution::Unique(providers::ExternalId::MalAnime(id)) => {
                            self.session(service)?
                                .client
                                .media_by_mal(id, now_secs())
                                .map_err(api_message)?
                        }
                        _ => None,
                    }
                }
                Service::MyAnimeList => match context
                    .ids
                    .resolve_id(providers::IdNamespace::AnilistAnime)
                {
                    providers::IdResolution::Unique(providers::ExternalId::AnilistAnime(id)) => {
                        let media = Client::public_anilist(self.transport.clone())
                            .media(id, now_secs())
                            .map_err(api_message)?;
                        media
                            .mal_id
                            .map(|id| {
                                self.session(service)?
                                    .client
                                    .media(id, now_secs())
                                    .map_err(api_message)
                            })
                            .transpose()?
                    }
                    _ => None,
                },
            }
        } else {
            None
        };
        let mut results = if let Some(media) = cross {
            vec![media]
        } else if let Some(id) = explicit.or(supplied) {
            vec![
                self.session(service)?
                    .client
                    .media(id, now_secs())
                    .map_err(api_message)?,
            ]
        } else {
            let mut titles = vec![if input.is_empty() {
                context.title.clone()
            } else {
                input.to_string()
            }];
            if input.is_empty() {
                titles.extend(context.aliases.iter().take(2).cloned());
            }
            let mut found = Vec::new();
            for title in titles {
                if !self.current(generation) {
                    return Ok(());
                }
                let results = self
                    .session(service)?
                    .client
                    .search(&title, now_secs())
                    .map_err(api_message)?;
                for media in results {
                    if !found.iter().any(|m: &Media| m.id == media.id) && found.len() < 20 {
                        found.push(media);
                    }
                }
            }
            found
        };
        if !self.current(generation) {
            return Ok(());
        }
        let mut titles = vec![context.title.clone()];
        titles.extend(context.aliases.iter().cloned());
        rank_candidates(
            &mut results,
            &titles,
            context.year,
            context.source.media_type == "movie",
            context.episodes.len(),
        );
        self.cache
            .insert(service, cache_key, results.clone(), now_secs());
        if let Ok(raw) = serde_json::to_string(&self.cache) {
            let _ = storage::try_set_str(CATALOG_CACHE_KEY, &raw);
        }
        self.candidate_service = service;
        self.candidates = results;
        self.busy = false;
        self.notice = text::tr("Select a release, then confirm its episode alignment.").into();
        Ok(())
    }
    fn pick(&mut self, index: usize, generation: u64) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let media = self
            .candidates
            .get(index)
            .cloned()
            .ok_or_else(input_message)?;
        let remote = self
            .session(self.candidate_service)?
            .client
            .read(media.id, now_secs())
            .map_err(api_message)?;
        if !self.current(generation) {
            return Ok(());
        }
        let account = self
            .session(self.candidate_service)?
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        let repair_id = self.context.as_ref().and_then(|context| {
            self.store
                .state()
                .bindings
                .iter()
                .find(|b| {
                    b.source == context.source
                        && b.target.account == account
                        && b.target.remote_media_id == media.id
                })
                .map(|b| b.id.clone())
        });
        self.choice = Some(Choice {
            service: self.candidate_service,
            media,
            remote,
            assignments: vec![],
            repair_id,
            validated: false,
        });
        self.notice =
            text::tr("Review the source rows and preview the alignment before confirming.").into();
        Ok(())
    }
    fn preview(
        &mut self,
        first: String,
        last: String,
        start: String,
        generation: u64,
    ) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let context = self.context.as_ref().ok_or_else(input_message)?;
        let service = self.choice.as_ref().ok_or_else(input_message)?.service;
        let session = self.session(service)?;
        let account = session
            .client
            .viewer()
            .map_err(api_message)?
            .account
            .clone();
        let account_generation = session.generation;
        let choice = self.choice.as_mut().ok_or_else(input_message)?;
        choice.validated = false;
        choice.assignments.clear();
        let first = first.trim().parse::<usize>().map_err(|_| input_message())?;
        let last = last.trim().parse::<usize>().map_err(|_| input_message())?;
        let start = start.trim().parse::<u32>().map_err(|_| input_message())?;
        if first == 0 || first > last || last > context.episodes.len() || start == 0 {
            return Err(input_message());
        }
        for (index, (id, _)) in context.episodes[first - 1..last].iter().enumerate() {
            let ordinal = start
                .checked_add(u32::try_from(index).map_err(|_| input_message())?)
                .and_then(NonZeroU32::new)
                .ok_or_else(input_message)?;
            if choice.media.episodes.is_some_and(|total| ordinal > total) {
                return Err(text::tr("Alignment exceeds this release’s episode total.").into());
            }
            choice.assignments.push(Assignment {
                episode_id: id.clone(),
                target_episode: ordinal,
            });
        }
        let mut state = self.store.state().clone();
        let key = TargetKey {
            account,
            media_kind: MediaKind::Anime,
            remote_media_id: choice.media.id,
        };
        if !state.targets.iter().any(|t| t.key == key) {
            state.targets.push(Target {
                key: key.clone(),
                remote_entry_id: choice.remote.as_ref().and_then(|r| r.entry_id),
                final_episode_total: choice.media.episodes,
                release_finished: choice.media.finished,
            });
        }
        if let Some(id) = choice.repair_id.as_ref() {
            state.bindings.retain(|b| b.id != *id);
        }
        state.bindings.push(Binding {
            id: "alignment-preview".into(),
            source: context.source.clone(),
            target: key,
            account_generation,
            mapping_revision: NonZeroU64::MIN,
            enabled: true,
            assignments: choice.assignments.clone(),
        });
        if let Err(_error) = validate_bindings(&state.bindings, &state.targets) {
            choice.assignments.clear();
            return Err(text::tr(
                "This alignment overlaps another active release. Edit that link first.",
            )
            .into());
        }
        choice.validated = true;
        self.notice =
            text::tr("Alignment preview ready. Unmapped source rows will not update this release.")
                .into();
        Ok(())
    }
    fn assign(&mut self, row: usize, value: String, generation: u64) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let context = self.context.as_ref().ok_or_else(input_message)?;
        let choice = self.choice.as_mut().ok_or_else(input_message)?;
        choice.validated = false;
        let (id, _) = context.episodes.get(row).ok_or_else(input_message)?;
        choice.assignments.retain(|a| a.episode_id != *id);
        if !value.trim().is_empty() {
            let ordinal = value
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|v| *v <= i32::MAX as u32)
                .and_then(NonZeroU32::new)
                .ok_or_else(input_message)?;
            if choice.media.episodes.is_some_and(|total| ordinal > total) {
                return Err(input_message());
            }
            choice.assignments.push(Assignment {
                episode_id: id.clone(),
                target_episode: ordinal,
            });
        }
        choice.assignments.sort_by_key(|a| {
            context
                .episodes
                .iter()
                .position(|(id, _)| id == &a.episode_id)
                .unwrap_or(usize::MAX)
        });
        let session = self
            .sessions
            .iter()
            .find(|s| s.service == choice.service && !s.paused)
            .ok_or_else(input_message)?;
        let key = TargetKey {
            account: session
                .client
                .viewer()
                .map_err(api_message)?
                .account
                .clone(),
            media_kind: MediaKind::Anime,
            remote_media_id: choice.media.id,
        };
        let mut state = self.store.state().clone();
        if !state.targets.iter().any(|t| t.key == key) {
            state.targets.push(Target {
                key: key.clone(),
                remote_entry_id: choice.remote.as_ref().and_then(|r| r.entry_id),
                final_episode_total: choice.media.episodes,
                release_finished: choice.media.finished,
            });
        }
        if let Some(id) = choice.repair_id.as_ref() {
            state.bindings.retain(|b| b.id != *id);
        }
        state.bindings.push(Binding {
            id: "alignment-preview".into(),
            source: context.source.clone(),
            target: key,
            account_generation: session.generation,
            mapping_revision: NonZeroU64::MIN,
            enabled: true,
            assignments: choice.assignments.clone(),
        });
        if validate_bindings(&state.bindings, &state.targets).is_err() {
            return Err(text::tr(
                "This alignment overlaps another active release. Edit that link first.",
            )
            .into());
        }
        choice.validated = true;
        Ok(())
    }
    fn confirm(&mut self, generation: u64) -> Result<(), String> {
        if !self.current(generation) {
            return Ok(());
        }
        let choice = self.choice.as_ref().ok_or_else(input_message)?;
        if !choice.validated || choice.assignments.is_empty() {
            return Err(input_message());
        }
        let context = self.context.as_ref().ok_or_else(input_message)?;
        let source_still_current = {
            let shared = self.bridge.shared.lock().unwrap();
            shared.modal_item.as_ref().is_some_and(|m| {
                m.id == context.source.source_id
                    && m.type_ == context.source.media_type
                    && (m.type_ == "movie"
                        || m.videos.iter().map(|v| &v.id).collect::<Vec<_>>()
                            == context
                                .episodes
                                .iter()
                                .map(|(id, _)| id)
                                .collect::<Vec<_>>())
            })
        };
        if !source_still_current {
            return Err(
                text::tr("Source episodes changed. Reopen Tracking to review alignment.").into(),
            );
        }
        // Fetch again: search/preview time is not an authoritative baseline.
        let session = self.session(choice.service)?;
        let viewer = session.client.viewer().map_err(api_message)?.clone();
        let account_generation = session.generation;
        let remote = session
            .client
            .read(choice.media.id, now_secs())
            .map_err(api_message)?;
        let fresh_media = session
            .client
            .media(choice.media.id, now_secs())
            .map_err(api_message)?;
        if fresh_media
            .episodes
            .is_some_and(|total| choice.assignments.iter().any(|a| a.target_episode > total))
        {
            return Err(text::tr("Alignment exceeds this release’s episode total.").into());
        }
        if !self.current(generation) {
            return Ok(());
        }
        let key = TargetKey {
            account: viewer.account.clone(),
            media_kind: MediaKind::Anime,
            remote_media_id: choice.media.id,
        };
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let mut history = owner.lock().unwrap();
        history.save().map_err(|_| storage_message())?;
        let sequence = history
            .extra_value(EVENT_COUNTER_KEY)
            .map_err(|_| storage_message())?
            .map(|raw| serde_json::from_str::<u64>(&raw))
            .transpose()
            .map_err(|_| storage_message())?
            .unwrap_or(0);
        let map: HashMap<String, EpisodeProgress> = history
            .extra_value(EPISODE_PROGRESS_KEY)
            .map_err(|_| storage_message())?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|_| storage_message())?
            .unwrap_or_default();
        let mut state = self.store.state().clone();
        if let Some(target) = state.targets.iter_mut().find(|t| t.key == key) {
            target.remote_entry_id = remote.as_ref().and_then(|r| r.entry_id);
            target.final_episode_total = fresh_media.episodes;
            target.release_finished = fresh_media.finished;
        } else {
            state.targets.push(Target {
                key: key.clone(),
                remote_entry_id: remote.as_ref().and_then(|r| r.entry_id),
                final_episode_total: fresh_media.episodes,
                release_finished: fresh_media.finished,
            });
        }
        let binding_id = if let Some(id) = choice.repair_id.clone() {
            id
        } else {
            let mut number = state.bindings.len() + 1;
            while state
                .bindings
                .iter()
                .any(|b| b.id == format!("binding-{number}"))
            {
                number += 1;
            }
            format!("binding-{number}")
        };
        let revision = if let Some(binding) = state.bindings.iter().find(|b| b.id == binding_id) {
            binding
                .mapping_revision
                .get()
                .checked_add(1)
                .and_then(NonZeroU64::new)
                .ok_or_else(input_message)?
        } else {
            NonZeroU64::MIN
        };
        state.bindings.retain(|b| b.id != binding_id);
        let binding = Binding {
            id: binding_id.clone(),
            source: context.source.clone(),
            target: key.clone(),
            account_generation,
            mapping_revision: revision,
            enabled: true,
            assignments: choice.assignments.clone(),
        };
        state.bindings.push(binding.clone());
        state
            .link_checkpoints
            .retain(|c| c.binding_id != binding_id);
        state.link_checkpoints.push(LinkCheckpoint {
            binding_id: binding_id.clone(),
            sequence,
        });
        let observations = checkpoint(&state, &key, &map);
        let baseline = remote.as_ref().map(|r| r.progress).unwrap_or(0);
        if let Some(projection) = state.projections.iter_mut().find(|p| p.target == key) {
            if choice.repair_id.is_some() {
                let old = projection.remote_baseline();
                projection
                    .repair(old.max(baseline), observations)
                    .map_err(|_| input_message())?;
                state.outbox.cancel_unsent(&key);
            } else {
                projection.refresh_remote(account_generation, projection.revision, baseline);
                for observation in observations
                    .iter()
                    .filter(|o| o.episode.source == binding.source)
                {
                    projection
                        .checkpoint(&binding, &observation.episode, observation.watched)
                        .map_err(|_| input_message())?;
                }
            }
        } else {
            state.projections.push(Projection::new(
                key.clone(),
                account_generation,
                baseline,
                observations,
            ));
        }
        if let Some(snapshot) = state.snapshots.iter_mut().find(|s| s.target == key) {
            snapshot.media = fresh_media.clone();
            snapshot.remote = remote;
        } else {
            state.snapshots.push(TargetSnapshot {
                target: key,
                media: fresh_media.clone(),
                remote,
                score_format: viewer.score_format,
                status_pinned: false,
                dates_pinned: false,
            });
        }
        self.store.save(state).map_err(|_| storage_message())?;
        self.choice = None;
        self.candidates.clear();
        self.notice = text::tr(
            "Linked. New watched events will update this release; saved history was not uploaded.",
        )
        .into();
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn edit(
        &mut self,
        id: String,
        progress: String,
        status: i32,
        score: String,
        started: Option<String>,
        completed: Option<String>,
    ) -> Result<(), String> {
        let binding = self
            .store
            .state()
            .bindings
            .iter()
            .find(|b| b.id == id)
            .cloned()
            .ok_or_else(input_message)?;
        let mut state = self.store.state().clone();
        let snapshot = state
            .snapshots
            .iter()
            .find(|s| s.target == binding.target)
            .ok_or_else(input_message)?;
        let mut desired = snapshot.remote.clone();
        apply_desired(&state, &binding.target, &mut desired);
        let progress = progress
            .trim()
            .parse::<u32>()
            .map_err(|_| input_message())?;
        let status = parse_status(status).ok_or_else(input_message)?;
        let score = score
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 100.0)
            .ok_or_else(input_message)?;
        let tenths = (score * 10.0).round() as u32;
        if (f64::from(tenths) - score * 10.0).abs() > 0.001 {
            return Err(input_message());
        }
        let patch = EntryPatch {
            progress: None,
            status: if desired.as_ref().map(|r| r.status) != Some(status) {
                Some(status)
            } else {
                None
            },
            score_tenths: if desired.as_ref().map(|r| r.score_tenths) != Some(tenths) {
                Some(tenths)
            } else {
                None
            },
            started: started.map(|v| parse_date(&v)).transpose()?,
            completed: completed.map(|v| parse_date(&v)).transpose()?,
        };
        if !patch.is_empty() {
            patch
                .validate(binding.target.account.service, snapshot.score_format)
                .map_err(api_message)?;
        }
        let current_progress = display_progress(&state, &binding.target);
        let owner = nova_sync::local_store().map_err(|_| storage_message())?;
        let mut history = owner.lock().unwrap();
        history.save().map_err(|_| storage_message())?;
        if progress != current_progress {
            let map: HashMap<String, EpisodeProgress> = history
                .extra_value(EPISODE_PROGRESS_KEY)
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or_default();
            let observations = checkpoint(&state, &binding.target, &map);
            let sequence = history
                .extra_value(EVENT_COUNTER_KEY)
                .map_err(|_| storage_message())?
                .map(|raw| serde_json::from_str::<u64>(&raw))
                .transpose()
                .map_err(|_| storage_message())?
                .unwrap_or(0);
            // Store API applies replacement and edit together in one commit.
            self.store
                .mutate(|state| {
                    state.replace_progress(&binding.target, progress, observations)?;
                    let ids: Vec<_> = state
                        .bindings
                        .iter()
                        .filter(|b| b.target == binding.target)
                        .map(|b| b.id.clone())
                        .collect();
                    for cursor in &mut state.link_checkpoints {
                        if ids.contains(&cursor.binding_id) {
                            cursor.sequence = sequence;
                        }
                    }
                    if !patch.is_empty() {
                        state.enqueue_edit(&binding.target, patch)?;
                    }
                    Ok(())
                })
                .map_err(|_| input_message())?;
        } else if !patch.is_empty() {
            state
                .enqueue_edit(&binding.target, patch)
                .map_err(|_| input_message())?;
            self.store.save(state).map_err(|_| storage_message())?;
        }
        self.notice = text::tr("Tracker edits queued. Nova watched history is unchanged.").into();
        Ok(())
    }
    fn action(&mut self, id: String, action: i32) -> Result<(), String> {
        let binding = self
            .store
            .state()
            .bindings
            .iter()
            .find(|b| b.id == id)
            .cloned()
            .ok_or_else(input_message)?;
        match action {
            0 => {
                let session = self.session(binding.target.account.service)?;
                if session.client.viewer().map_err(api_message)?.account != binding.target.account {
                    return Err(api_message(ApiError::WrongAccount));
                }
                let score_format = session.client.viewer().map_err(api_message)?.score_format;
                let remote = session
                    .client
                    .read(binding.target.remote_media_id, now_secs())
                    .map_err(api_message)?;
                let mut state = self.store.state().clone();
                if let Some(snapshot) = state
                    .snapshots
                    .iter_mut()
                    .find(|s| s.target == binding.target)
                {
                    snapshot.remote = remote.clone();
                    snapshot.score_format = score_format;
                }
                if let Some(p) = state
                    .projections
                    .iter_mut()
                    .find(|p| p.target == binding.target)
                {
                    p.refresh_remote(
                        binding.account_generation,
                        p.revision,
                        remote.as_ref().map(|r| r.progress).unwrap_or(0),
                    );
                }
                state.retry_edits(&binding.target, now_secs());
                self.store.save(state).map_err(|_| storage_message())?;
                self.store
                    .retry_target(&binding.target, now_secs())
                    .map_err(|_| storage_message())?;
                self.notice =
                    text::tr("Tracker entry refreshed. Retry respects service cooldowns.").into();
            }
            1 => {
                if self
                    .session(binding.target.account.service)?
                    .client
                    .viewer()
                    .map_err(api_message)?
                    .account
                    != binding.target.account
                {
                    return Err(api_message(ApiError::WrongAccount));
                }
                let snapshot = self
                    .store
                    .state()
                    .snapshots
                    .iter()
                    .find(|s| s.target == binding.target)
                    .ok_or_else(input_message)?;
                let remote = self
                    .session(binding.target.account.service)?
                    .client
                    .read(binding.target.remote_media_id, now_secs())
                    .map_err(api_message)?;
                self.choice = Some(Choice {
                    service: binding.target.account.service,
                    media: snapshot.media.clone(),
                    remote,
                    assignments: vec![],
                    repair_id: Some(binding.id),
                    validated: false,
                });
                self.notice=text::tr("Preview and confirm replacement coverage. Previous unsent updates will be discarded.").into();
            }
            2 => {
                let owner = nova_sync::local_store().map_err(|_| storage_message())?;
                let mut history = owner.lock().unwrap();
                history.save().map_err(|_| storage_message())?;
                let map: HashMap<String, EpisodeProgress> = history
                    .extra_value(EPISODE_PROGRESS_KEY)
                    .map_err(|_| storage_message())?
                    .map(|raw| serde_json::from_str(&raw))
                    .transpose()
                    .map_err(|_| storage_message())?
                    .unwrap_or_default();
                let sequence: u64 = history
                    .extra_value(EVENT_COUNTER_KEY)
                    .map_err(|_| storage_message())?
                    .map(|raw| serde_json::from_str(&raw))
                    .transpose()
                    .map_err(|_| storage_message())?
                    .unwrap_or(0);
                let mut state = self.store.state().clone();
                let progress = history_progress(&state, &binding.target, &map)
                    .max(display_progress(&state, &binding.target));
                let observations = checkpoint(&state, &binding.target, &map);
                state
                    .replace_progress(&binding.target, progress, observations)
                    .map_err(|_| input_message())?;
                let ids: Vec<_> = state
                    .bindings
                    .iter()
                    .filter(|b| b.target == binding.target)
                    .map(|b| b.id.clone())
                    .collect();
                for cursor in &mut state.link_checkpoints {
                    if ids.contains(&cursor.binding_id) {
                        cursor.sequence = sequence;
                    }
                }
                self.store.save(state).map_err(|_| storage_message())?;
                self.notice = text::tr("Nova history explicitly queued for this release.").into();
            }
            3 => {
                let mut state = self.store.state().clone();
                state.bindings.retain(|b| b.id != id);
                state.link_checkpoints.retain(|c| c.binding_id != id);
                if !state
                    .bindings
                    .iter()
                    .any(|b| b.enabled && b.target == binding.target)
                {
                    state.outbox.cancel_unsent(&binding.target);
                }
                self.store.save(state).map_err(|_| storage_message())?;
                self.notice =
                    text::tr("Unlinked. Local history and remote tracker values are unchanged.")
                        .into();
            }
            4 => {
                let url = match binding.target.account.service {
                    Service::MyAnimeList => format!(
                        "https://myanimelist.net/anime/{}",
                        binding.target.remote_media_id
                    ),
                    Service::AniList => format!(
                        "https://anilist.co/anime/{}",
                        binding.target.remote_media_id
                    ),
                };
                crate::player::open_browser(&url).map_err(|_| api_message(ApiError::Offline))?;
            }
            5 => {
                let mut state = self.store.state().clone();
                let snapshot = state
                    .snapshots
                    .iter_mut()
                    .find(|s| s.target == binding.target)
                    .ok_or_else(input_message)?;
                snapshot.status_pinned = false;
                snapshot.dates_pinned = false;
                self.store.save(state).map_err(|_| storage_message())?;
            }
            _ => return Err(input_message()),
        }
        Ok(())
    }
    fn consume_events(&mut self) -> Result<(), String> {
        if storage::try_get_str(JOURNAL_ERROR_KEY)
            .map_err(|_| storage_message())?
            .is_some()
        {
            return Err(text::tr("Tracking event data needs recovery.").into());
        }
        let rows = storage::try_scan_prefix(EVENT_PREFIX).map_err(|_| storage_message())?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut state = self.store.state().clone();
        let mut consumed = vec![];
        for (key, raw) in rows.into_iter().take(128) {
            let event: WatchEvent = serde_json::from_str(&raw)
                .map_err(|_| text::tr("Tracking event data needs recovery.").to_string())?;
            if event.sequence == 0 || key != format!("{EVENT_PREFIX}{:020}", event.sequence) {
                return Err(text::tr("Tracking event data needs recovery.").into());
            }
            if state.consume_event(&event).is_err() {
                self.notice =
                    text::tr("Episode alignment needs review. Queued work is paused.").into();
                return Err(self.notice.clone());
            }
            consumed.push(key);
        }
        self.store
            .save_with_events(state, &consumed)
            .map_err(|_| storage_message())
    }
    fn deliver(&mut self) {
        if self.completion.is_some() && !self.commit_completion() {
            return;
        }
        let now = now_secs();
        // Only one request is active in this owner; a service outage does not
        // affect the other service's queued state or credentials.
        for index in 0..self.sessions.len() {
            if self.sessions[index].paused
                || now < self.sessions[index].refresh_after
                || self
                    .logins
                    .iter()
                    .any(|l| l.service == self.sessions[index].service)
            {
                continue;
            }
            let service = self.sessions[index].service;
            if self.bridge.tracking.login_generation[service_index(service)].load(Ordering::Acquire)
                != self.sessions[index].epoch
            {
                continue;
            }
            if self.sessions[index]
                .tokens
                .expires_at
                .is_some_and(|expiry| expiry <= now.saturating_add(60))
            {
                if service == Service::MyAnimeList {
                    let refreshed = self.sessions[index].tokens.refresh(
                        &self.sessions[index].registration,
                        &self.transport,
                        now,
                    );
                    match refreshed {
                        Ok(tokens) => {
                            let mut client = Client::new(
                                service,
                                tokens.access.duplicate(),
                                self.transport.clone(),
                            );
                            match client.verify(now) {
                                Ok(viewer)
                                    if self.sessions[index]
                                        .client
                                        .viewer()
                                        .is_ok_and(|old| old.account == viewer.account) =>
                                {
                                    self.sessions[index].tokens = tokens;
                                    self.sessions[index].client = client;
                                }
                                _ => {
                                    self.sessions[index].paused = true;
                                    continue;
                                }
                            }
                        }
                        Err(ApiError::Offline) => {
                            self.sessions[index].refresh_after = now.saturating_add(60);
                            continue;
                        }
                        Err(ApiError::RateLimited { retry_at }) => {
                            self.sessions[index].refresh_after = retry_at;
                            let _ = self.store.mutate(|state| {
                                state.outbox.defer_service(service, retry_at);
                                Ok(())
                            });
                            continue;
                        }
                        Err(_) => {
                            self.sessions[index].paused = true;
                            continue;
                        }
                    }
                } else {
                    self.sessions[index].paused = true;
                    continue;
                }
            }
            let account = match self.sessions[index].client.viewer() {
                Ok(v) => v.account.clone(),
                Err(_) => continue,
            };
            let targets: Vec<_> = self
                .store
                .state()
                .targets
                .iter()
                .filter(|t| {
                    t.key.account == account
                        && self
                            .store
                            .state()
                            .bindings
                            .iter()
                            .any(|b| b.enabled && b.target == t.key)
                })
                .map(|t| t.key.clone())
                .collect();
            for target in targets {
                if let Ok(Some(attempt)) = self.store.mutate(|state| state.begin_edit(&target, now))
                {
                    let result = self.sessions[index]
                        .client
                        .read(target.remote_media_id, now)
                        .and_then(|remote| {
                            if self.bridge.tracking.login_generation[service_index(service)]
                                .load(Ordering::Acquire)
                                != self.sessions[index].epoch
                            {
                                return Err(ApiError::Authentication);
                            }
                            if attempt.edit().patch.score_tenths.is_some()
                                && attempt.edit().score_format
                                    != Some(self.sessions[index].client.viewer()?.score_format)
                            {
                                return Err(ApiError::UnsupportedField);
                            }
                            self.sessions[index].client.update(
                                target.remote_media_id,
                                remote.as_ref(),
                                &attempt.edit().patch,
                                now,
                            )
                        })
                        .map_err(delivery_failure);
                    self.completion = Some(Completion::Edit(attempt, result));
                    if !self.commit_completion() {
                        return;
                    }
                }
                let attempt = match self.store.begin_delivery(&target, now) {
                    Ok(Some(a)) => a,
                    Ok(None) => continue,
                    Err(_) => {
                        self.notice = storage_message();
                        return;
                    }
                };
                let mut alignment_changed = false;
                let result = self.sessions[index]
                    .client
                    .read(target.remote_media_id, now)
                    .and_then(|remote| {
                        if self.bridge.tracking.login_generation[service_index(service)]
                            .load(Ordering::Acquire)
                            != self.sessions[index].epoch
                        {
                            return Err(ApiError::Authentication);
                        }
                        let media = self.sessions[index]
                            .client
                            .media(target.remote_media_id, now)?;
                        if media.episodes.is_some_and(|total| {
                            self.store
                                .state()
                                .bindings
                                .iter()
                                .filter(|b| b.enabled && b.target == target)
                                .any(|b| b.assignments.iter().any(|a| a.target_episode > total))
                        }) {
                            alignment_changed = true;
                            return Err(ApiError::InvalidInput);
                        }
                        let progress = attempt
                            .progress_for_remote(remote.as_ref().map(|r| r.progress).unwrap_or(0));
                        let patch = if attempt.patch().intent == ProgressIntent::ExplicitReplacement
                        {
                            EntryPatch {
                                progress: Some(progress),
                                ..Default::default()
                            }
                        } else {
                            let state = self.store.state();
                            let mut target = state
                                .targets
                                .iter()
                                .find(|t| t.key == target)
                                .cloned()
                                .ok_or(ApiError::InvalidInput)?;
                            target.final_episode_total = media.episodes;
                            target.release_finished = media.finished;
                            let snapshot = state
                                .snapshots
                                .iter()
                                .find(|s| s.target == target.key)
                                .ok_or(ApiError::InvalidInput)?;
                            let mut patch = EntryPatch::automatic(
                                &target,
                                snapshot,
                                remote.as_ref(),
                                progress,
                                attempt.patch().observed_date,
                                !attempt.patch().playback_start,
                            );
                            if patch.started.is_some() {
                                patch.started = attempt.patch().first_date.or(patch.started);
                            }
                            patch
                        };
                        if self.bridge.tracking.login_generation[service_index(service)]
                            .load(Ordering::Acquire)
                            != self.sessions[index].epoch
                        {
                            return Err(ApiError::Authentication);
                        }
                        self.sessions[index].client.update(
                            target.remote_media_id,
                            remote.as_ref(),
                            &patch,
                            now,
                        )
                    });
                let (outcome, remote) = match result {
                    Ok(remote) => (
                        DeliveryOutcome::Applied {
                            progress: remote.progress,
                        },
                        Some(remote),
                    ),
                    Err(error) => (
                        DeliveryOutcome::Failed(if alignment_changed {
                            DeliveryFailure::NeedsAlignment
                        } else {
                            delivery_failure(error)
                        }),
                        None,
                    ),
                };
                self.completion = Some(Completion::Progress(attempt, outcome, remote));
                if !self.commit_completion() {
                    return;
                }
            }
        }
    }
    fn commit_completion(&mut self) -> bool {
        let Some(completion) = self.completion.as_ref() else {
            return true;
        };
        let formats: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|s| {
                s.client
                    .viewer()
                    .ok()
                    .map(|v| (v.account.clone(), v.score_format))
            })
            .collect();
        let result = match completion {
            Completion::Progress(attempt, outcome, remote) => self.store.mutate(|state| {
                state.finish_delivery(attempt, *outcome, now_secs())?;
                if let Some(remote) = remote
                    && let Some(snapshot) = state
                        .snapshots
                        .iter_mut()
                        .find(|s| s.target == attempt.patch().target)
                {
                    snapshot.remote = Some(remote.clone());
                    if let Some((_, format)) = formats.iter().find(|(a, _)| *a == remote.account) {
                        snapshot.score_format = *format;
                    }
                }
                Ok(())
            }),
            Completion::Edit(attempt, result) => self.store.mutate(|state| {
                state.finish_edit(attempt, result.clone(), now_secs())?;
                if let Ok(remote) = result
                    && let Some(snapshot) = state
                        .snapshots
                        .iter_mut()
                        .find(|s| s.target == attempt.edit().target)
                    && let Some((_, format)) = formats.iter().find(|(a, _)| *a == remote.account)
                {
                    snapshot.score_format = *format;
                }
                Ok(())
            }),
        };
        if result.is_ok() {
            self.completion = None;
            true
        } else {
            self.notice = storage_message();
            false
        }
    }
    fn publish(&self) {
        use crate::{TrackingAccountRow, TrackingCandidateRow, TrackingLinkRow};
        let state = self.store.state();
        let accounts = vec![Service::MyAnimeList, Service::AniList]
            .into_iter()
            .map(|service| {
                let active = state
                    .active_accounts
                    .iter()
                    .find(|a| a.service == service)
                    .and_then(|key| state.accounts.iter().find(|a| a.key == *key));
                let connected = self.sessions.iter().any(|s| {
                    s.service == service
                        && !s.paused
                        && self.bridge.tracking.login_generation[service_index(service)]
                            .load(Ordering::Acquire)
                            == s.epoch
                });
                let registration = self.registrations.get(service);
                TrackingAccountRow {
                    service: service_index(service) as i32,
                    name: active
                        .map(|a| a.display_name.clone())
                        .unwrap_or_default()
                        .into(),
                    state: text::tr(if connected {
                        "Connected for this session"
                    } else {
                        "Reconnect to send retained updates"
                    })
                    .into(),
                    connected,
                    automatic: !state.automatic_paused.contains(&service),
                    signing_in: self.logins.iter().any(|l| {
                        l.service == service
                            && self.bridge.tracking.login_generation[service_index(service)]
                                .load(Ordering::Acquire)
                                == l.epoch
                    }),
                    client_id: registration.client_id.clone().into(),
                    redirect_uri: if registration.redirect_uri.is_empty() {
                        match service {
                            Service::MyAnimeList => "http://127.0.0.1:53926/callback",
                            Service::AniList => "https://anilist.co/api/v2/oauth/pin",
                        }
                        .into()
                    } else {
                        registration.redirect_uri.clone().into()
                    },
                }
            })
            .collect::<Vec<_>>();
        let history: HashMap<String, EpisodeProgress> = read_json_result(EPISODE_PROGRESS_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let links = self
            .context
            .as_ref()
            .map(|context| {
                state
                    .bindings
                    .iter()
                    .filter(|b| b.source == context.source)
                    .map(|binding| {
                        let snapshot = state.snapshots.iter().find(|s| s.target == binding.target);
                        let mut desired = snapshot.and_then(|s| s.remote.clone());
                        apply_desired(state, &binding.target, &mut desired);
                        let title = snapshot
                            .map(|s| s.media.title.clone())
                            .unwrap_or_else(|| binding.target.remote_media_id.to_string());
                        let status = desired
                            .as_ref()
                            .map(|r| r.status)
                            .unwrap_or(ListStatus::Planning);
                        let pending = state
                            .outbox
                            .pending()
                            .iter()
                            .rfind(|p| p.target == binding.target)
                            .map(|p| p.state)
                            .or_else(|| {
                                state
                                    .outbox
                                    .edits()
                                    .iter()
                                    .rfind(|e| e.target == binding.target)
                                    .map(|e| e.state)
                            });
                        let active = state.active_accounts.contains(&binding.target.account);
                        let connected = self.sessions.iter().any(|s| {
                            !s.paused
                                && s.client
                                    .viewer()
                                    .is_ok_and(|v| v.account == binding.target.account)
                        });
                        let state_label = if !binding.enabled {
                            "Needs alignment"
                        } else if !active {
                            "Inactive account"
                        } else if !connected {
                            "Reconnect to send retained updates"
                        } else {
                            match pending {
                                None => "Up to date",
                                Some(DeliveryState::Queued) => "Update queued",
                                Some(DeliveryState::InFlight) => "Sending update",
                                Some(DeliveryState::Uncertain) => "Waiting to retry",
                                Some(
                                    DeliveryState::AuthenticationRequired
                                    | DeliveryState::StaleAccount,
                                ) => "Reconnect to send retained updates",
                                Some(DeliveryState::Rejected) => {
                                    "Tracker rejected the update; edit the values"
                                }
                                Some(DeliveryState::NeedsAlignment) => "Needs alignment",
                            }
                        };
                        let score = desired.as_ref().map(|r| r.score_tenths).unwrap_or(0);
                        TrackingLinkRow {
                            id: binding.id.clone().into(),
                            title: format!(
                                "{} · {}",
                                service_name(binding.target.account.service),
                                title
                            )
                            .into(),
                            state: text::tr(state_label).into(),
                            progress: display_progress(state, &binding.target).to_string().into(),
                            status: status_index(status),
                            score: format_score(score).into(),
                            score_hint: text::tracking_score_hint(
                                snapshot
                                    .map(|s| s.score_format.maximum() / 10)
                                    .unwrap_or(10),
                                snapshot
                                    .is_some_and(|s| s.score_format == ScoreFormat::Point10Decimal),
                            )
                            .into(),
                            started: desired
                                .as_ref()
                                .map(|r| format_date(r.started))
                                .unwrap_or_default()
                                .into(),
                            completed: desired
                                .as_ref()
                                .map(|r| format_date(r.completed))
                                .unwrap_or_default()
                                .into(),
                            dates_writable: binding.target.account.service == Service::AniList,
                            history_preview: text::tracking_history_preview(
                                history_progress(state, &binding.target, &history)
                                    .max(display_progress(state, &binding.target)),
                            )
                            .into(),
                            coverage: text::tracking_coverage(
                                binding.assignments.len(),
                                binding
                                    .assignments
                                    .iter()
                                    .map(|a| a.target_episode.get())
                                    .min()
                                    .unwrap_or(0),
                                binding
                                    .assignments
                                    .iter()
                                    .map(|a| a.target_episode.get())
                                    .max()
                                    .unwrap_or(0),
                            )
                            .into(),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let candidates = self
            .candidates
            .iter()
            .map(|media| TrackingCandidateRow {
                title: media.title.clone().into(),
                reason: self
                    .context
                    .as_ref()
                    .map(|context| candidate_reason(context, self.candidate_service, media))
                    .unwrap_or_default()
                    .into(),
                detail: text::tracking_candidate(
                    &media.format,
                    media.year,
                    media.episodes.map(|n| n.get()),
                    media.id.get(),
                )
                .into(),
            })
            .collect::<Vec<_>>();
        let source_rows = self
            .context
            .as_ref()
            .map(|context| {
                context
                    .episodes
                    .iter()
                    .enumerate()
                    .map(|(i, (_, label))| SharedString::from(format!("{} · {label}", i + 1)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let preview_rows = self
            .choice
            .as_ref()
            .map(|choice| {
                choice
                    .assignments
                    .iter()
                    .map(|assignment| {
                        let label = self
                            .context
                            .as_ref()
                            .and_then(|c| {
                                c.episodes
                                    .iter()
                                    .find(|(id, _)| id == &assignment.episode_id)
                            })
                            .map(|(_, label)| label.as_str())
                            .unwrap_or(&assignment.episode_id);
                        SharedString::from(text::tracking_assignment(
                            label,
                            assignment.target_episode.get(),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mapping_rows = self
            .context
            .as_ref()
            .filter(|_| self.choice.is_some())
            .map(|context| {
                context
                    .episodes
                    .iter()
                    .enumerate()
                    .map(|(index, (id, label))| crate::TrackingMappingRow {
                        source_row: index as i32,
                        label: label.clone().into(),
                        target_episode: self
                            .choice
                            .as_ref()
                            .and_then(|c| c.assignments.iter().find(|a| a.episode_id == *id))
                            .map(|a| a.target_episode.to_string())
                            .unwrap_or_default()
                            .into(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let candidate_title = self
            .choice
            .as_ref()
            .map(|choice| choice.media.title.clone())
            .unwrap_or_default();
        let can_confirm = self
            .choice
            .as_ref()
            .is_some_and(|c| c.validated && !c.assignments.is_empty());
        let notice = self.notice.clone();
        let busy = self.busy;
        let selected_service = service_index(self.candidate_service) as i32;
        let bridge = self.bridge.clone();
        let generation = self.generation;
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = bridge.app() {
                if let Some(model) = update_rows(app.get_tracking_accounts(), accounts) {
                    app.set_tracking_accounts(model);
                }
                if bridge.tracking.context_generation.load(Ordering::Acquire) == generation
                    || !app.get_tracking_open()
                {
                    app.set_tracking_notice(notice.into());
                    app.set_tracking_busy(busy);
                    app.set_tracking_service(selected_service);
                    if let Some(model) = update_rows(app.get_tracking_links(), links) {
                        app.set_tracking_links(model);
                    }
                    if let Some(model) = update_rows(app.get_tracking_candidates(), candidates) {
                        app.set_tracking_candidates(model);
                    }
                    if let Some(model) = update_rows(app.get_tracking_source_rows(), source_rows) {
                        app.set_tracking_source_rows(model);
                    }
                    if let Some(model) = update_rows(app.get_tracking_preview_rows(), preview_rows)
                    {
                        app.set_tracking_preview_rows(model);
                    }
                    if let Some(model) = update_rows(app.get_tracking_mapping_rows(), mapping_rows)
                    {
                        app.set_tracking_mapping_rows(model);
                    }
                    app.set_tracking_candidate_title(candidate_title.into());
                    app.set_tracking_can_confirm(can_confirm);
                }
            }
        });
    }
}
fn checkpoint(
    state: &TrackingState,
    key: &TargetKey,
    map: &HashMap<String, EpisodeProgress>,
) -> Vec<Observation> {
    let mut observations = vec![];
    for binding in state
        .bindings
        .iter()
        .filter(|b| b.enabled && b.target == *key)
    {
        for assignment in &binding.assignments {
            let episode = SourceEpisode {
                source: binding.source.clone(),
                episode_id: assignment.episode_id.clone(),
            };
            if observations
                .iter()
                .any(|o: &Observation| o.episode == episode)
            {
                continue;
            }
            let watched = map.values().any(|p| {
                p.series_id == binding.source.source_id
                    && p.episode_id == assignment.episode_id
                    && p.watched
            });
            observations.push(Observation { episode, watched });
        }
    }
    observations
}
fn history_progress(
    state: &TrackingState,
    key: &TargetKey,
    map: &HashMap<String, EpisodeProgress>,
) -> u32 {
    state
        .bindings
        .iter()
        .filter(|b| b.enabled && b.target == *key)
        .flat_map(|b| {
            b.assignments
                .iter()
                .filter(|a| {
                    map.values().any(|p| {
                        p.series_id == b.source.source_id
                            && p.episode_id == a.episode_id
                            && p.watched
                    })
                })
                .map(|a| a.target_episode.get())
        })
        .max()
        .unwrap_or(0)
}
fn display_progress(state: &TrackingState, key: &TargetKey) -> u32 {
    if let Some(pending) = state.outbox.pending().iter().rfind(|p| p.target == *key) {
        return pending.progress;
    }
    state
        .projections
        .iter()
        .find(|p| p.target == *key)
        .and_then(|p| {
            state
                .targets
                .iter()
                .find(|t| t.key == *key)
                .and_then(|t| p.proposal(t).ok())
        })
        .map(|p| p.progress)
        .unwrap_or(0)
}
fn apply_desired(state: &TrackingState, key: &TargetKey, remote: &mut Option<RemoteEntry>) {
    if remote.is_none() {
        *remote = Some(RemoteEntry {
            account: key.account.clone(),
            media_id: key.remote_media_id,
            entry_id: None,
            progress: 0,
            status: ListStatus::Planning,
            score_tenths: 0,
            started: ListDate::default(),
            completed: ListDate::default(),
            repeating: false,
        });
    }
    if let Some(remote) = remote {
        for edit in state.outbox.edits().iter().filter(|e| e.target == *key) {
            if let Some(v) = edit.patch.status {
                remote.status = v;
            }
            if let Some(v) = edit.patch.score_tenths {
                remote.score_tenths = v;
            }
            if let Some(v) = edit.patch.started {
                remote.started = v;
            }
            if let Some(v) = edit.patch.completed {
                remote.completed = v;
            }
        }
    }
}
fn parse_status(index: i32) -> Option<ListStatus> {
    Some(match index {
        0 => ListStatus::Planning,
        1 => ListStatus::Watching,
        2 => ListStatus::Completed,
        3 => ListStatus::OnHold,
        4 => ListStatus::Dropped,
        5 => ListStatus::Repeating,
        _ => return None,
    })
}
fn status_index(status: ListStatus) -> i32 {
    match status {
        ListStatus::Planning => 0,
        ListStatus::Watching => 1,
        ListStatus::Completed => 2,
        ListStatus::OnHold => 3,
        ListStatus::Dropped => 4,
        ListStatus::Repeating => 5,
    }
}
fn service_name(service: Service) -> &'static str {
    match service {
        Service::MyAnimeList => "MyAnimeList",
        Service::AniList => "AniList",
    }
}
fn format_score(tenths: u32) -> String {
    if tenths.is_multiple_of(10) {
        (tenths / 10).to_string()
    } else {
        format!("{}.{:01}", tenths / 10, tenths % 10)
    }
}
fn format_date(date: ListDate) -> String {
    if date == ListDate::default() {
        String::new()
    } else {
        format!(
            "{}-{}-{}",
            date.year
                .map(|v| format!("{v:04}"))
                .unwrap_or("????".into()),
            date.month.map(|v| format!("{v:02}")).unwrap_or("??".into()),
            date.day.map(|v| format!("{v:02}")).unwrap_or("??".into())
        )
    }
}
fn parse_date(raw: &str) -> Result<ListDate, String> {
    if raw.trim().is_empty() {
        return Ok(ListDate::default());
    }
    let parts: Vec<_> = raw.trim().split('-').collect();
    if parts.len() > 3 {
        return Err(input_message());
    }
    let year = if parts[0] == "????" {
        None
    } else {
        Some(parts[0].parse::<u16>().map_err(|_| input_message())?)
    };
    let month = parts
        .get(1)
        .filter(|v| **v != "??")
        .map(|v| v.parse::<u8>())
        .transpose()
        .map_err(|_| input_message())?;
    let day = parts
        .get(2)
        .filter(|v| **v != "??")
        .map(|v| v.parse::<u8>())
        .transpose()
        .map_err(|_| input_message())?;
    let date = ListDate { year, month, day };
    if !date.valid() {
        return Err(input_message());
    }
    Ok(date)
}
fn api_message(error: ApiError) -> String {
    text::tr(match error {
        ApiError::Authentication | ApiError::WrongAccount => {
            "Tracker sign-in required. Reconnect this account."
        }
        ApiError::Offline => "Tracker unavailable. Queued updates are retained.",
        ApiError::RateLimited { .. } => {
            "Tracker rate limit. Updates will retry after the cooldown."
        }
        ApiError::Rejected => "Tracker rejected the request. Check the entry and values.",
        ApiError::InvalidResponse => "Invalid tracker response. The update remains unconfirmed.",
        ApiError::InvalidInput => "Check the client registration, tracker ID, or values.",
        ApiError::UnsupportedField => "This tracker does not support that field.",
    })
    .into()
}
fn delivery_failure(error: ApiError) -> DeliveryFailure {
    match error {
        ApiError::Offline | ApiError::InvalidResponse => DeliveryFailure::Transient,
        ApiError::RateLimited { retry_at } => DeliveryFailure::RateLimited { retry_at },
        ApiError::Authentication | ApiError::WrongAccount => {
            DeliveryFailure::AuthenticationRequired
        }
        _ => DeliveryFailure::Rejected,
    }
}
fn storage_message() -> String {
    text::tr("Tracking changes could not be saved. Check storage and retry.").into()
}
fn input_message() -> String {
    text::tr("Check episode alignment, progress, score, and date values.").into()
}
fn numeric(id: providers::ExternalId) -> Option<NonZeroU32> {
    match id {
        providers::ExternalId::MalAnime(n) | providers::ExternalId::AnilistAnime(n) => Some(n),
        _ => None,
    }
}

fn update_rows<T: Clone + PartialEq + 'static>(
    current: slint::ModelRc<T>,
    rows: Vec<T>,
) -> Option<slint::ModelRc<T>> {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<T>>()
        && model.row_count() == rows.len()
    {
        for (index, row) in rows.into_iter().enumerate() {
            if model.row_data(index).as_ref() != Some(&row) {
                model.set_row_data(index, row);
            }
        }
        None
    } else {
        Some(Rc::new(VecModel::from(rows)).into())
    }
}

fn reset_store() -> Result<Store<KvStorage>, nova_tracking::LoadError> {
    let owner = nova_sync::local_store()
        .map_err(|error| nova_tracking::LoadError::InvalidState(error.to_string()))?;
    let mut history = owner.lock().unwrap();
    history
        .save()
        .map_err(|error| nova_tracking::LoadError::InvalidState(error.to_string()))?;
    let rows = storage::try_scan_prefix(EVENT_PREFIX)?;
    Store::reset_with_journal_backup(KvStorage, &rows)
}

fn candidate_reason(context: &SourceContext, service: Service, media: &Media) -> String {
    let namespace = match service {
        Service::MyAnimeList => providers::IdNamespace::MalAnime,
        Service::AniList => providers::IdNamespace::AnilistAnime,
    };
    let direct = match context.ids.resolve_id(namespace) {
        providers::IdResolution::Unique(id) => numeric(id) == Some(media.id),
        _ => false,
    };
    let cross = service == Service::AniList
        && matches!(context.ids.resolve_id(providers::IdNamespace::MalAnime),providers::IdResolution::Unique(providers::ExternalId::MalAnime(id)) if media.mal_id==Some(id));
    let mut titles = vec![context.title.clone()];
    titles.extend(context.aliases.iter().cloned());
    let label = if direct {
        "Source tracker ID matches; confirm episode coverage."
    } else if cross {
        "Official MAL cross-reference matches; confirm episode coverage."
    } else {
        match title_match(
            media,
            &titles,
            context.year,
            context.source.media_type == "movie",
        ) {
            TitleMatch::TitleAndYear => {
                "Title and year match; confirm release and episode coverage."
            }
            TitleMatch::Title => {
                "Title or alias matches; check year, format, and episode coverage."
            }
            TitleMatch::SearchResult => "Search result; verify the release and episode coverage.",
        }
    };
    text::tr(label).into()
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    #[test]
    fn candidate_evidence_requires_the_correct_namespace_and_exact_id() {
        let mut context = SourceContext {
            source: SourceRef {
                provider_id: "nova".into(),
                source_id: "original-id".into(),
                media_type: "series".into(),
            },
            title: "Source title".into(),
            aliases: vec![],
            year: None,
            episodes: vec![],
            ids: providers::ExternalIds::default(),
        };
        let id = NonZeroU32::new(12).unwrap();
        let media = Media {
            id,
            mal_id: Some(id),
            title: "Different title".into(),
            format: "TV".into(),
            episodes: None,
            finished: false,
            year: None,
        };
        context.ids.typed.push(providers::ExternalId::MalAnime(id));
        assert_eq!(
            candidate_reason(&context, Service::MyAnimeList, &media),
            text::tr("Source tracker ID matches; confirm episode coverage.")
        );
        assert_eq!(
            candidate_reason(&context, Service::AniList, &media),
            text::tr("Official MAL cross-reference matches; confirm episode coverage.")
        );
        context.ids.typed.push(providers::ExternalId::MalAnime(
            NonZeroU32::new(99).unwrap(),
        ));
        assert_eq!(
            candidate_reason(&context, Service::MyAnimeList, &media),
            text::tr("Search result; verify the release and episode coverage.")
        );
    }
}

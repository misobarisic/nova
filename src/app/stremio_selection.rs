//! Explicit, account-bound title enrolment. A selection is upload authority,
//! separate from cached reconciliation state and discovered metadata aliases.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SyncMode {
    #[default]
    FullLibrary,
    SelectedTitles,
}
impl SyncMode {
    pub(super) fn index(self) -> i32 {
        i32::from(self == Self::SelectedTitles)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Selection {
    version: u32,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    pub native_type: String,
    pub native_id: String,
    pub remote_type: String,
    pub remote_id: String,
    pub name: String,
    pub episode_ids: BTreeMap<String, String>,
    native_revision: u64,
    remote_revision: u64,
    pub history_since_ms: u64,
    revision: String,
}
fn enabled_default() -> bool {
    true
}
impl Selection {
    fn valid(&self) -> bool {
        self.version == 1
            && matches!(self.native_type.as_str(), "movie" | "series")
            && self.native_type == self.remote_type
            && [self.native_id.as_str(), self.remote_id.as_str()]
                .iter()
                .all(|id| !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control))
            && (!self.enabled || !self.episode_ids.is_empty())
            && self.episode_ids.len() <= nova_stremio::MAX_VIDEOS
            && self
                .episode_ids
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.episode_ids.len()
    }
    pub(super) fn key(&self, account: &str) -> String {
        selection_key(account, &self.native_type, &self.native_id)
    }
    fn matches(&self, title: &Title) -> bool {
        title.item.id() == self.remote_id
            && title.item.media_type() == self.remote_type
            && title.mapping.id == self.native_id
            && title.mapping.media_type == self.native_type
            && title.mapping.episode_ids == self.episode_ids
    }
}
fn prefix(account: &str) -> String {
    format!("selection:{}:", serde_json::to_string(account).unwrap())
}
fn selection_key(account: &str, media_type: &str, id: &str) -> String {
    format!(
        "{}{}",
        prefix(account),
        serde_json::to_string(&(media_type, id)).unwrap()
    )
}
pub(super) fn load(
    store: &nova_sync::Store,
    connection: &Connection,
) -> Result<Vec<Selection>, ApiError> {
    let rows = store.records_with_prefix(
        &crate::app::sync::profile_domain(DOMAIN),
        &prefix(&connection.account_id),
    );
    if rows.len() > MAX_TITLES {
        return Err(ApiError::InvalidResponse);
    }
    rows.into_iter()
        .map(|(key, raw)| {
            let selection: Selection =
                serde_json::from_str(&raw).map_err(|_| ApiError::InvalidResponse)?;
            if !selection.valid() || key != selection.key(&connection.account_id) {
                return Err(ApiError::InvalidResponse);
            }
            Ok(selection)
        })
        .collect()
}
pub(super) fn allowed(mode: SyncMode, selections: &[Selection], title: &Title) -> bool {
    if selections.iter().any(|s| {
        !s.enabled
            && ((s.native_id == title.mapping.id && s.native_type == title.mapping.media_type)
                || (title.binding.is_none()
                    && s.remote_id == title.item.id()
                    && s.remote_type == title.item.media_type()))
    }) {
        return false;
    }
    // A cached canonical row must not import progress into another native title.
    // Explicit bindings retain their consent check in either mode so a peer's
    // unlink also revokes queued work during a full-library pass.
    match title.selection.as_ref() {
        Some(selection) => {
            selection.enabled
                && selection.matches(title)
                && selections.iter().any(|active| active == selection)
        }
        None => mode == SyncMode::FullLibrary,
    }
}
pub(super) fn authorize(
    store: &nova_sync::Store,
    connection: &Connection,
    title: &Title,
) -> Result<(), ApiError> {
    let mut current = connection.clone();
    session_flags(store, &mut current)?;
    if current.mode != connection.mode {
        return Err(ApiError::Rejected);
    }
    if let Some(selection) = &title.selection {
        if !selection.enabled || !selection.valid() || !selection.matches(title) {
            return Err(ApiError::Rejected);
        }
        let stored = store
            .record(
                &crate::app::sync::profile_domain(DOMAIN),
                &selection.key(&connection.account_id),
            )
            .and_then(|r| r.value.as_ref())
            .and_then(|raw| serde_json::from_str::<Selection>(raw).ok());
        if stored.as_ref() != Some(selection) {
            return Err(ApiError::Rejected);
        }
    } else if current.mode == SyncMode::SelectedTitles {
        return Err(ApiError::Rejected);
    }
    let native = store
        .record(
            &crate::app::sync::profile_domain(DOMAIN),
            &selection_key(
                &connection.account_id,
                &title.mapping.media_type,
                &title.mapping.id,
            ),
        )
        .and_then(|r| r.value.as_ref())
        .map(|raw| serde_json::from_str::<Selection>(raw).map_err(|_| ApiError::InvalidResponse))
        .transpose()?;
    if native.is_some_and(|s| !s.enabled)
        || (title.binding.is_none()
            && !store
                .records_with_prefix(
                    &crate::app::sync::profile_domain(DOMAIN),
                    &remote_exclusion_prefix(
                        &connection.account_id,
                        title.item.media_type(),
                        title.item.id(),
                    ),
                )
                .is_empty())
    {
        return Err(ApiError::Rejected);
    }
    Ok(())
}
fn remote_exclusion_prefix(account: &str, media_type: &str, id: &str) -> String {
    format!(
        "excluded_remote:{}:",
        serde_json::to_string(&(account, media_type, id)).unwrap()
    )
}
fn remote_exclusion_key(account: &str, selection: &Selection) -> String {
    format!(
        "{}{}",
        remote_exclusion_prefix(account, &selection.remote_type, &selection.remote_id),
        serde_json::to_string(&(&selection.native_type, &selection.native_id)).unwrap()
    )
}
fn remote_revision(videos: &[nova_stremio::Video]) -> u64 {
    nova_config::fnv1a(&serde_json::to_vec(videos).expect("remote video inventory serializes"))
}
pub(super) fn resolve(
    selection: &Selection,
    local: &stremio_metadata::LocalTitle,
    item: &LibraryItem,
    descriptors: &[serde_json::Value],
    fetch: &impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()>,
) -> Result<stremio_metadata::ResolvedTitle, stremio_metadata::PendingReason> {
    let mut resolved = stremio_metadata::resolve(item, &[], descriptors, fetch)?;
    let native_revision = stremio_metadata::local_inventory_revision(Some(local));
    let changed = native_revision != selection.native_revision
        || remote_revision(&resolved.videos) != selection.remote_revision;
    if local.id != selection.native_id
        || local.media_type != selection.native_type
        || (changed
            && selection.native_type != "movie"
            && !approved_pairs_present(selection, local, &resolved.videos))
    {
        return Err(stremio_metadata::PendingReason::EpisodeMappingUnavailable);
    }
    resolved.id = selection.native_id.clone();
    resolved.media_type = selection.native_type.clone();
    resolved.local_inventory_revision = native_revision;
    resolved.episode_ids = selection.episode_ids.clone();
    // Selecting a match authorizes a mapping, never replacement of native metadata.
    resolved.preserve_native_metadata = true;
    Ok(resolved)
}
fn approved_pairs_present(
    selection: &Selection,
    local: &stremio_metadata::LocalTitle,
    remote: &[nova_stremio::Video],
) -> bool {
    let native: std::collections::BTreeSet<_> =
        local.videos.iter().map(|v| v.id.as_str()).collect();
    let remote: std::collections::BTreeSet<_> = remote.iter().map(|v| v.id.as_str()).collect();
    // New episodes do not revoke approved older coverage or grant new coverage.
    selection
        .episode_ids
        .iter()
        .all(|(r, n)| remote.contains(r.as_str()) && native.contains(n.as_str()))
}

#[derive(Clone)]
pub(super) struct Context {
    media_type: String,
    id: String,
    name: String,
    videos: Vec<Video>,
}
impl Context {
    fn local(&self) -> stremio_metadata::LocalTitle {
        stremio_metadata::LocalTitle {
            media_type: self.media_type.clone(),
            id: self.id.clone(),
            videos: self.videos.clone(),
        }
    }
}
pub(super) enum Request {
    Search(Context, String, u64),
    Pick(usize, u64),
    Assign(usize, usize, u64),
    Confirm(bool, u64),
    Unlink(Context, u64),
}
pub(super) struct Review {
    context: Context,
    generation: u64,
    candidates: Vec<LibraryItem>,
    picked: Option<LibraryItem>,
    remote: Option<stremio_metadata::ResolvedTitle>,
    assignments: Vec<usize>,
}
impl Bridge {
    fn stremio_title_context(&self) -> Option<Context> {
        let state = self.personal_state()?;
        let modal = state.modal_item.as_ref()?;
        Some(Context {
            media_type: modal.type_.clone(),
            id: modal.id.clone(),
            name: modal.name.clone(),
            videos: modal.videos.clone(),
        })
    }
    pub(in crate::app) fn stremio_mode(&self, index: i32) {
        if !self.require_profile_management() {
            return;
        }
        let mode = match index {
            0 => SyncMode::FullLibrary,
            1 => SyncMode::SelectedTitles,
            _ => return,
        };
        let generation = self.stremio().generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.stremio().send(Command::Mode(mode, generation));
    }
    pub(in crate::app) fn stremio_title_search(&self, query: String) {
        if !self.require_profile_management() {
            return;
        }
        let Some(context) = self.stremio_title_context() else {
            return;
        };
        let handle = self.stremio();
        let review = handle.review_generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(app) = self.app() {
            app.set_stremio_title_busy(true);
            app.set_stremio_title_can_confirm(false);
            app.set_stremio_title_match("".into());
            app.set_stremio_title_status(text::tr("Loading Stremio matches…").into());
        }
        handle.send(Command::Review(
            Request::Search(context, query, review),
            handle.generation.load(Ordering::Acquire),
        ));
    }
    pub(in crate::app) fn stremio_title_pick(&self, index: i32) {
        if !self.require_profile_management() {
            return;
        }
        if let Ok(index) = usize::try_from(index) {
            self.stremio_title_request(|g| Request::Pick(index, g));
        }
    }
    pub(in crate::app) fn stremio_title_assign(&self, row: i32, target: i32) {
        if !self.require_profile_management() {
            return;
        }
        if let (Ok(row), Ok(target)) = (usize::try_from(row), usize::try_from(target)) {
            self.stremio_title_request(|g| Request::Assign(row, target, g));
        }
    }
    pub(in crate::app) fn stremio_title_confirm(&self, history: bool) {
        if !self.require_profile_management() {
            return;
        }
        self.stremio_title_request(|g| Request::Confirm(history, g));
    }
    pub(in crate::app) fn stremio_title_unlink(&self) {
        if !self.require_profile_management() {
            return;
        }
        if let Some(context) = self.stremio_title_context() {
            self.stremio_title_request(|g| Request::Unlink(context, g));
        }
    }
    fn stremio_title_request(&self, request: impl FnOnce(u64) -> Request) {
        let h = self.stremio();
        h.send(Command::Review(
            request(h.review_generation.load(Ordering::Acquire)),
            h.generation.load(Ordering::Acquire),
        ));
    }
    pub(in crate::app) fn stremio_title_close(&self) {
        self.stremio()
            .review_generation
            .fetch_add(1, Ordering::AcqRel);
    }
}
fn review_ui(bridge: &Bridge, generation: u64, action: impl FnOnce(AppWindow) + Send + 'static) {
    let handle = bridge.stremio();
    ui(bridge, move |app| {
        if handle.review_generation.load(Ordering::Acquire) == generation
            && app.get_tracking_open()
            && app.get_tracking_service() == 2
        {
            action(app);
        }
    });
}
pub(super) fn review_error(bridge: &Bridge, generation: u64, error: ApiError) {
    review_ui(bridge, generation, move |app| {
        app.set_stremio_title_busy(false);
        app.set_stremio_title_can_confirm(false);
        app.set_stremio_title_status(
            text::tr(if error == ApiError::Authentication {
                "Connect Stremio in Settings → Integrations first."
            } else {
                "Could not prepare Stremio tracking. Try again."
            })
            .into(),
        );
    });
}

/// Old full-library work is not authority for a newly enrolled binding. Keep
/// remote observations independent, and let other selected bindings retain
/// their own fields in the shared account row.
pub(super) fn restrict_shared(selection: Option<&Selection>, state: &mut Canonical) {
    let Some(selection) = selection else {
        return;
    };
    state.watched.retain(|id, field| {
        selection.episode_ids.contains_key(id) && field.changed_at > selection.history_since_ms
    });
    if state.resume.changed_at <= selection.history_since_ms
        || state
            .resume
            .value
            .as_ref()
            .is_some_and(|r| !selection.episode_ids.contains_key(&r.video_id))
    {
        state.resume = Stamped::default();
    }
}
pub(super) fn restrict_for_mode(
    mode: SyncMode,
    selection: Option<&Selection>,
    state: &mut Canonical,
) {
    if mode == SyncMode::FullLibrary {
        let selection = selection.cloned().map(|mut s| {
            s.history_since_ms = 0;
            s
        });
        restrict_shared(selection.as_ref(), state);
    } else {
        restrict_shared(selection, state);
    }
}

#[cfg(test)]
#[path = "stremio_selection_tests.rs"]
mod tests;
fn episode_label(video: &Video) -> String {
    format!("{} · {}", episode_se_label(video), video.label())
}
fn review_fetch(
    bridge: Bridge,
    generation: u64,
    review: u64,
) -> impl Fn(&str, Duration, usize) -> Result<Vec<u8>, ()> {
    let owner = profiles::capture();
    let handle = bridge.stremio();
    move |url, timeout, limit| {
        if !profiles::is_current(&owner)
            || handle.generation.load(Ordering::Acquire) != generation
            || handle.review_generation.load(Ordering::Acquire) != review
        {
            return Err(());
        }
        net::fetch_metadata_bytes(url, timeout, limit).map_err(|_| ())
    }
}
impl<T: nova_stremio::Transport> Worker<T> {
    pub(super) fn selection_request(
        &mut self,
        request: Request,
        generation: u64,
    ) -> Result<(), ApiError> {
        let Some(connection) = self.connection.clone().filter(|_| self.verified) else {
            return Err(ApiError::Authentication);
        };
        self.check_current(&connection, generation)?;
        let rg = self
            .bridge
            .stremio()
            .review_generation
            .load(Ordering::Acquire);
        match request {
            Request::Search(context, query, review) if review == rg => {
                self.review = None;
                let selected = {
                    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                    load(&owner.lock().unwrap(), &connection)?
                        .into_iter()
                        .find(|s| s.native_type == context.media_type && s.native_id == context.id)
                };
                let linked = selected
                    .as_ref()
                    .map_or(connection.mode == SyncMode::FullLibrary, |s| s.enabled);
                review_ui(&self.bridge, review, move |app| {
                    app.set_stremio_title_linked(linked);
                    app.set_stremio_title_candidates(
                        Rc::new(VecModel::<crate::TrackingCandidateRow>::default()).into(),
                    );
                    app.set_stremio_title_mapping(
                        Rc::new(VecModel::<crate::TrackingMappingRow>::default()).into(),
                    );
                });
                self.descriptors = self
                    .client
                    .addon_collection(&connection.auth_key, now_secs())?;
                self.check_current(&connection, generation)?;
                let fetch = review_fetch(self.bridge.clone(), generation, review);
                let mut candidates = Vec::new();
                if query.trim().is_empty() {
                    if let Some(s) = &selected {
                        candidates.push(LibraryItem::new(&s.remote_id, &s.name, &s.remote_type, 0));
                    }
                    if let Ok((kind, id)) =
                        stremio_metadata::outbound_identity(&context.local(), &self.descriptors)
                        && !candidates
                            .iter()
                            .any(|c| c.id() == id && c.media_type() == kind)
                    {
                        candidates.push(LibraryItem::new(&id, &context.name, &kind, 0));
                    }
                }
                let term = if query.trim().is_empty() {
                    context.name.as_str()
                } else {
                    query.trim()
                };
                candidates.extend(stremio_metadata::search(
                    &context.media_type,
                    term,
                    &self.descriptors,
                    &fetch,
                ));
                let mut seen = std::collections::BTreeSet::new();
                candidates.retain(|item| {
                    seen.insert((item.media_type().to_owned(), item.id().to_owned()))
                });
                candidates.truncate(30);
                let rows: Vec<_> = candidates
                    .iter()
                    .map(|item| crate::TrackingCandidateRow {
                        title: item.name().into(),
                        detail: item.id().into(),
                        reason: "".into(),
                    })
                    .collect();
                self.review = Some(Review {
                    context,
                    generation: review,
                    candidates,
                    picked: None,
                    remote: None,
                    assignments: Vec::new(),
                });
                review_ui(&self.bridge, review, move |app| {
                    app.set_stremio_title_busy(false);
                    app.set_stremio_title_candidates(Rc::new(VecModel::from(rows)).into());
                    app.set_stremio_title_status(
                        text::tr("Choose a Stremio match, then review episode alignment.").into(),
                    );
                });
            }
            Request::Pick(index, review) if review == rg => {
                let Some(draft) = self.review.as_mut().filter(|r| r.generation == review) else {
                    return Ok(());
                };
                let Some(item) = draft.candidates.get(index).cloned() else {
                    return Ok(());
                };
                let fetch = review_fetch(self.bridge.clone(), generation, review);
                let resolved = stremio_metadata::resolve(&item, &[], &self.descriptors, &fetch)
                    .map_err(|_| ApiError::InvalidResponse)?;
                let selected = {
                    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                    load(&owner.lock().unwrap(), &connection)?
                        .into_iter()
                        .find(|s| {
                            s.native_type == draft.context.media_type
                                && s.native_id == draft.context.id
                                && s.remote_id == item.id()
                        })
                };
                draft.assignments = if draft.context.media_type == "movie" {
                    vec![1]
                } else {
                    draft
                        .context
                        .videos
                        .iter()
                        .map(|native| {
                            resolved
                                .videos
                                .iter()
                                .position(|remote| {
                                    selected.as_ref().is_some_and(|s| {
                                        s.episode_ids.get(&remote.id) == Some(&native.id)
                                    }) || (native.id == remote.id
                                        || confirmed_episode_stream_ids(native)
                                            .iter()
                                            .any(|id| id == &remote.id))
                                })
                                .map_or(0, |index| index + 1)
                        })
                        .collect()
                };
                draft.picked = Some(item);
                draft.remote = Some(resolved);
                self.publish_review();
            }
            Request::Assign(row, target, review) if review == rg => {
                if let Some(draft) = self.review.as_mut().filter(|r| r.generation == review)
                    && target <= draft.remote.as_ref().map_or(0, |r| r.videos.len())
                    && let Some(assignment) = draft.assignments.get_mut(row)
                {
                    *assignment = target;
                }
                self.publish_review();
            }
            Request::Confirm(history, review) if review == rg => {
                let Some(draft) = self.review.as_ref().filter(|r| r.generation == review) else {
                    return Ok(());
                };
                let Some(item) = &draft.picked else {
                    return Ok(());
                };
                let Some(remote) = &draft.remote else {
                    return Ok(());
                };
                let Some(ids) = assignments(draft) else {
                    return Err(ApiError::InvalidInput);
                };
                let selection = Selection {
                    version: 1,
                    enabled: true,
                    native_type: draft.context.media_type.clone(),
                    native_id: draft.context.id.clone(),
                    remote_type: item.media_type().into(),
                    remote_id: item.id().into(),
                    name: item.name().into(),
                    episode_ids: ids,
                    native_revision: stremio_metadata::local_inventory_revision(Some(
                        &draft.context.local(),
                    )),
                    remote_revision: remote_revision(&remote.videos),
                    history_since_ms: if history { 0 } else { nova_config::now_ms() },
                    revision: format!(
                        "{}:{}",
                        nova_config::now_ms(),
                        nova_sync::local_device().map_err(|_| ApiError::Rejected)?
                    ),
                };
                self.check_current(&connection, generation)?;
                self.write_selection(
                    &connection,
                    &selection.key(&connection.account_id),
                    Some(serde_json::to_string(&selection).map_err(|_| ApiError::InvalidInput)?),
                )?;
                // Enrolment itself never starts a whole-library merge.
                if connection.mode == SyncMode::SelectedTitles {
                    self.save_flag(&connection, SessionFlag::Started)?;
                    self.force = true;
                }
                review_ui(&self.bridge, review, |app| {
                    app.set_stremio_title_linked(true);
                    app.set_stremio_title_can_confirm(false);
                    app.set_stremio_title_status(
                        text::tr("This title is enabled for Stremio sync.").into(),
                    );
                });
            }
            Request::Unlink(context, review) if review == rg => {
                let key = selection_key(&connection.account_id, &context.media_type, &context.id);
                let mut excluded = {
                    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
                    load(&owner.lock().unwrap(), &connection)?
                        .into_iter()
                        .find(|s| s.native_id == context.id && s.native_type == context.media_type)
                }
                .unwrap_or_else(|| {
                    let remote = self
                        .titles
                        .values()
                        .find(|t| {
                            t.mapping.id == context.id && t.mapping.media_type == context.media_type
                        })
                        .map(|t| t.item.id().to_owned())
                        .or_else(|| {
                            stremio_metadata::outbound_identity(&context.local(), &self.descriptors)
                                .ok()
                                .map(|(_, id)| id)
                        })
                        .unwrap_or_else(|| context.id.clone());
                    Selection {
                        version: 1,
                        enabled: false,
                        native_type: context.media_type.clone(),
                        native_id: context.id.clone(),
                        remote_type: context.media_type.clone(),
                        remote_id: remote,
                        name: context.name.clone(),
                        episode_ids: BTreeMap::new(),
                        native_revision: 0,
                        remote_revision: 0,
                        history_since_ms: 0,
                        revision: String::new(),
                    }
                });
                excluded.enabled = false;
                self.write_selection(
                    &connection,
                    &key,
                    Some(serde_json::to_string(&excluded).map_err(|_| ApiError::InvalidInput)?),
                )?;
                self.force = connection.mode == SyncMode::SelectedTitles;
                review_ui(&self.bridge, review, |app| {
                    app.set_stremio_title_linked(false);
                    app.set_stremio_title_can_confirm(false);
                    app.set_stremio_title_status(
                        text::tr(
                            "Stremio tracking stopped for this title. Existing history is kept.",
                        )
                        .into(),
                    );
                });
            }
            _ => {}
        }
        Ok(())
    }
    fn write_selection(
        &mut self,
        connection: &Connection,
        key: &str,
        value: Option<String>,
    ) -> Result<(), ApiError> {
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let current = store
            .record(&crate::app::sync::profile_domain(DOMAIN), CONNECTION)
            .and_then(|r| r.value.as_ref())
            .and_then(|raw| serde_json::from_str::<Connection>(raw).ok());
        if current.as_ref().is_none_or(|c| !c.same_session(connection)) {
            return Err(ApiError::Authentication);
        }
        let domain = crate::app::sync::profile_domain(DOMAIN);
        if let Some(old) = store
            .record(&domain, key)
            .and_then(|r| r.value.as_ref())
            .and_then(|raw| serde_json::from_str::<Selection>(raw).ok())
        {
            store.set(
                &domain,
                &remote_exclusion_key(&connection.account_id, &old),
                None,
                0,
                device,
            );
        }
        if let Some(new) = value
            .as_ref()
            .and_then(|raw| serde_json::from_str::<Selection>(raw).ok())
            && !new.enabled
        {
            store.set(
                &domain,
                &remote_exclusion_key(&connection.account_id, &new),
                Some("true".into()),
                0,
                device,
            );
        }
        store.set(&domain, key, value, 0, device);
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        Ok(())
    }
    fn publish_review(&self) {
        let Some(draft) = &self.review else {
            return;
        };
        let Some(remote) = &draft.remote else {
            return;
        };
        let Some(item) = &draft.picked else {
            return;
        };
        let title = item.name().to_owned();
        let mut options = vec![text::tr("Not mapped").into()];
        options.extend(remote.videos.iter().map(|video| {
            format!(
                "S{} E{} · {}",
                video.season.unwrap_or(0),
                video.episode.unwrap_or(0),
                video.id
            )
            .into()
        }));
        let rows: Vec<_> = draft
            .context
            .videos
            .iter()
            .zip(&draft.assignments)
            .map(|(video, target)| crate::TrackingMappingRow {
                label: episode_label(video).into(),
                source_row: *target as i32,
                target_episode: "".into(),
            })
            .collect();
        let valid = assignments(draft).is_some();
        review_ui(&self.bridge, draft.generation, move |app| {
            app.set_stremio_title_match(title.into());
            app.set_stremio_title_options(Rc::new(VecModel::from(options)).into());
            app.set_stremio_title_mapping(Rc::new(VecModel::from(rows)).into());
            app.set_stremio_title_can_confirm(valid);
            app.set_stremio_title_busy(false);
            app.set_stremio_title_status(
                text::tr("Only mapped episodes will sync. Review the match before enabling it.")
                    .into(),
            );
        });
    }
}
fn assignments(review: &Review) -> Option<BTreeMap<String, String>> {
    let remote = review.remote.as_ref()?;
    if review.context.media_type == "movie" {
        return Some(
            [(
                review.picked.as_ref()?.id().into(),
                review.context.id.clone(),
            )]
            .into(),
        );
    }
    if review.assignments.len() != review.context.videos.len() {
        return None;
    }
    let mut mapped = BTreeMap::new();
    for (native, &target) in review.context.videos.iter().zip(&review.assignments) {
        if target == 0 {
            continue;
        }
        let video = remote.videos.get(target - 1)?;
        if mapped.insert(video.id.clone(), native.id.clone()).is_some() {
            return None;
        }
    }
    (!mapped.is_empty()).then_some(mapped)
}

//! Prepare remote materialization on the Stremio worker. The event loop only
//! accepts a still-current, record-sized patch and queues its ordered save.
use super::*;

#[derive(Clone, PartialEq)]
struct NativeSnapshot {
    entry: Option<LibraryEntry>,
    progress: HashMap<String, EpisodeProgress>,
}

impl NativeSnapshot {
    fn capture(state: &Shared, mapping: &Mapping) -> Self {
        Self {
            entry: state
                .entries
                .iter()
                .find(|entry| entry.id == mapping.id && entry.type_ == mapping.media_type)
                .cloned(),
            progress: mapping
                .episode_ids
                .values()
                .filter_map(|id| {
                    let key = progress_map_key(&mapping.id, id);
                    state.progress.get(&key).map(|value| (key, value.clone()))
                })
                .collect(),
        }
    }

    fn matches(&self, state: &Shared, mapping: &Mapping) -> bool {
        let entry = state
            .entries
            .iter()
            .find(|entry| entry.id == mapping.id && entry.type_ == mapping.media_type);
        entry == self.entry.as_ref()
            && mapping.episode_ids.values().all(|id| {
                let key = progress_map_key(&mapping.id, id);
                state.progress.get(&key) == self.progress.get(&key)
            })
    }
}

pub(super) struct Prepared {
    before: NativeSnapshot,
    clock: nova_sync::Hlc,
    mapping: Mapping,
    desired: Canonical,
    patch: persistence::ExternalPatch,
    persisted_patch: persistence::ExternalPatch,
}

impl Prepared {
    pub(super) fn unchanged(&self) -> Option<Canonical> {
        self.patch.is_empty().then(|| self.desired.clone())
    }
}

pub(super) enum Accepted {
    Retry,
    Ready(Canonical, Option<mpsc::Receiver<Result<(), String>>>),
}

/// A clock check brackets reads from Shared and the sync owner. If another
/// writer intervenes, retry on the worker rather than reconcile on the UI.
pub(super) fn prepare(
    bridge: &Bridge,
    connection: &Connection,
    title: &Title,
    with_progress: bool,
    generation: u64,
) -> Result<Option<Prepared>, ApiError> {
    if generation != bridge.stremio.generation.load(Ordering::Acquire) {
        return Err(ApiError::Rejected);
    }
    if !writable_key("library") || (with_progress && !writable_key(EPISODE_PROGRESS_KEY)) {
        return Err(ApiError::Rejected);
    }
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let clock = {
        let store = owner.lock().unwrap();
        let current = store
            .record(DOMAIN, CONNECTION)
            .and_then(|row| row.value.as_ref())
            .and_then(|raw| serde_json::from_str::<Connection>(raw).ok());
        if current
            .as_ref()
            .is_none_or(|current| !current.valid() || !current.same_session(connection))
        {
            return Err(ApiError::Authentication);
        }
        store.clock()
    };
    let before = NativeSnapshot::capture(&bridge.shared.lock().unwrap(), &title.mapping);
    let entries: Vec<_> = before.entry.iter().cloned().collect();
    let projection = local_projection(&title.mapping, &entries, &before.progress);
    let local = source_canonical(connection, title, &projection, &entries, &before.progress)?;
    let shared = read_shared(connection, title.item.id())?;
    let mut title = title.clone();
    title.desired = nova_stremio::reconcile(&local, &title.desired, false);
    title.desired = nova_stremio::reconcile(&title.desired, &shared, false);
    if owner.lock().unwrap().clock() != clock {
        return Ok(None);
    }
    let mut patch = persistence::ExternalPatch::default();
    let id = &title.mapping.id;
    if title.desired.saved.value != before.entry.is_some() {
        patch.library = Some(persistence::ExternalLibraryEdit {
            id: id.clone(),
            media_type: title.mapping.media_type.clone(),
            entry: title.desired.saved.value.then(|| LibraryEntry {
                id: id.clone(),
                type_: title.mapping.media_type.clone(),
                name: title.item.name().into(),
                year: String::new(),
                poster_url: title
                    .item
                    .0
                    .get("poster")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into(),
                background_url: String::new(),
                genres: Vec::new(),
                description: String::new(),
                categories: Vec::new(),
                watch_status: WatchStatus::Auto,
                added_at_secs: title
                    .item
                    .0
                    .get("_ctime")
                    .and_then(|v| v.as_str())
                    .and_then(nova_stremio::released_timestamp_ms)
                    .map_or(title.desired.saved.changed_at / 1000, |t| {
                        (t.max(0) as u64) / 1000
                    }),
            }),
        });
    }
    if with_progress {
        let mut progress = before.progress.clone();
        for (remote, field) in &title.desired.watched {
            let Some(native) = title.mapping.episode_ids.get(remote) else {
                continue;
            };
            let key = progress_map_key(id, native);
            if !field.value && !progress.contains_key(&key) {
                continue;
            }
            let value = progress
                .entry(key)
                .or_insert_with(|| imported_progress(&title, native));
            if value.history.is_none() {
                value.history = imported_progress(&title, native).history;
            }
            if value.watched != field.value {
                value.watched = field.value;
                value.updated_at_secs = field.changed_at / 1000;
                value.unwatched_at_secs = if field.value {
                    0
                } else {
                    (field.changed_at / 1000).max(1)
                };
                if !field.value {
                    value.position_secs = 0.0;
                }
            }
        }
        if let Some(resume) = &title.desired.resume.value {
            if let Some(native) = title.mapping.episode_ids.get(&resume.video_id) {
                let value = progress
                    .entry(progress_map_key(id, native))
                    .or_insert_with(|| imported_progress(&title, native));
                if value.history.is_none() {
                    value.history = imported_progress(&title, native).history;
                }
                value.position_secs = resume.position_ms as f64 / 1000.0;
                value.duration_secs = resume.duration_ms as f64 / 1000.0;
                value.updated_at_secs = title.desired.resume.changed_at / 1000;
                if value.watched {
                    value
                        .history
                        .get_or_insert_with(PlaybackHistory::default)
                        .rewatch = true;
                }
            }
        } else if title.desired.resume.changed_at > title.local.resume.changed_at
            && let Some(old) = &title.local.resume.value
            && let Some(native) = title.mapping.episode_ids.get(&old.video_id)
            && let Some(value) = progress.get_mut(&progress_map_key(id, native))
        {
            value.position_secs = 0.0;
            value.updated_at_secs = title.desired.resume.changed_at / 1000;
        }
        patch.progress = progress
            .into_iter()
            .filter(|(key, value)| before.progress.get(key) != Some(value))
            .collect();
    }
    Ok(Some(Prepared {
        before,
        clock,
        mapping: title.mapping,
        desired: title.desired,
        persisted_patch: patch.clone(),
        patch,
    }))
}

/// Only this small compare-and-apply runs on the event loop. A busy store is
/// never waited on here; disk/JSON work and newer-action reconciliation stay
/// on the worker. The queue insertion shares the UI's local-edit ordering.
pub(super) fn accept(
    bridge: &Bridge,
    prepared: Prepared,
    generation: u64,
) -> Result<Accepted, ApiError> {
    if generation != bridge.stremio.generation.load(Ordering::Acquire) {
        return Err(ApiError::Rejected);
    }
    let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
    let Ok(store) = owner.try_lock() else {
        return Ok(Accepted::Retry);
    };
    if store.clock() != prepared.clock {
        return Ok(Accepted::Retry);
    }
    let Ok(mut state) = bridge.shared.try_lock() else {
        return Ok(Accepted::Retry);
    };
    if !prepared.before.matches(&state, &prepared.mapping) {
        return Ok(Accepted::Retry);
    }
    if prepared.patch.is_empty() {
        return Ok(Accepted::Ready(prepared.desired, None));
    }
    let Some(receipt) = persistence::try_external(prepared.persisted_patch) else {
        return Ok(Accepted::Retry);
    };
    if let Some(edit) = prepared.patch.library {
        state
            .entries
            .retain(|entry| entry.id != edit.id || entry.type_ != edit.media_type);
        if let Some(entry) = edit.entry {
            state.entries.push(entry);
        }
    }
    state.progress.extend(prepared.patch.progress);
    Ok(Accepted::Ready(prepared.desired, Some(receipt)))
}

pub(super) fn wait(receipt: mpsc::Receiver<Result<(), String>>) -> Result<(), ApiError> {
    receipt
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| ApiError::Rejected)?
        .map_err(|error| {
            tracing::warn!(%error, "Stremio background materialization failed");
            ApiError::Rejected
        })
}

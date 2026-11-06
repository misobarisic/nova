//! Profile navigation swaps personal state while account sync stays connected.
use super::*;
use unicode_normalization::UnicodeNormalization;

// Hash the durable ID, not the name or row index: a profile keeps its colors
// after a rename, sync reorder, restart or selection on another device. Both
// colors vary; curated pairs keep even small initials readable on every theme.
const AVATAR_COLORS: [(u32, u32); 12] = [
    (0x3c254b, 0xffd5b5),
    (0xecc3d5, 0x502550),
    (0x254633, 0xc5efcf),
    (0x153b50, 0xb9e9ff),
    (0xe4d2a5, 0x4e361d),
    (0x352c64, 0xded3ff),
    (0xa9ddd2, 0x153d36),
    (0x543025, 0xffdac1),
    (0xd0d8fa, 0x293667),
    (0x3f401b, 0xe5eead),
    (0xedc8b2, 0x5b2c20),
    (0x253f54, 0xc8e2f7),
];

fn profile_row(profile: &profiles::Profile, active: &str) -> crate::ProfileRow {
    let name = if profile.name == "Default" {
        text::tr("Default")
    } else {
        profile.name.as_str()
    };
    let initial = name
        .trim()
        .nfc()
        .next()
        .and_then(|letter| letter.to_uppercase().next())
        .unwrap_or('?')
        .to_string();
    // Explicit FNV-1a avoids the randomized, implementation-dependent default
    // hasher, so the same synced identity has the same appearance everywhere.
    let hash = profile
        .id
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let (background, foreground) = AVATAR_COLORS[(hash % AVATAR_COLORS.len() as u64) as usize];
    crate::ProfileRow {
        id: profile.id.clone().into(),
        name: name.into(),
        active: profile.id == active,
        initial: initial.into(),
        avatar_background: slint::Color::from_argb_encoded(0xff000000 | background),
        avatar_foreground: slint::Color::from_argb_encoded(0xff000000 | foreground),
    }
}

pub(super) struct WorkerDone(pub Arc<AtomicBool>);
impl Drop for WorkerDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn read_owned<T: serde::de::DeserializeOwned + Default>(
    store: &profiles::ProfileStore,
    key: &str,
) -> Result<T, String> {
    store
        .read(key)
        .map_err(|error| error.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|error| format!("Unreadable {key}: {error}")))
        .transpose()
        .map(Option::unwrap_or_default)
}

#[derive(Clone)]
pub(super) struct OutgoingTransition {
    pub(super) progress: Option<HashMap<String, EpisodeProgress>>,
    settings: Option<settings::CapturedSettings>,
    torrent: settings::CapturedSettings,
    preferences: HashMap<String, Option<nova_config::playback_preferences::PreferenceValue>>,
    journal: Option<(profiles::ProfileStore, viewing_journal::ViewingSession)>,
}

impl OutgoingTransition {
    pub(super) fn for_sync(bridge: &Bridge) -> Self {
        let state = bridge.shared.lock().unwrap();
        Self {
            progress: None,
            settings: settings::capture_local_settings(&state.cache_settings),
            torrent: bridge.capture_torrent_edits(),
            preferences: state.playback_preference_edits.clone(),
            journal: None,
        }
    }

    pub(super) fn for_policy_stop(bridge: &Bridge) -> Self {
        let mut capture = Self::for_sync(bridge);
        capture.progress = bridge.preview_policy_progress();
        capture.journal = bridge.preview_policy_journal();
        capture
    }
    pub(super) fn accept_policy_journal(&self, bridge: &Bridge) {
        if let Some((_, row)) = &self.journal {
            bridge.accept_policy_journal(row);
        }
    }
    pub(super) fn save(&self, owner: &profiles::ProfileStore) -> Result<(), String> {
        fn write<T: Serialize>(
            owner: &profiles::ProfileStore,
            key: &str,
            value: &T,
        ) -> Result<(), String> {
            let raw = serde_json::to_string(value).map_err(|error| error.to_string())?;
            owner
                .persist_snapshot(key, &raw, false)
                .map_err(|error| error.to_string())
        }
        if let Some(progress) = &self.progress {
            // This is the captured playback edit, not a replacement of unrelated
            // earlier external patches still waiting ahead of this transition.
            let mut committed: HashMap<String, EpisodeProgress> =
                read_owned(owner, EPISODE_PROGRESS_KEY)?;
            committed.extend(progress.clone());
            write(owner, EPISODE_PROGRESS_KEY, &committed)?;
        }
        if !self.preferences.is_empty() {
            let mut values: HashMap<String, nova_config::playback_preferences::PreferenceValue> =
                read_owned(owner, playback_preferences::PREFERENCES_KEY)?;
            for (key, edit) in &self.preferences {
                match edit {
                    Some(value) => {
                        values.insert(key.clone(), value.clone());
                    }
                    None => {
                        values.remove(key);
                    }
                }
            }
            write(owner, playback_preferences::PREFERENCES_KEY, &values)?;
        }
        if let Some(settings) = &self.settings {
            settings.save_admitted::<CacheSettings>(owner, "settings")?;
        }
        self.torrent
            .save_admitted::<TorrentSettings>(owner, TORRENT_SETTINGS_KEY)?;
        if let Some((journal_owner, row)) = &self.journal {
            viewing_journal::persist_captured(journal_owner, row)?;
        }
        Ok(())
    }
}

/// Detached storage data only. UI allocations are constructed after accepting
/// this request's durable receipt, never on the preparation worker.
struct PreparedProfileData {
    policy: profile_policy::ProfilePolicy,
    settings: CacheSettings,
    download_settings: DownloadSettings,
    entries: Vec<LibraryEntry>,
    progress: HashMap<String, EpisodeProgress>,
    continue_hidden: HashMap<String, u64>,
    addons: Vec<AddonStore>,
    plugins: nova_providers::plugin_settings::PreparedPluginSettings,
}

impl PreparedProfileData {
    fn load(id: &str, last_stream: &last_stream::Cache) -> Result<Self, String> {
        let store = profiles::ProfileStore::for_profile(id);
        // Settings recovery retains unknown fields by durable owner. Even
        // detached preparation must attribute that bookkeeping to the incoming
        // profile, without granting it access to an activation's mutable state.
        store.run(|| {
            let settings = match store.read("settings").map_err(|error| error.to_string())? {
                Some(raw) => recover_settings(
                    serde_json::from_str(&raw)
                        .map_err(|error| format!("Unreadable settings: {error}"))?,
                )
                .ok_or("Unreadable settings object retained")?,
                None => CacheSettings::default(),
            };
            let mut data = Self {
                policy: profile_policy::load(&store)?,
                settings,
                download_settings: read_download_settings(),
                entries: read_owned(&store, "library")?,
                progress: read_owned(&store, EPISODE_PROGRESS_KEY)?,
                continue_hidden: read_owned(&store, CONTINUE_HIDDEN_KEY)?,
                addons: read_owned(&store, "addons")?,
                plugins: {
                    addon_packages::validate_owned(&store)?;
                    plugin_settings::prepare_backend(store.clone())?
                },
            };
            sync::prepare_activation_storage(
                &store,
                &data.settings,
                &data.entries,
                &data.progress,
                &data.addons,
            )
            .map_err(|error| error.to_string())?;
            // Pending headless records are materialized before the final DTO
            // is read. These operations cannot acquire a live UI allocation.
            data.settings = match store.read("settings").map_err(|error| error.to_string())? {
                Some(raw) => recover_settings(
                    serde_json::from_str(&raw)
                        .map_err(|error| format!("Unreadable settings: {error}"))?,
                )
                .ok_or("Unreadable settings object retained")?,
                None => CacheSettings::default(),
            };
            data.settings = sync::prepare_playback_defaults(&data.settings)
                .map_err(|error| error.to_string())?;
            data.entries = read_owned(&store, "library")?;
            data.progress = read_owned(&store, EPISODE_PROGRESS_KEY)?;
            data.continue_hidden = read_owned(&store, CONTINUE_HIDDEN_KEY)?;
            data.addons = read_owned(&store, "addons")?;
            watch_state::prepare_activation_activity(&store, &data.entries, &data.progress)
                .map_err(|error| error.to_string())?;
            sync::prepare_activation_aliases(&store, &data.entries, &mut data.progress)
                .map_err(|error| error.to_string())?;
            last_stream::prepare_activation(last_stream).map_err(|error| error.to_string())?;
            data.plugins = plugin_settings::prepare_backend(store.clone())?;
            data.policy = profile_policy::load(&store)?;
            Ok(data)
        })
    }

    fn into_prepared(self, bridge: &Bridge, id: &str) -> PreparedProfile {
        let state = Shared {
            chosen_addon: usize::MAX,
            restoring_models: true,
            cache_settings: self.settings,
            download_settings: self.download_settings,
            entries: self.entries,
            progress: self.progress,
            continue_hidden: self.continue_hidden,
            ..Shared::default()
        };
        let session = bridge.sessions.prepare(id, state);
        *session.applied_policy.lock().unwrap() = self.policy;
        PreparedProfile {
            session,
            addons: self.addons,
            plugins: self.plugins,
        }
    }
}

struct PreparedProfile {
    session: Arc<profile_session::ProfileSession>,
    addons: Vec<AddonStore>,
    plugins: nova_providers::plugin_settings::PreparedPluginSettings,
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<PreparedProfileData>();
};

#[derive(Clone, PartialEq)]
struct SourceStamp {
    admission: u64,
    entries: Vec<LibraryEntry>,
    progress: HashMap<String, EpisodeProgress>,
    settings: CacheSettings,
    preferences: HashMap<String, Option<nova_config::playback_preferences::PreferenceValue>>,
    playback: Option<(u64, f64, f64)>,
    journal: Option<(String, u64)>,
}
impl SourceStamp {
    fn matches(&self, state: &Shared) -> bool {
        self.entries == state.entries
            && self.progress == state.progress
            && self.settings == state.cache_settings
            && self.preferences == state.playback_preference_edits
            && self.playback
                == state
                    .playback
                    .as_ref()
                    .map(|target| (target.session, target.last_pos, target.last_dur))
            && self.journal == state.viewing_journal.transition_revision()
    }
}

#[derive(Clone)]
struct SelectionRequest {
    target: String,
    require_selection: bool,
    continuation: Option<ActivationContinuation>,
}
type ActivationContinuation =
    Arc<Mutex<Option<Box<dyn FnOnce(&Bridge, profiles::Context) + Send>>>>;

#[derive(Clone)]
struct ActivationRequest {
    generation: u64,
    selection: SelectionRequest,
    source: profiles::Context,
    stamp: SourceStamp,
    source_policy: String,
    target_policy: String,
    was_blocked: bool,
    source_owner: profiles::ProfileStore,
    target_owner: profiles::ProfileStore,
    last_stream: Arc<last_stream::Cache>,
}

type ActivationData = (PreparedProfileData, profiles::ActivationBasis);
impl ActivationRequest {
    fn prepare_data(&self) -> Result<ActivationData, String> {
        for _ in 0..8 {
            let before = self.target_owner.revision_token().read();
            if !before.is_multiple_of(2) {
                // Wait for an in-progress writer on the coordinator, never
                // burn retries or spin on the event loop.
                drop(profiles::WRITE_SERIAL.lock().unwrap());
                continue;
            }
            let clock_before = nova_sync::local_store()
                .map_err(|error| error.to_string())?
                .lock()
                .unwrap()
                .clock();
            let basis_before = profiles::activation_basis(&self.selection.target)?;
            let data = PreparedProfileData::load(&self.selection.target, &self.last_stream)?;
            let basis = profiles::activation_basis(&self.selection.target)?;
            let after = self.target_owner.revision_token().read();
            let clock = nova_sync::local_store()
                .map_err(|error| error.to_string())?
                .lock()
                .unwrap()
                .clock();
            if before == after
                && after.is_multiple_of(2)
                && basis_before == basis
                && clock_before == clock
            {
                let source_revision = self.source_owner.revision_token().read();
                if !source_revision.is_multiple_of(2) {
                    drop(profiles::WRITE_SERIAL.lock().unwrap());
                    continue;
                }
                if profile_policy::revision(&self.source_owner) != self.source_policy
                    || profile_policy::revision(&self.target_owner) != self.target_policy
                {
                    return Err("Profile authorization changed".into());
                }
                if self.source_owner.revision_token().read() != source_revision {
                    continue;
                }
                return Ok((
                    data,
                    profiles::ActivationBasis {
                        snapshots: basis,
                        revision: after,
                        clock,
                        source_revision,
                    },
                ));
            }
        }
        Err("Incoming profile changed during preparation".into())
    }
}

type ActivationPhase = Box<dyn FnOnce() + Send + 'static>;
struct ActivationWorker {
    sender: Option<std::sync::mpsc::SyncSender<ActivationPhase>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl ActivationWorker {
    fn spawn() -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ActivationPhase>(1);
        let thread = std::thread::Builder::new()
            .name("nova-profile-transition".into())
            .spawn(move || {
                while let Ok(phase) = receiver.recv() {
                    phase();
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }
}
impl Drop for ActivationWorker {
    fn drop(&mut self) {
        // Closing the sole sender drains the one admitted phase before receive
        // disconnects. No previous phase has a detached OS worker to outlive it.
        self.sender.take();
        if let Some(worker) = self.thread.take()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
pub(super) struct ActivationCoordinator {
    generation: u64,
    current: Option<Arc<ActivationRequest>>,
    latest: Option<SelectionRequest>,
    sealed: bool,
    cancelled: bool,
    closed: bool,
    worker: Option<ActivationWorker>,
    pending_continuation: Option<(String, ActivationContinuation)>,
    current_continuation: Option<ActivationContinuation>,
    published: Option<profiles::Context>,
    #[cfg(test)]
    waiting: bool,
}

impl Bridge {
    fn enqueue_activation_phase(&self, phase: impl FnOnce() + Send + 'static) {
        let result = {
            let mut coordinator = self.profile_activation.lock().unwrap();
            if coordinator.closed {
                Err("Profile navigation is shutting down".into())
            } else {
                if coordinator.worker.is_none() {
                    match ActivationWorker::spawn() {
                        Ok(worker) => coordinator.worker = Some(worker),
                        Err(error) => {
                            drop(coordinator);
                            self.profile_activation_finished(Some(error.to_string()));
                            return;
                        }
                    }
                }
                coordinator
                    .worker
                    .as_ref()
                    .unwrap()
                    .sender
                    .as_ref()
                    .unwrap()
                    .try_send(Box::new(phase))
                    .map_err(|_| "Profile transition coordinator unavailable".to_string())
            }
        };
        if let Err(error) = result {
            self.profile_activation_finished(Some(error));
        }
    }

    fn activation_stamp(&self) -> SourceStamp {
        let state = self.shared.lock().unwrap();
        SourceStamp {
            admission: persistence::admission_revision(),
            entries: state.entries.clone(),
            progress: state.progress.clone(),
            settings: state.cache_settings.clone(),
            preferences: state.playback_preference_edits.clone(),
            playback: state
                .playback
                .as_ref()
                .map(|target| (target.session, target.last_pos, target.last_dur)),
            journal: state.viewing_journal.transition_revision(),
        }
    }

    fn capture_profile_transition(
        &self,
        source: &profiles::Context,
    ) -> Result<Option<persistence::TransitionWait>, String> {
        let loaded = self.profile_loaded_epoch.load(Ordering::Acquire) == source.epoch;
        if !loaded || !profiles::writable(&source.id) {
            return Ok(None);
        }
        // Reserve all transition capacity before authoritative capture edits.
        let _reservation =
            persistence::reserve(1).ok_or("persistence queue unavailable; edit not accepted")?;
        let progress = self.capture_profile_progress_patch();
        let journal = self.capture_profile_journal();
        let state = self.shared.lock().unwrap();
        let capture = OutgoingTransition {
            progress,
            settings: settings::capture_local_settings(&state.cache_settings),
            torrent: self.capture_torrent_edits(),
            preferences: state.playback_preference_edits.clone(),
            journal,
        };
        let has_progress = capture.progress.is_some();
        drop(state);
        let wait = persistence::enqueue_transition(capture)?;
        if has_progress && wait.queued() {
            let state = self.shared.lock().unwrap();
            unified_search::progress_admitted(&profiles::ProfileStore::capture(), &state.progress);
            progress_projection::invalidate();
        }
        Ok(Some(wait))
    }

    fn begin_profile_activation(&self, id: &str, require_selection: bool) -> Result<(), String> {
        let selection = {
            let mut coordinator = self.profile_activation.lock().unwrap();
            let continuation = coordinator
                .pending_continuation
                .take()
                .filter(|(target, _)| target == id)
                .map(|(_, continuation)| continuation);
            let selection = SelectionRequest {
                target: id.into(),
                require_selection,
                continuation,
            };
            if coordinator.closed {
                return Err("Profile navigation is shutting down".into());
            }
            if let Some(current) = &coordinator.current {
                if current.selection.target != id
                    || current.selection.require_selection != require_selection
                {
                    coordinator.current_continuation = None;
                    coordinator.latest = Some(selection);
                } else {
                    coordinator.latest = None;
                    if !coordinator.sealed {
                        coordinator.cancelled = false;
                    }
                    if selection.continuation.is_some() {
                        coordinator.current_continuation = selection.continuation;
                    }
                }
                return Ok(());
            }
            selection
        };
        let source = profiles::active_context();
        let loaded = self.profile_loaded_epoch.load(Ordering::Acquire) == source.epoch;
        if loaded && source.id == id {
            self.finish_profile_selection(require_selection);
            self.profile_activation_idle();
            if let Some(continuation) = selection.continuation
                && let Some(continuation) = continuation.lock().unwrap().take()
            {
                continuation(self, profiles::active_context());
            }
            return Ok(());
        }
        let wait = self.capture_profile_transition(&source)?;
        let watermark = persistence::watermark();
        let request = {
            let mut coordinator = self.profile_activation.lock().unwrap();
            coordinator.generation += 1;
            coordinator.current_continuation = selection.continuation.clone();
            coordinator.published = None;
            let request = Arc::new(ActivationRequest {
                generation: coordinator.generation,
                source_policy: profile_policy::revision(&profiles::ProfileStore::for_profile(
                    &source.id,
                )),
                target_policy: profile_policy::revision(&profiles::ProfileStore::for_profile(id)),
                source,
                selection,
                stamp: self.activation_stamp(),
                was_blocked: profile_policy::entry_blocked(),
                source_owner: profiles::ProfileStore::for_profile(&profiles::active_context().id),
                target_owner: profiles::ProfileStore::for_profile(id),
                last_stream: self.last_stream.clone(),
            });
            coordinator.current = Some(request.clone());
            coordinator.sealed = false;
            coordinator.cancelled = false;
            request
        };
        if let Some(app) = self.app() {
            app.set_profiles_busy(true);
            app.set_profiles_error("".into());
        }
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            let result = (|| {
                if let Some(wait) = wait {
                    wait.wait()?;
                }
                watermark.wait()?;
                request.prepare_data()
            })();
            let _ =
                profiles::invoke_account(move || bridge.profile_activation_ready(request, result));
        });
        Ok(())
    }

    fn profile_activation_finished(&self, error: Option<String>) {
        let (next, continuation, published) = {
            let mut coordinator = self.profile_activation.lock().unwrap();
            coordinator.current = None;
            coordinator.sealed = false;
            coordinator.cancelled = false;
            let next = coordinator.latest.take().filter(|_| !coordinator.closed);
            let continuation = coordinator
                .current_continuation
                .take()
                .filter(|_| next.is_none() && error.is_none() && !coordinator.closed);
            (next, continuation, coordinator.published.take())
        };
        if let (Some(continuation), Some(context)) = (continuation, published)
            && profiles::active_context() == context
            && let Some(continuation) = continuation.lock().unwrap().take()
        {
            continuation(self, context);
        }
        if let Some(error) = error {
            self.profile_error(error);
        }
        if let Some(next) = next {
            if let Some(continuation) = next.continuation {
                self.profile_activation.lock().unwrap().pending_continuation =
                    Some((next.target.clone(), continuation));
            }
            if !next.require_selection && !self.gate_profile_selection(&next.target) {
                self.profile_activation_idle();
            } else if let Err(error) = self.activate_profile(&next.target, next.require_selection) {
                self.profile_error(error);
                self.profile_activation_idle();
            }
            if self.profile_activation.lock().unwrap().current.is_none() {
                self.profile_activation_idle();
            }
        } else {
            self.profile_activation_idle();
        }
    }

    fn profile_activation_idle(&self) {
        if self.profile_activation.lock().unwrap().current.is_some() {
            return;
        }
        if let Some(app) = self.app() {
            app.set_profiles_busy(false);
        }
        #[cfg(test)]
        if std::mem::take(&mut self.profile_activation.lock().unwrap().waiting) {
            let _ = slint::quit_event_loop();
        }
    }

    fn profile_activation_ready(
        &self,
        request: Arc<ActivationRequest>,
        result: Result<ActivationData, String>,
    ) {
        let obsolete = {
            let coordinator = self.profile_activation.lock().unwrap();
            coordinator.closed
                || coordinator
                    .current
                    .as_ref()
                    .is_none_or(|current| current.generation != request.generation)
                || coordinator.latest.is_some()
                || coordinator.cancelled
        };
        if obsolete {
            self.profile_activation_finished(None);
            return;
        }
        let (data, basis) = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                profile_policy::set_entry_blocked(request.was_blocked);
                self.profile_activation_finished(Some(error));
                return;
            }
        };
        if profiles::active_context() != request.source {
            self.profile_activation_finished(None);
            return;
        }
        if request.source_owner.revision_token().read() != basis.source_revision
            || request.target_owner.revision_token().read() != basis.revision
        {
            self.restart_activation_preparation(request, None);
            return;
        }
        if self.activation_stamp() != request.stamp {
            let wait = match self.capture_profile_transition(&request.source) {
                Ok(wait) => wait,
                Err(error) => {
                    self.profile_activation_finished(Some(error));
                    return;
                }
            };
            let mut refreshed = (*request).clone();
            refreshed.stamp = self.activation_stamp();
            let refreshed = Arc::new(refreshed);
            self.profile_activation.lock().unwrap().current = Some(refreshed.clone());
            self.restart_activation_preparation(refreshed, wait);
            return;
        }
        // Sealing is the cancellation boundary. Later requests occupy one
        // replacement slot and cannot obsolete a durable selected-profile write.
        self.profile_activation.lock().unwrap().sealed = true;
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            let receipt = profiles::prepare_activation_checked(
                &request.selection.target,
                &request.source,
                &basis,
                &request.source_policy,
                &request.target_policy,
            );
            let _ = profiles::invoke_account(move || {
                bridge.profile_activation_committed(request, data, receipt)
            });
        });
    }

    fn restart_activation_preparation(
        &self,
        request: Arc<ActivationRequest>,
        wait: Option<persistence::TransitionWait>,
    ) {
        let watermark = persistence::watermark();
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            let result = (|| {
                if let Some(wait) = wait {
                    wait.wait()?;
                }
                watermark.wait()?;
                request.prepare_data()
            })();
            let _ =
                profiles::invoke_account(move || bridge.profile_activation_ready(request, result));
        });
    }

    #[cfg(test)]
    pub(super) fn wait_profile_activation(&self) {
        {
            let mut coordinator = self.profile_activation.lock().unwrap();
            if coordinator.current.is_none() {
                return;
            }
            coordinator.waiting = true;
        }
        slint::run_event_loop().unwrap();
        assert!(self.profile_activation.lock().unwrap().current.is_none());
    }

    pub(super) fn close_profile_activations(&self) {
        self.close_sync_projections();
        let worker = {
            let mut coordinator = self.profile_activation.lock().unwrap();
            coordinator.closed = true;
            coordinator.latest = None;
            coordinator.pending_continuation = None;
            coordinator.current_continuation = None;
            coordinator.worker.take()
        };
        // A sealed request owns its durable receipt. Its completion is never
        // reinterpreted as cancellation or replaced with an unrelated request.
        // Called after the event loop exits. No UI callback waits on this join.
        drop(worker);
    }
}

impl Bridge {
    fn profile_error(&self, error: String) {
        tracing::warn!(%error, "profile operation failed");
        if let Some(app) = self.app() {
            app.set_profiles_busy(false);
            let message = match error.as_str() {
                "Keep at least one profile."
                | "This profile is unavailable."
                | "Enter a profile name of up to 64 characters." => text::tr(&error),
                _ => text::tr("Could not save profile. Try again."),
            };
            app.set_profiles_error(message.into());
        }
    }

    pub(super) fn refresh_profiles(&self) {
        if !profiles::is_initialized() {
            return;
        }
        let rows = match profiles::list() {
            Ok(rows) => rows,
            Err(error) => {
                self.profile_error(error);
                return;
            }
        };
        self.publish_prepared_profiles(
            &rows,
            profiles::device_preferences().open_last_profile_on_startup,
        );
    }
    pub(super) fn publish_prepared_profiles(
        &self,
        rows: &[profiles::Profile],
        remember_last: bool,
    ) {
        let active = profiles::active_context().id;
        if let Some(app) = self.app() {
            app.set_profiles_remember_last(remember_last);
            app.set_profiles(
                Rc::new(VecModel::from(
                    rows.iter()
                        .map(|row| profile_row(row, &active))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
        if !rows.iter().any(|row| row.id == active) {
            // Deletion can arrive inside sync projection. Finish that batch in
            // its captured context before activating a surviving profile.
            if self.profile_recovery_pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let replacement = rows
                .first()
                .map(|row| row.id.clone())
                .map(Ok)
                .unwrap_or_else(|| profiles::create("Default"));
            match replacement {
                Ok(id) => {
                    if let Some(app) = self.app() {
                        app.set_profiles_require_selection(true);
                    }
                    let bridge = self.clone();
                    let _ = profiles::invoke_account(move || {
                        bridge
                            .profile_recovery_pending
                            .store(false, Ordering::Release);
                        if let Err(error) = bridge.activate_profile(&id, true) {
                            bridge.profile_error(error);
                        }
                    });
                }
                Err(error) => {
                    self.profile_recovery_pending
                        .store(false, Ordering::Release);
                    self.profile_error(error);
                }
            }
        }
    }

    pub(super) fn create_profile(&self, name: &str) {
        if !self.gate_profile_create(name) {
            return;
        }
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::create(name) {
            Ok(_) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn rename_profile(&self, id: &str, name: &str) {
        if !profiles::valid_id(id) || !self.gate_profile_rename(id, name) {
            return;
        }
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::rename(id, name) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn delete_profile(&self, id: &str) {
        if !profiles::valid_id(id) || !self.gate_profile_delete(id) {
            return;
        }
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        if profiles::current_id() == id {
            self.cast_shutdown();
        }
        match profiles::delete(id) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn switch_profile(&self, id: &str) {
        {
            let mut coordinator = self.profile_activation.lock().unwrap();
            if coordinator
                .pending_continuation
                .as_ref()
                .is_some_and(|(target, _)| target != id)
            {
                coordinator.pending_continuation = None;
            }
            if coordinator
                .current
                .as_ref()
                .is_some_and(|current| current.selection.target != id)
            {
                coordinator.current_continuation = None;
                if !coordinator.sealed {
                    coordinator.cancelled = true;
                }
            }
        }
        if !profiles::valid_id(id) {
            self.profile_activation.lock().unwrap().pending_continuation = None;
            return;
        }
        if !self.gate_profile_selection(id) {
            if !self
                .app()
                .is_some_and(|app| app.global::<crate::MoreBridge>().get_pin_open())
            {
                self.profile_activation.lock().unwrap().pending_continuation = None;
            }
            return;
        }
        self.activate_profile_authorized(id);
    }

    #[cfg(any(test, target_os = "android"))]
    pub(super) fn switch_profile_after(
        &self,
        id: &str,
        continuation: impl FnOnce(&Bridge, profiles::Context) + Send + 'static,
    ) {
        self.profile_activation.lock().unwrap().pending_continuation = Some((
            id.into(),
            Arc::new(Mutex::new(Some(Box::new(continuation)))),
        ));
        self.switch_profile(id);
    }

    #[cfg(target_os = "android")]
    pub(super) fn profile_continuation_pending(&self, id: &str) -> bool {
        let coordinator = self.profile_activation.lock().unwrap();
        coordinator
            .pending_continuation
            .as_ref()
            .is_some_and(|(target, _)| target == id)
            || (coordinator
                .current
                .as_ref()
                .is_some_and(|request| request.selection.target == id)
                && coordinator.current_continuation.is_some())
            || coordinator
                .latest
                .as_ref()
                .is_some_and(|request| request.target == id && request.continuation.is_some())
    }

    pub(super) fn activate_profile_authorized(&self, id: &str) {
        self.finish_profile_management_for_selection();
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        let was_blocked = profile_policy::entry_blocked();
        if let Err(error) = self.activate_profile(id, false) {
            self.profile_activation.lock().unwrap().pending_continuation = None;
            profile_policy::set_entry_blocked(was_blocked);
            self.profile_error(error);
        }
    }

    pub(super) fn remember_profile_on_startup(&self, enabled: bool) {
        if let Some(app) = self.app() {
            app.set_profiles_error("".into());
        }
        match profiles::set_open_last_profile_on_startup(enabled) {
            Ok(()) => self.refresh_profiles(),
            Err(error) => self.profile_error(error),
        }
    }

    pub(super) fn activate_profile(&self, id: &str, require_selection: bool) -> Result<(), String> {
        let source = profiles::active_context();
        if source.id == id && self.profile_loaded_epoch.load(Ordering::Acquire) != source.epoch {
            self.sessions.active().block_authoritative_edits();
        }
        if !profiles::list()?.iter().any(|profile| profile.id == id) {
            return Err("This profile is unavailable.".into());
        }
        // Recovery/import may request a picker for a protected target. Keep
        // its durable snapshots detached until an actual PIN-authorized choice.
        if require_selection
            && profile_policy::load(&profiles::ProfileStore::for_profile(id))
                .map_or(true, |p| p.require_pin_on_entry)
        {
            profile_policy::set_entry_blocked(true);
            self.cast_shutdown();
            self.cancel_pending_playback();
            self.cancel_sleep_timer();
            self.player.close();
            self.finish_playback_request();
            self.shutdown_tracking();
            self.shutdown_stremio();
            self.shutdown_list_services();
            self.sessions
                .active()
                .services_requested
                .store(false, Ordering::Release);
            self.profile_loaded_epoch.store(0, Ordering::Release);
            if let Some(app) = self.app() {
                app.set_profiles_require_selection(true);
                app.set_profiles_open(true);
                app.set_more_open(true);
                app.set_show_home(false);
            }
            return Ok(());
        }
        self.begin_profile_activation(id, require_selection)
    }

    fn profile_activation_committed(
        &self,
        request: Arc<ActivationRequest>,
        data: PreparedProfileData,
        receipt: Result<profiles::PreparedActivation, String>,
    ) {
        let incoming = match receipt {
            Ok(receipt) => receipt,
            Err(error) => {
                profile_policy::set_entry_blocked(request.was_blocked);
                self.profile_activation_finished(Some(error));
                return;
            }
        };
        if profiles::active_context() != request.source {
            self.fail_sealed_activation(request, "Profile activation source changed".into());
            return;
        }
        let validation = incoming
            .validation
            .expect("sealed activation requires validation receipt");
        if request.target_owner.revision_token().read() != validation.target
            || request.source_owner.revision_token().read() != validation.source
        {
            self.reprepare_sealed_activation(request, incoming, None);
            return;
        }
        if self.profile_loaded_epoch.load(Ordering::Acquire) == request.source.epoch
            && self.activation_stamp() != request.stamp
        {
            let wait = match self.capture_profile_transition(&request.source) {
                Ok(wait) => wait,
                Err(error) => {
                    self.fail_sealed_activation(request, error);
                    return;
                }
            };
            let mut refreshed = (*request).clone();
            refreshed.stamp = self.activation_stamp();
            let refreshed = Arc::new(refreshed);
            self.profile_activation.lock().unwrap().current = Some(refreshed.clone());
            self.reprepare_sealed_activation(refreshed, incoming, wait);
            return;
        }
        let owner = match nova_sync::local_store() {
            Ok(owner) => owner,
            Err(error) => {
                self.fail_sealed_activation(request, error.to_string());
                return;
            }
        };
        let Ok(store) = owner.try_lock() else {
            self.defer_activation_publication(request, data, incoming);
            return;
        };
        let Ok(serial) = profiles::WRITE_SERIAL.try_lock() else {
            drop(store);
            self.defer_activation_publication(request, data, incoming);
            return;
        };
        let Ok(mut source_state) = self.shared.try_lock() else {
            drop(serial);
            drop(store);
            self.defer_activation_publication(request, data, incoming);
            return;
        };
        let id = &request.selection.target;
        let unchanged = store.clock() == validation.clock
            && request.target_owner.revision_token().read() == validation.target
            && request.source_owner.revision_token().read() == validation.source
            && validation.target % 2 == 0
            && validation.source % 2 == 0
            && profiles::active_context() == request.source
            && store
                .record(profiles::DOMAIN, id)
                .is_some_and(|record| !record.is_deleted())
            && store
                .record(profiles::DELETIONS_DOMAIN, id)
                .is_none_or(|record| record.is_deleted());
        if !unchanged {
            drop(source_state);
            drop(serial);
            drop(store);
            self.reprepare_sealed_activation(request, incoming, None);
            return;
        }
        if self.profile_loaded_epoch.load(Ordering::Acquire) == request.source.epoch
            && !request.stamp.matches(&source_state)
        {
            drop(source_state);
            drop(serial);
            drop(store);
            self.defer_activation_publication(request, data, incoming);
            return;
        }
        // No storage read, persistence callback, provider callback or player
        // teardown is permitted inside this Store -> serializer boundary.
        for (key, value) in &request.stamp.preferences {
            if source_state.playback_preference_edits.get(key) == Some(value) {
                source_state.playback_preference_edits.remove(key);
            }
        }
        if source_state.playback_preference_edits.is_empty() {
            source_state.playback_preference_save_at = None;
        }
        let prepared = data.into_prepared(self, id);
        self.sessions.publish(prepared.session.clone());
        profiles::commit_session(incoming, &prepared.session);
        drop(source_state);
        drop(serial);
        drop(store);
        self.profile_activation.lock().unwrap().published = Some(profiles::active_context());
        self.catalog_gen.fetch_add(1, Ordering::AcqRel);
        self.poster_intake.invalidate();
        self.home_showcase_gen.fetch_add(1, Ordering::AcqRel);
        self.home_catalog_rows_gen.fetch_add(1, Ordering::AcqRel);
        // Retained source callbacks keep their allocation even after runtime
        // publication. Cleanup stays attributed to that source on the UI.
        profiles::with_context(request.source.clone(), || {
            self.cast_shutdown();
            self.cancel_season_download();
            self.shared.lock().unwrap().catalog_detail.clear();
            self.party_leave();
            self.remote_disconnect();
            self.clear_addon_subtitles();
            #[cfg(target_os = "android")]
            android_player::player_brightness_reset();
            self.cancel_sleep_timer();
            self.finish_playback_request();
            self.player.close();
            self.shutdown_tracking();
            self.shutdown_stremio();
            self.shutdown_list_services();
        });
        // Provider locks may call owner backends. Never take them while holding
        // WRITE_SERIAL; incoming services start only after this UI turn finishes.
        nova_providers::plugin_settings::activate_plugin_settings(prepared.plugins);
        let _ = prepared
            .session
            .providers
            .set(nova_providers::plugin_settings::capture_settings_capability());
        profile_policy::set_entry_blocked(false);
        if let Some(app) = self.app() {
            app.set_profiles_require_selection(true);
            app.set_profiles_open(true);
            app.set_more_open(true);
        }

        profiles::with_context(profiles::active_context(), || {
            self.load_profile(prepared.addons);
            self.finish_profile_selection(request.selection.require_selection);
        });
        self.profile_activation_finished(None);
    }

    fn fail_sealed_activation(&self, request: Arc<ActivationRequest>, error: String) {
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            let repair =
                profiles::restore_activation_selection(&request.selection.target, &request.source);
            let error = repair.err().map_or(error.clone(), |repair| {
                format!("{error}; selection recovery failed: {repair}")
            });
            let _ = profiles::invoke_account(move || {
                profile_policy::set_entry_blocked(request.was_blocked);
                bridge.profile_activation_finished(Some(error));
                bridge.refresh_profiles();
            });
        });
    }

    fn defer_activation_publication(
        &self,
        request: Arc<ActivationRequest>,
        data: PreparedProfileData,
        incoming: profiles::PreparedActivation,
    ) {
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            // Await contention away from UI, without retaining either lock
            // while waiting for source state. The UI rechecks every condition.
            if let Ok(owner) = nova_sync::local_store() {
                let store = owner.lock().unwrap();
                let serial = profiles::WRITE_SERIAL.lock().unwrap();
                drop(serial);
                drop(store);
            }
            if let Some(session) = &request.source.session {
                session.wait_for_state_boundary();
            }
            let _ = profiles::invoke_account(move || {
                bridge.profile_activation_committed(request, data, Ok(incoming))
            });
        });
    }

    fn reprepare_sealed_activation(
        &self,
        request: Arc<ActivationRequest>,
        mut incoming: profiles::PreparedActivation,
        wait: Option<persistence::TransitionWait>,
    ) {
        let bridge = self.clone();
        self.enqueue_activation_phase(move || {
            let result: Result<_, String> = (|| {
                if let Some(wait) = wait {
                    wait.wait()?;
                }
                let (data, basis) = request.prepare_data()?;
                incoming.validation = Some(profiles::revalidate_activation(
                    &request.selection.target,
                    &request.source,
                    &basis,
                    &request.source_policy,
                    &request.target_policy,
                )?);
                Ok((data, incoming))
            })();
            match result {
                Ok((data, incoming)) => {
                    let _ = profiles::invoke_account(move || {
                        bridge.profile_activation_committed(request, data, Ok(incoming))
                    });
                }
                Err(error) => {
                    let repair = profiles::restore_activation_selection(
                        &request.selection.target,
                        &request.source,
                    );
                    let error = repair.err().map_or(error.clone(), |repair| {
                        format!("{error}; selection recovery failed: {repair}")
                    });
                    let _ = profiles::invoke_account(move || {
                        profile_policy::set_entry_blocked(request.was_blocked);
                        bridge.profile_activation_finished(Some(error));
                        bridge.refresh_profiles();
                    });
                }
            }
        });
    }

    fn load_profile(&self, addons: Vec<AddonStore>) {
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        set_active_cache_settings(settings.clone());
        persistence::register_collections();
        self.settings_to_ui();
        self.load_search_history();
        *self.image_usage_scan.lock().unwrap() = Default::default();
        *self.torrent_usage_scan.lock().unwrap() = Default::default();
        self.clear_streams();
        if let Some(app) = self.app() {
            app.set_catalog(Default::default());
            app.set_addon_zip_import_busy(false);
            app.set_addon_zip_import_status("".into());
            // Directory controls are transient personal view state. Clear the
            // shared UI globals before publishing the incoming inventory.
            app.set_addon_browser_tab(0);
            app.set_addon_browser_source(0);
            app.set_addon_browser_directory_url("".into());
            app.set_addon_browser_search("".into());
            app.set_addon_browser_filter(0);
            app.set_addon_browser_rows(Default::default());
            app.set_addon_browser_source_name("".into());
            app.set_addon_browser_source_url("".into());
            app.set_addon_browser_status("".into());
            app.set_addon_browser_busy(false);
            app.set_addon_config_open(false);
            app.set_addon_config_url("".into());
            app.set_addon_config_label("".into());
            app.set_plugin_settings_notice("".into());
            app.set_completion_prompt_open(false);
            app.set_tracking_open(false);
            app.set_tracking_notice("".into());
            app.set_tracking_accounts(Default::default());
            app.set_tracking_links(Default::default());
            app.set_tracking_candidates(Default::default());
            app.set_tracking_setup(Default::default());
            app.set_tracking_setup_active(false);
            app.set_tracking_source_rows(Default::default());
            app.set_tracking_preview_rows(Default::default());
            app.set_tracking_mapping_rows(Default::default());
            app.set_tracking_busy(false);
            app.set_tracking_can_confirm(false);
            app.set_stremio_connected(false);
            app.set_stremio_mode_index(0);
            app.set_stremio_title_busy(false);
            app.set_stremio_title_linked(false);
            app.set_stremio_title_can_confirm(false);
            app.set_stremio_title_status("".into());
            app.set_stremio_title_match("".into());
            app.set_stremio_title_candidates(Default::default());
            app.set_stremio_title_mapping(Default::default());
            app.set_stremio_title_options(Default::default());
            list_services::clear_ui(&app);
            app.set_stremio_busy(false);
            app.set_stremio_account("".into());
            app.set_stremio_status("".into());
            app.set_stremio_link_code("".into());
            app.set_stremio_link_url("".into());
            app.set_stremio_preview("".into());
            app.set_stremio_pending("".into());
            app.set_stremio_merge_ready(false);
            app.set_modal_visible(false);
            app.set_selected_details(Default::default());
            app.set_stream_selector_open(false);
            app.set_season_download_active(false);
            app.set_season_download_review(false);
            app.set_season_download_status(Default::default());
            app.set_season_download_rows(Default::default());
            app.global::<nova_ui_shared::DetailBridge>()
                .set_season_download_menu(-1);
            app.set_categories_modal(false);
            app.set_discover_search_open(false);
            app.set_discover_search_focused(false);
            app.set_search_scope(unified_search::DEFAULT_SCOPE);
            app.set_library_search_open(false);
            // Search text and category filter belong to the previous profile.
            app.set_library_query(SharedString::default());
            app.set_library_filter_category(SharedString::default());
            app.set_library_view_loading(false);
            app.set_party_page_open(false);
            app.set_remote_controls_open(false);
            app.set_remote_devices_open(false);
        }
        self.shared.lock().unwrap().loading_addons = true;
        plugin_cache::start_gc_worker();
        addon_packages::initialize(self);
        for addon in addons {
            nova_providers::configure_provider_capabilities(
                &addon.url,
                addon.capabilities.unwrap_or_default(),
            );
            self.install_persisted(
                &addon.url,
                addon.enabled,
                addon.configure_ok,
                Some(addon.label),
            );
        }
        self.shared.lock().unwrap().loading_addons = false;
        self.refresh_all(true);
        addon_packages::reconcile(self);
        // Storage migrations, seed/replay and alias reconciliation completed
        // in the detached preparation phase before this session was accepted.
        self.remote_control_policy_changed(settings.remote_control_enabled);
        self.shared.lock().unwrap().restoring_models = false;
        self.apply_library_to_ui();
        self.refresh_watch_queue();
        self.refresh_bookmarks();
        self.refresh_viewing_journal();
        self.refresh_playback_tools();
        self.profile_loaded_epoch
            .store(profiles::active_context().epoch, Ordering::Release);
        self.refresh_profiles();
    }

    fn finish_profile_selection(&self, require_selection: bool) {
        if !require_selection {
            profile_policy::set_entry_blocked(false);
            profile_policy::remember_entry(&profiles::ProfileStore::capture());
        }
        if !require_selection {
            self.sessions
                .active()
                .services_requested
                .store(true, Ordering::Release);
            self.retry_profile_services();
        }
        if let Some(app) = self.app() {
            app.global::<crate::SettingsSync>().set_open(false);
            app.global::<crate::JournalBridge>().set_open(false);
            self.invalidate_last_time();
            app.global::<crate::QueueBridge>().set_open(false);
            let bookmarks = app.global::<crate::BookmarksBridge>();
            bookmarks.set_open(false);
            bookmarks.set_title_id("".into());
            bookmarks.set_title_caption("".into());
            bookmarks.set_query("".into());
            let from_picker = app.get_profiles_open();
            app.set_profiles_require_selection(require_selection);
            app.set_profiles_open(require_selection);
            app.set_show_calendar(false);
            app.set_show_downloads(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_profiles_busy(false);
            self.show_home_page();
            if require_selection {
                app.set_show_home(false);
                app.set_profiles_open(true);
                app.set_more_open(true);
            } else {
                app.set_more_open(false);
                if from_picker {
                    app.invoke_animate_profile_home_entry();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_transition_keeps_committed_remote_settings_before_ui_receipt() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::outgoing_transition_keeps_committed_remote_settings_before_ui_receipt"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let source = profiles::active_context();
                let owner = profiles::ProfileStore::capture();
                let stale = bridge.shared.lock().unwrap().cache_settings.clone();
                let mut remote = stale.clone();
                remote.spoiler_protection = !stale.spoiler_protection;
                remote.playback_speed = 1.75;
                owner
                    .persist_snapshot("settings", &serde_json::to_string(&remote).unwrap(), false)
                    .unwrap();
                // The writer has committed the incoming batch, but its UI
                // receipt has not replaced this allocation's old Shared.
                assert_eq!(bridge.shared.lock().unwrap().cache_settings, stale);
                let durable = owner.read("settings").unwrap();
                bridge
                    .capture_profile_transition(&source)
                    .unwrap()
                    .unwrap()
                    .wait()
                    .unwrap();
                assert_eq!(
                    owner.read("settings").unwrap(),
                    durable,
                    "navigation is not a settings edit"
                );
                assert_eq!(bridge.shared.lock().unwrap().cache_settings, stale);
                // A real local field edit now rebases against that same remote
                // commit, preserving its unrelated field in the coupled data.
                bridge.shared.lock().unwrap().cache_settings.playback_speed = 1.25;
                bridge
                    .capture_profile_transition(&source)
                    .unwrap()
                    .unwrap()
                    .wait()
                    .unwrap();
                let saved: CacheSettings =
                    serde_json::from_str(&owner.read("settings").unwrap().unwrap()).unwrap();
                assert_eq!(saved.playback_speed, 1.25);
                assert_eq!(saved.spoiler_protection, remote.spoiler_protection);
                assert!(
                    source
                        .session
                        .unwrap()
                        .settings_edits
                        .lock()
                        .unwrap()
                        .pending()
                        .is_empty()
                );
            },
        );
    }

    #[test]
    fn forced_same_owner_reload_rejects_stale_library_and_progress_gestures() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::forced_same_owner_reload_rejects_stale_library_and_progress_gestures"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let source = profiles::active_context();
                let owner = profiles::ProfileStore::for_profile(&source.id);
                let stale_entry: LibraryEntry = serde_json::from_value(serde_json::json!({
                    "id":"source", "type_":"series", "name":"Stale before import", "added_at_secs":1,
                })).unwrap();
                let stale_progress = HashMap::from([(
                    progress_map_key("source", "episode"),
                    EpisodeProgress {
                        series_id: "source".into(),
                        episode_id: "episode".into(),
                        position_secs: 12.0,
                        duration_secs: 100.0,
                        ..Default::default()
                    },
                )]);
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.entries = vec![stale_entry.clone()];
                    state.progress = stale_progress.clone();
                    state.modal_item = Some(reload_race_modal());
                }
                // The import is already durable; the old allocation is stale.
                let imported_entry: LibraryEntry = serde_json::from_value(serde_json::json!({
                    "id":"imported", "type_":"movie", "name":"Imported", "added_at_secs":2,
                }))
                .unwrap();
                let imported_progress = HashMap::from([(
                    progress_map_key("imported", "new"),
                    EpisodeProgress {
                        series_id: "imported".into(),
                        episode_id: "new".into(),
                        position_secs: 88.0,
                        duration_secs: 100.0,
                        ..Default::default()
                    },
                )]);
                let library_raw = serde_json::to_string(&vec![imported_entry.clone()]).unwrap();
                let progress_raw = serde_json::to_string(&imported_progress).unwrap();
                owner
                    .persist_snapshot("library", &library_raw, false)
                    .unwrap();
                owner
                    .persist_snapshot(EPISODE_PROGRESS_KEY, &progress_raw, false)
                    .unwrap();
                // Preserve the storage layer's committed representation,
                // including collection canonicalization, across rejected edits.
                let library_raw = owner.read("library").unwrap().unwrap();
                let progress_raw = owner.read(EPISODE_PROGRESS_KEY).unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<Vec<LibraryEntry>>(&library_raw).unwrap(),
                    vec![imported_entry.clone()]
                );
                assert_eq!(
                    serde_json::from_str::<HashMap<String, EpisodeProgress>>(&progress_raw)
                        .unwrap(),
                    imported_progress
                );
                let (gate, entered) = persistence::hold_next_save();
                persistence::write_progress(&imported_progress);
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                bridge.profile_loaded_epoch.store(0, Ordering::Release);
                bridge.activate_profile(&source.id, false).unwrap();
                assert!(!profiles::authoritative_edits_allowed());
                let failures = io::write_failures();
                bridge.toggle_current_in_library();
                bridge.episode_toggle_watched(0);
                {
                    let state = bridge.shared.lock().unwrap();
                    assert_eq!(state.entries, vec![stale_entry.clone()]);
                    assert_eq!(state.progress, stale_progress);
                }
                assert!(
                    io::write_failures() >= failures + 2,
                    "real gestures must be rejected before mutation"
                );
                write_persisted_library(&[stale_entry.clone()]);
                write_progress_map(&stale_progress);
                assert!(storage::try_set_str("library", "[]").is_err());
                assert_eq!(
                    owner.read("library").unwrap().as_deref(),
                    Some(library_raw.as_str())
                );
                assert_eq!(
                    owner.read(EPISODE_PROGRESS_KEY).unwrap().as_deref(),
                    Some(progress_raw.as_str())
                );
                profiles::invoke_account(move || gate.release()).unwrap();
                bridge.wait_profile_activation();
                assert_ne!(profiles::active_context().epoch, source.epoch);
                assert!(profiles::authoritative_edits_allowed());
                assert_eq!(bridge.shared.lock().unwrap().entries, vec![imported_entry]);
                assert_eq!(bridge.shared.lock().unwrap().progress, imported_progress);
                // A retained old callback cannot overwrite the fresh same-ID allocation.
                profiles::with_context(source, || {
                    assert!(!profiles::authoritative_edits_allowed());
                    write_persisted_library(&[stale_entry]);
                    write_progress_map(&stale_progress);
                });
                persistence::flush().unwrap();
                assert_eq!(
                    owner.read("library").unwrap().as_deref(),
                    Some(library_raw.as_str())
                );
                assert_eq!(
                    owner.read(EPISODE_PROGRESS_KEY).unwrap().as_deref(),
                    Some(progress_raw.as_str())
                );
            },
        );
    }

    #[test]
    fn ordinary_activation_accepts_and_revalidates_outgoing_library_and_progress_edits() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::ordinary_activation_accepts_and_revalidates_outgoing_library_and_progress_edits"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let source = profiles::active_context();
                let owner = profiles::ProfileStore::for_profile(&source.id);
                let entry: LibraryEntry = serde_json::from_value(serde_json::json!({
                    "id":"source", "type_":"series", "name":"Source", "added_at_secs":1,
                }))
                .unwrap();
                let progress = HashMap::from([(
                    progress_map_key("source", "episode"),
                    EpisodeProgress {
                        series_id: "source".into(),
                        episode_id: "episode".into(),
                        position_secs: 12.0,
                        duration_secs: 100.0,
                        ..Default::default()
                    },
                )]);
                owner
                    .persist_snapshot(
                        "library",
                        &serde_json::to_string(&vec![entry.clone()]).unwrap(),
                        false,
                    )
                    .unwrap();
                owner
                    .persist_snapshot(
                        EPISODE_PROGRESS_KEY,
                        &serde_json::to_string(&progress).unwrap(),
                        false,
                    )
                    .unwrap();
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.entries = vec![entry];
                    state.progress = progress.clone();
                    state.modal_item = Some(reload_race_modal());
                }
                let target = profiles::create("Outgoing edits survive").unwrap();
                let (gate, entered) = persistence::hold_next_save();
                persistence::write_progress(&progress);
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                bridge.switch_profile(&target);
                assert!(profiles::authoritative_edits_allowed());
                bridge.toggle_current_in_library();
                bridge.episode_toggle_watched(0);
                assert!(bridge.shared.lock().unwrap().entries.is_empty());
                assert!(
                    bridge.shared.lock().unwrap().progress[&progress_map_key("source", "episode")]
                        .watched
                );
                let generation = bridge.profile_activation.lock().unwrap().generation;
                profiles::invoke_account(move || gate.release()).unwrap();
                bridge.wait_profile_activation();
                assert_eq!(profiles::active_context().id, target);
                assert_eq!(
                    bridge.profile_activation.lock().unwrap().generation,
                    generation,
                    "edits restart the same logical coordinator"
                );
                assert_eq!(owner.read("library").unwrap().as_deref(), Some("[]"));
                let saved: HashMap<String, EpisodeProgress> =
                    read_owned(&owner, EPISODE_PROGRESS_KEY).unwrap();
                assert!(saved[&progress_map_key("source", "episode")].watched);
            },
        );
    }

    fn reload_race_modal() -> ModalItem {
        ModalItem {
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "source".into(),
            type_: "series".into(),
            request_id: "episode".into(),
            videos: vec![Video {
                id: "episode".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }],
            season_backdrops: HashMap::new(),
            seasons: vec![1],
            season_index: 0,
            episode_page: 0,
            name: "Source".into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        }
    }

    #[test]
    fn activation_worker_is_single_and_shutdown_drains_every_admitted_phase() {
        let worker = ActivationWorker::spawn().unwrap();
        let (entered, entering) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (saved, saves) = std::sync::mpsc::channel();
        let first = saved.clone();
        worker
            .sender
            .as_ref()
            .unwrap()
            .try_send(Box::new(move || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                first.send((1, std::thread::current().id())).unwrap();
            }))
            .unwrap();
        entering.recv_timeout(Duration::from_secs(5)).unwrap();
        worker
            .sender
            .as_ref()
            .unwrap()
            .try_send(Box::new(move || {
                saved.send((2, std::thread::current().id())).unwrap();
            }))
            .unwrap();
        let (closed, closing) = std::sync::mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            drop(worker);
            closed.send(()).unwrap();
        });
        assert!(
            saves.try_recv().is_err(),
            "a replacement phase cannot run past the held predecessor"
        );
        assert!(closing.try_recv().is_err());
        release.send(()).unwrap();
        closing.recv_timeout(Duration::from_secs(5)).unwrap();
        shutdown.join().unwrap();
        let first = saves.recv().unwrap();
        let second = saves.recv().unwrap();
        assert_eq!((first.0, second.0), (1, 2));
        assert_eq!(
            first.1, second.1,
            "all phases execute on one retained OS worker"
        );
    }

    #[test]
    fn activation_returns_before_held_writer_and_coalesces_repeated_requests() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::activation_returns_before_held_writer_and_coalesces_repeated_requests"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let target = profiles::create("Incoming").unwrap();
                let source = profiles::active_context();
                let before = bridge.profile_activation.lock().unwrap().generation;
                let (gate, entered) = persistence::hold_next_save();
                persistence::write_progress(&bridge.shared.lock().unwrap().progress);
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                let (returned, receipt) = std::sync::mpsc::channel();
                let blocked = Arc::new(AtomicBool::new(false));
                let watchdog = std::thread::spawn({
                    let gate = gate.clone();
                    let blocked = blocked.clone();
                    move || {
                        if receipt.recv_timeout(Duration::from_secs(5)).is_err() {
                            blocked.store(true, Ordering::Release);
                            gate.release();
                        }
                    }
                });
                let started = std::time::Instant::now();
                let continued = Arc::new(AtomicBool::new(false));
                bridge.switch_profile_after(&target, {
                    let continued = continued.clone();
                    let target = target.clone();
                    move |_, context| {
                        assert_eq!(context.id, target);
                        assert_eq!(profiles::active_context(), context);
                        continued.store(true, Ordering::Release);
                    }
                });
                bridge.switch_profile(&target);
                let callback_us = started.elapsed().as_micros();
                assert!(
                    !blocked.load(Ordering::Acquire),
                    "selection callback waited for the writer"
                );
                returned.send(()).unwrap();
                assert_eq!(profiles::active_context(), source);
                {
                    let coordinator = bridge.profile_activation.lock().unwrap();
                    assert_eq!(coordinator.generation, before + 1);
                    assert!(coordinator.latest.is_none());
                    assert!(!coordinator.sealed);
                }
                let dispatched = Arc::new(AtomicBool::new(false));
                profiles::invoke_account({
                    let dispatched = dispatched.clone();
                    move || {
                        dispatched.store(true, Ordering::Release);
                        gate.release();
                    }
                })
                .unwrap();
                bridge.wait_profile_activation();
                watchdog.join().unwrap();
                assert!(
                    dispatched.load(Ordering::Acquire),
                    "harmless UI event must run while persistence is held"
                );
                assert_eq!(profiles::active_context().id, target);
                assert!(
                    continued.load(Ordering::Acquire),
                    "notification-style continuation follows its matching activation receipt"
                );
                eprintln!(
                    "STATE05 activation callback_us={callback_us}; one coordinator; UI sentinel released held writer"
                );
            },
        );
    }

    #[test]
    fn replacement_selection_cancels_preparation_without_persisting_obsolete_target() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::replacement_selection_cancels_preparation_without_persisting_obsolete_target"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let target = profiles::create("Canceled").unwrap();
                let source = profiles::active_context();
                let selection = nova_storage::try_get_str("profiles:device:v1").unwrap();
                let (gate, entered) = persistence::hold_next_save();
                persistence::write_progress(&bridge.shared.lock().unwrap().progress);
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                let continued = Arc::new(AtomicBool::new(false));
                bridge.switch_profile_after(&target, {
                    let continued = continued.clone();
                    move |_, _| continued.store(true, Ordering::Release)
                });
                bridge.switch_profile(&source.id);
                profiles::invoke_account(move || gate.release()).unwrap();
                bridge.wait_profile_activation();
                assert_eq!(profiles::active_context(), source);
                assert!(!continued.load(Ordering::Acquire));
                assert_eq!(
                    nova_storage::try_get_str("profiles:device:v1").unwrap(),
                    selection
                );
            },
        );
    }

    #[test]
    fn target_policy_changed_during_preparation_cannot_publish_or_change_selection() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::target_policy_changed_during_preparation_cannot_publish_or_change_selection"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let target = profiles::create("Protected while preparing").unwrap();
                let source = profiles::active_context();
                let selection = nova_storage::try_get_str("profiles:device:v1").unwrap();
                let (gate, entered) = persistence::hold_next_save();
                persistence::write_progress(&bridge.shared.lock().unwrap().progress);
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                bridge.switch_profile(&target);
                profiles::ProfileStore::for_profile(&target)
                    .write(
                        profile_policy::KEY,
                        &serde_json::to_string(&profile_policy::ProfilePolicy {
                            pin: "0012".into(),
                            require_pin_on_entry: true,
                            ..Default::default()
                        })
                        .unwrap(),
                    )
                    .unwrap();
                profiles::invoke_account(move || gate.release()).unwrap();
                bridge.wait_profile_activation();
                assert_eq!(profiles::active_context(), source);
                assert_eq!(
                    nova_storage::try_get_str("profiles:device:v1").unwrap(),
                    selection
                );
                assert!(!bridge.app().unwrap().get_profiles_error().is_empty());
            },
        );
    }

    #[test]
    fn initials_follow_display_names_while_colors_follow_identity() {
        let mut profile = profiles::Profile {
            id: "stable-profile".into(),
            name: "  e\u{301}va  ".into(),
        };
        let original = profile_row(&profile, &profile.id);
        assert_eq!(original.initial, "É");
        for (name, expected) in [("mišo", "M"), ("张伟", "张"), ("", "?")] {
            profile.name = name.into();
            let renamed = profile_row(&profile, "another-profile");
            assert_eq!(renamed.initial, expected);
            assert_eq!(renamed.avatar_background, original.avatar_background);
            assert_eq!(renamed.avatar_foreground, original.avatar_foreground);
        }
        let pairs = (0..128)
            .map(|index| {
                profile.id = format!("device:{index}");
                let row = profile_row(&profile, "");
                (
                    row.avatar_background.as_argb_encoded(),
                    row.avatar_foreground.as_argb_encoded(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(pairs.len(), AVATAR_COLORS.len());
    }

    #[test]
    fn every_avatar_color_pair_meets_small_text_contrast() {
        fn luminance(rgb: u32) -> f64 {
            let linear = |shift: u32| {
                let channel = f64::from((rgb >> shift) & 0xff_u32) / 255.0;
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
        }
        for (background, foreground) in AVATAR_COLORS {
            let a = luminance(background);
            let b = luminance(foreground);
            let contrast = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(
                contrast >= 4.5,
                "{background:06x}/{foreground:06x}: {contrast}"
            );
        }
    }
}

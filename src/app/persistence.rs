//! Ordered off-thread Library/history serialization and atomic persistence.
//! Queued snapshots are visible to app readers; only the worker reads the
//! committed previous value when deriving activity and tracking journal edits.
use super::*;
use std::collections::VecDeque;
use std::sync::{Condvar, mpsc};

const MAX_PENDING: usize = 64;
static ACTIVE: Mutex<Option<Arc<Queue>>> = Mutex::new(None);
thread_local! { static IN_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

pub(super) fn register_collections() {
    register_for(&profiles::ProfileStore::capture());
}

fn register_for(owner: &profiles::ProfileStore) {
    owner.register_collection(profile_policy::KEY, storage::CollectionKind::Object);
    owner.register_collection(smart_offline::KEY, storage::CollectionKind::Object);
    owner.register_collection(
        "library",
        storage::CollectionKind::CompositeArray {
            id_fields: vec!["type_".into(), "id".into()],
        },
    );
    owner.register_collection(EPISODE_PROGRESS_KEY, storage::CollectionKind::Object);
    for domain in [sync::DOMAIN_LIBRARY, sync::DOMAIN_PROGRESS] {
        owner.register_collection(
            &format!("sync:baseline:{}", owner.domain(domain)),
            storage::CollectionKind::Object,
        );
    }
}

#[derive(Clone)]
enum Snapshot {
    Library(Vec<LibraryEntry>),
    Progress(HashMap<String, EpisodeProgress>, bool),
    External(Box<ExternalPatch>, mpsc::Sender<Result<(), String>>, bool),
}

/// A remote materialization changes only these records. Resolve it against
/// committed snapshots on the ordered writer so unrelated queued local edits
/// are retained, without copying or serializing the whole history on the UI.
#[derive(Clone, Default)]
pub(super) struct ExternalPatch {
    pub library: Option<ExternalLibraryEdit>,
    pub progress: HashMap<String, EpisodeProgress>,
}

#[derive(Clone)]
pub(super) struct ExternalLibraryEdit {
    pub id: String,
    pub media_type: String,
    pub entry: Option<LibraryEntry>,
}

impl ExternalPatch {
    pub fn is_empty(&self) -> bool {
        self.library.is_none() && self.progress.is_empty()
    }

    fn save(&self, owner: &profiles::ProfileStore, projection: bool) -> Result<(), String> {
        // Origin is thread-local: recreate both guards on the actual writer.
        let _applying = sync::ApplyingGuard::new();
        let _external = (!projection).then(stremio_events::ExternalGuard::new);
        if let Some(edit) = &self.library {
            let mut entries: Vec<LibraryEntry> = owner
                .read("library")
                .map_err(|error| error.to_string())?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or_default();
            edit.apply(&mut entries);
            let raw = serde_json::to_string(&entries).map_err(|error| error.to_string())?;
            owner
                .persist_snapshot("library", &raw, false)
                .map_err(|error| format!("{error:#}"))?;
        }
        if !self.progress.is_empty() {
            let mut progress: HashMap<String, EpisodeProgress> = owner
                .read(EPISODE_PROGRESS_KEY)
                .map_err(|error| error.to_string())?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or_default();
            progress.extend(self.progress.clone());
            let raw = serde_json::to_string(&progress).map_err(|error| error.to_string())?;
            owner
                .persist_snapshot(EPISODE_PROGRESS_KEY, &raw, false)
                .map_err(|error| format!("{error:#}"))?;
        }
        Ok(())
    }
}

impl ExternalLibraryEdit {
    fn apply(&self, entries: &mut Vec<LibraryEntry>) {
        entries.retain(|entry| entry.id != self.id || entry.type_ != self.media_type);
        if let Some(entry) = &self.entry {
            entries.push(entry.clone());
        }
    }
}

impl Snapshot {
    fn periodic(&self) -> bool {
        matches!(self, Self::Progress(_, true))
    }
    fn save(&self, owner: &profiles::ProfileStore) -> Result<(), String> {
        let (key, raw) = match self {
            Self::Library(entries) => ("library", serde_json::to_string(entries)),
            Self::Progress(map, _) => (EPISODE_PROGRESS_KEY, serde_json::to_string(map)),
            Self::External(patch, _, projection) => return patch.save(owner, *projection),
        };
        let raw = raw.map_err(|error| error.to_string())?;
        let save = || {
            owner
                .persist_snapshot(key, &raw, false)
                .map_err(|error| format!("{error:#}"))
        };
        if self.periodic() {
            nova_sync::playback_checkpoint(save)
        } else {
            save()
        }
    }
}
#[derive(Default)]
struct State {
    pending: VecDeque<(u64, profiles::ProfileStore, Snapshot)>,
    next: u64,
    completed: u64,
    head_started: bool,
    error: Option<String>,
    retry: bool,
    closing: bool,
    stop: bool,
}
struct Queue {
    state: Mutex<State>,
    changed: Condvar,
}
impl Queue {
    fn enqueue(&self, snapshot: Snapshot) -> Result<bool, String> {
        let owner = profiles::ProfileStore::capture();
        let mut state = self.state.lock().unwrap();
        while state.pending.len() >= MAX_PENDING
            && state.error.is_none()
            && !state.stop
            && !state.closing
        {
            state = self.changed.wait(state).unwrap();
        }
        if state.stop || state.closing {
            return Ok(false);
        }
        if state.pending.len() >= MAX_PENDING {
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| "persistence queue full".into()));
        }
        state.next += 1;
        let sequence = state.next;
        // Only adjacent periodic checkpoints may collapse. Manual watched /
        // unwatched edits and Library changes retain their transaction order.
        let replace = snapshot.periodic()
            && state
                .pending
                .back()
                .is_some_and(|(_, old_owner, old)| old_owner == &owner && old.periodic())
            && !(state.pending.len() == 1 && state.head_started);
        if replace {
            state.pending.pop_back();
        }
        state.pending.push_back((sequence, owner, snapshot));
        self.changed.notify_all();
        Ok(true)
    }
    fn flush(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let target = state.next;
        if state.completed >= target {
            return Ok(());
        }
        if state.stop {
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| "persistence worker stopped before saving".into()));
        }
        if state.error.is_some() {
            state.retry = true;
            state.error = None;
            self.changed.notify_all();
        }
        while state.completed < target {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            if state.stop {
                return Err("persistence worker stopped before saving".into());
            }
            state = self.changed.wait(state).unwrap();
        }
        Ok(())
    }
}
pub(super) struct Worker {
    queue: Arc<Queue>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    fn spawn(
        save: impl Fn(&profiles::ProfileStore, &Snapshot) -> Result<(), String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let queue = Arc::new(Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let worker = queue.clone();
        let thread = std::thread::Builder::new()
            .name("nova-persistence".into())
            .spawn(move || {
                IN_WORKER.set(true);
                loop {
                    let (sequence, owner, snapshot) = {
                        let mut state = worker.state.lock().unwrap();
                        while !state.stop
                            && (state.pending.is_empty() || (state.error.is_some() && !state.retry))
                        {
                            if state.error.is_some() {
                                let (next, timeout) = worker
                                    .changed
                                    .wait_timeout(state, Duration::from_secs(2))
                                    .unwrap();
                                state = next;
                                if timeout.timed_out() {
                                    state.retry = true;
                                }
                            } else {
                                state = worker.changed.wait(state).unwrap();
                            }
                        }
                        if state.stop {
                            break;
                        }
                        state.head_started = true;
                        state.retry = false;
                        state.pending.front().unwrap().clone()
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if owner.retired().map_err(|error| error.to_string())? {
                            return Ok(());
                        }
                        let result = save(&owner, &snapshot);
                        // A durable retirement can race the transaction guard.
                        if result.is_err() && owner.retired().map_err(|error| error.to_string())? {
                            Ok(())
                        } else {
                            result
                        }
                    }))
                    .unwrap_or_else(|_| {
                        Err("persistence worker panicked; pending saves retained".into())
                    });
                    if let Snapshot::External(_, done, _) = &snapshot {
                        let _ = done.send(result.clone());
                    }
                    let mut state = worker.state.lock().unwrap();
                    match result {
                        Ok(()) => {
                            state.pending.pop_front();
                            state.head_started = false;
                            state.completed = sequence;
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error),
                    }
                    worker.changed.notify_all();
                }
            })?;
        Ok(Self {
            queue,
            thread: Some(thread),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.state.lock().unwrap().closing = true;
        self.queue.changed.notify_all();
        // Drain while producers still see this queue; no synchronous writer
        // may overtake a pending job during shutdown.
        if let Err(error) = self.queue.flush() {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
        }
        let mut active = ACTIVE.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|queue| Arc::ptr_eq(queue, &self.queue))
        {
            *active = None;
        }
        drop(active);
        self.queue.state.lock().unwrap().stop = true;
        self.queue.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) fn start() -> Option<Worker> {
    register_collections();
    // Resolve the stable device prefix during startup, never while a watched
    // gesture holds Shared. The boot/sequence suffix also covers no-writer tests.
    if let Ok(device) = nova_sync::local_device() {
        detail::initialize_manual_watch_actions(device);
    }
    match Worker::spawn(|owner, snapshot| {
        snapshot.save(owner).inspect_err(|error| {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
        })
    }) {
        Ok(worker) => {
            *ACTIVE.lock().unwrap() = Some(worker.queue.clone());
            nova_sync::set_persistence_barrier(Arc::new(|| {
                flush().map_err(|error| {
                    storage::Error::new(storage::ErrorKind::Transaction, error).into()
                })
            }));
            Some(worker)
        }
        Err(error) => {
            storage::report(storage::Error::new(storage::ErrorKind::Unavailable, error));
            None
        }
    }
}
pub(super) fn flush() -> Result<(), String> {
    if IN_WORKER.get() {
        return Ok(());
    }
    let queue = ACTIVE.lock().unwrap().clone();
    queue.map_or(Ok(()), |queue| queue.flush())
}
pub(super) fn flush_report() {
    if let Err(error) = flush() {
        io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
    }
}
pub(super) fn failed() -> bool {
    let queue = ACTIVE.lock().unwrap().clone();
    queue.is_some_and(|queue| queue.state.lock().unwrap().error.is_some())
}

/// Never wait for queue capacity on the event loop. A busy writer makes the
/// Stremio worker retry preparation; accepted jobs remain ordered with manual
/// edits, and only their background completion acknowledges delivery.
pub(super) fn try_external(patch: ExternalPatch) -> Option<mpsc::Receiver<Result<(), String>>> {
    try_patch(patch, false)
}

/// Alias materialization advances only the local raw view/baseline. Unlike an
/// imported external user action, it must never allocate fresh sync clocks.
pub(super) fn try_progress_projection(
    progress: HashMap<String, EpisodeProgress>,
) -> Option<mpsc::Receiver<Result<(), String>>> {
    try_patch(
        ExternalPatch {
            progress,
            ..Default::default()
        },
        true,
    )
}

fn try_patch(patch: ExternalPatch, projection: bool) -> Option<mpsc::Receiver<Result<(), String>>> {
    // Without the ordered writer there is nowhere to retain a failed save.
    // Leave Shared untouched and delivery pending rather than launch a
    // one-shot write whose failure could later look like a successful no-op.
    let queue = ACTIVE.lock().unwrap().clone()?;
    let (done, receipt) = mpsc::channel();
    let mut state = queue.state.try_lock().ok()?;
    if state.pending.len() >= MAX_PENDING || state.stop || state.closing {
        return None;
    }
    state.next += 1;
    let sequence = state.next;
    state.pending.push_back((
        sequence,
        profiles::ProfileStore::capture(),
        Snapshot::External(Box::new(patch), done, projection),
    ));
    queue.changed.notify_all();
    Some(receipt)
}

#[cfg(test)]
pub(super) fn running() -> bool {
    ACTIVE.lock().unwrap().is_some()
}
fn enqueue(snapshot: Snapshot) -> bool {
    if applying() || IN_WORKER.get() {
        return false;
    }
    let queue = ACTIVE.lock().unwrap().clone();
    let Some(queue) = queue else {
        return false;
    };
    match queue.enqueue(snapshot) {
        Ok(accepted) => accepted,
        Err(error) => {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
            true
        }
    }
}
pub(super) fn write_library(entries: &[LibraryEntry]) {
    // In-memory edits precede the ordered write. Do not let an older worker
    // projection publish while the new snapshot is still waiting to save.
    progress_projection::invalidate();
    if !enqueue(Snapshot::Library(entries.to_vec())) {
        write_json("library", entries);
    }
}
pub(super) fn write_progress(map: &HashMap<String, EpisodeProgress>) {
    progress_projection::invalidate();
    if !enqueue(Snapshot::Progress(
        map.clone(),
        nova_sync::is_playback_checkpoint(),
    )) {
        write_json(EPISODE_PROGRESS_KEY, map);
    }
}
pub(super) fn pending_library() -> Option<Vec<LibraryEntry>> {
    if IN_WORKER.get() {
        return None;
    }
    let queue = ACTIVE.lock().unwrap().clone()?;
    let (base, edits) = {
        let state = queue.state.lock().unwrap();
        let mut base = None;
        let mut edits = Vec::new();
        for (_, owner, snapshot) in state.pending.iter().rev() {
            if owner != &profiles::ProfileStore::capture() {
                continue;
            }
            match snapshot {
                Snapshot::Library(entries) => {
                    base = Some(entries.clone());
                    break;
                }
                Snapshot::External(patch, _, _) => edits.extend(patch.library.iter().cloned()),
                _ => {}
            }
        }
        (base, edits)
    };
    if base.is_none() && edits.is_empty() {
        return None;
    }
    let mut entries = base.unwrap_or_else(|| read_json("library").unwrap_or_default());
    for edit in edits.into_iter().rev() {
        edit.apply(&mut entries);
    }
    Some(entries)
}
pub(super) fn pending_progress() -> Option<HashMap<String, EpisodeProgress>> {
    if IN_WORKER.get() {
        return None;
    }
    let queue = ACTIVE.lock().unwrap().clone()?;
    let (base, edits) = {
        let state = queue.state.lock().unwrap();
        let mut base = None;
        let mut edits = Vec::new();
        for (_, owner, snapshot) in state.pending.iter().rev() {
            if owner != &profiles::ProfileStore::capture() {
                continue;
            }
            match snapshot {
                Snapshot::Progress(map, _) => {
                    base = Some(map.clone());
                    break;
                }
                Snapshot::External(patch, _, _) if !patch.progress.is_empty() => {
                    edits.push(patch.progress.clone())
                }
                _ => {}
            }
        }
        (base, edits)
    };
    if base.is_none() && edits.is_empty() {
        return None;
    }
    let mut progress = base.unwrap_or_else(|| read_json(EPISODE_PROGRESS_KEY).unwrap_or_default());
    for edit in edits.into_iter().rev() {
        progress.extend(edit);
    }
    Some(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn progress(position: f64, periodic: bool) -> Snapshot {
        Snapshot::Progress(
            HashMap::from([(
                "episode".into(),
                EpisodeProgress {
                    position_secs: position,
                    ..Default::default()
                },
            )]),
            periodic,
        )
    }
    fn position(snapshot: &Snapshot) -> f64 {
        match snapshot {
            Snapshot::Progress(map, _) => map["episode"].position_secs,
            _ => unreachable!(),
        }
    }
    #[test]
    fn pending_periodic_saves_coalesce_without_erasing_manual_edits() {
        let queue = Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        };
        queue.enqueue(progress(1.0, true)).unwrap();
        queue.enqueue(progress(2.0, true)).unwrap();
        queue.enqueue(progress(0.0, false)).unwrap();
        queue.enqueue(progress(3.0, true)).unwrap();
        queue.enqueue(progress(4.0, true)).unwrap();
        let state = queue.state.lock().unwrap();
        assert_eq!(
            state
                .pending
                .iter()
                .map(|(_, _, save)| position(save))
                .collect::<Vec<_>>(),
            vec![2.0, 0.0, 4.0]
        );
        assert_eq!(state.pending.back().unwrap().0, 5);
    }
    #[test]
    fn periodic_saves_and_pending_reads_keep_profile_ownership() {
        let queue = Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        };
        profiles::with_context(profiles::context_for("alice"), || {
            queue.enqueue(progress(1.0, true)).unwrap()
        });
        profiles::with_context(profiles::context_for("bob"), || {
            queue.enqueue(progress(2.0, true)).unwrap()
        });
        let state = queue.state.lock().unwrap();
        assert_eq!(state.pending.len(), 2);
        assert_eq!(state.pending[0].1.id(), "alice");
        assert_eq!(state.pending[1].1.id(), "bob");
    }
    #[test]
    fn remote_retirement_drops_failed_head_before_ui_reconciliation() {
        const ROOT: &str = "NOVA_PROFILE_QUEUE_RETIREMENT_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-profile-retirement-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::persistence::tests::remote_retirement_drops_failed_head_before_ui_reconciliation", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        nova_storage::init_at(&PathBuf::from(root));
        profiles::initialize().unwrap();
        let retired = profiles::capture();
        let survivor = profiles::create("Survivor").unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, receiver) = mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::spawn({
            let gate = gate.clone();
            let saved = saved.clone();
            let retired = retired.clone();
            move |owner, snapshot| {
                if owner.id() == retired.id {
                    let _ = started.send(());
                    let mut ready = gate.0.lock().unwrap();
                    while !*ready {
                        ready = gate.1.wait(ready).unwrap();
                    }
                    return Err("profile has been deleted".into());
                }
                saved
                    .lock()
                    .unwrap()
                    .push((owner.id().to_string(), position(snapshot)));
                Ok(())
            }
        })
        .unwrap();
        worker.queue.enqueue(progress(1.0, false)).unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        {
            let owner = nova_sync::local_store().unwrap();
            let mut store = owner.lock().unwrap();
            store.set(
                profiles::DELETIONS_DOMAIN,
                &retired.id,
                Some("1".into()),
                0,
                9,
            );
            store.save().unwrap();
        }
        // The UI has not called list()/refresh_profiles yet: its cached view
        // still allows this identity, exactly the network delivery gap.
        assert!(profiles::writable(&retired.id));
        worker.queue.enqueue(progress(2.0, false)).unwrap();
        profiles::with_context(profiles::context_for(&survivor), || {
            worker.queue.enqueue(progress(3.0, false)).unwrap()
        });
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        worker.queue.flush().unwrap();
        assert_eq!(*saved.lock().unwrap(), vec![(survivor, 3.0)]);
        assert!(worker.queue.state.lock().unwrap().pending.is_empty());
        assert!(worker.queue.state.lock().unwrap().error.is_none());
        profiles::list().unwrap();
        profiles::with_context(retired, || {
            persist_sync_snapshot("downloads:v1", "[]", false).unwrap();
        });
        assert_eq!(
            nova_storage::try_get_str("downloads:v1")
                .unwrap()
                .as_deref(),
            Some("[]")
        );
    }

    #[test]
    fn in_flight_save_is_retained_and_flush_waits_for_worker_commit() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let saved = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::spawn({
            let gate = gate.clone();
            let started = started.clone();
            let saved = saved.clone();
            move |_, snapshot| {
                *started.0.lock().unwrap() = true;
                started.1.notify_all();
                let mut ready = gate.0.lock().unwrap();
                while !*ready {
                    ready = gate.1.wait(ready).unwrap();
                }
                saved.lock().unwrap().push(position(snapshot));
                Ok(())
            }
        })
        .unwrap();
        worker.queue.enqueue(progress(1.0, true)).unwrap();
        let mut running = started.0.lock().unwrap();
        while !*running {
            running = started.1.wait(running).unwrap();
        }
        drop(running);
        worker.queue.enqueue(progress(2.0, true)).unwrap();
        worker.queue.enqueue(progress(3.0, true)).unwrap();
        assert_eq!(worker.queue.state.lock().unwrap().pending.len(), 2);
        let (sender, receiver) = std::sync::mpsc::channel();
        let queue = worker.queue.clone();
        let waiter = thread::spawn(move || {
            sender.send(queue.flush()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(30)).is_err());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        waiter.join().unwrap();
        assert_eq!(*saved.lock().unwrap(), vec![1.0, 3.0]);
    }
    #[test]
    fn failed_head_stays_queued_and_later_edits_cannot_overtake_it() {
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let saved = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::spawn({
            let failing = failing.clone();
            let saved = saved.clone();
            move |_, snapshot| {
                if failing.load(Ordering::Relaxed) {
                    return Err("injected write failure".into());
                }
                saved.lock().unwrap().push(position(snapshot));
                Ok(())
            }
        })
        .unwrap();
        worker.queue.enqueue(progress(1.0, false)).unwrap();
        worker.queue.enqueue(progress(0.0, false)).unwrap();
        assert!(worker.queue.flush().is_err());
        assert_eq!(worker.queue.state.lock().unwrap().pending.len(), 2);
        assert!(saved.lock().unwrap().is_empty());
        failing.store(false, Ordering::Relaxed);
        worker.queue.flush().unwrap();
        assert_eq!(*saved.lock().unwrap(), vec![1.0, 0.0]);
    }
    #[test]
    fn shutdown_drains_pending_saves() {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::spawn({
            let saved = saved.clone();
            move |_, snapshot| {
                saved.lock().unwrap().push(position(snapshot));
                Ok(())
            }
        })
        .unwrap();
        worker.queue.enqueue(progress(1.0, false)).unwrap();
        worker.queue.enqueue(progress(2.0, false)).unwrap();
        let queue = worker.queue.clone();
        drop(worker);
        assert_eq!(*saved.lock().unwrap(), vec![1.0, 2.0]);
        assert!(
            !queue.enqueue(progress(3.0, false)).unwrap(),
            "late producers must use the synchronous fallback"
        );
        queue.flush().unwrap();
    }
    #[test]
    fn stopped_queue_returns_failure_instead_of_waiting_for_a_dead_worker() {
        let queue = Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        };
        queue.enqueue(progress(1.0, false)).unwrap();
        queue.state.lock().unwrap().stop = true;
        assert!(queue.flush().is_err());
    }
    #[test]
    fn production_worker_preserves_journal_order_and_synchronous_projection() {
        const ROOT: &str = "NOVA_PERSISTENCE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-persistence-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::persistence::tests::production_worker_preserves_journal_order_and_synchronous_projection", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        storage::init_at(&PathBuf::from(root));
        let worker = start().unwrap();
        let entries = vec![LibraryEntry {
            id: "sample".into(),
            type_: "series".into(),
            name: "Sample".into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: vec![],
            description: "Metadata ".repeat(200),
            categories: vec![],
            watch_status: WatchStatus::Auto,
            imported_tracking_status: None,
            added_at_secs: 1,
        }];
        write_persisted_library(&entries);
        assert_eq!(read_persisted_library(), entries);
        let key = progress_map_key("sample", "episode");
        let mut map = HashMap::from([(
            key.clone(),
            EpisodeProgress {
                series_id: "sample".into(),
                episode_id: "episode".into(),
                position_secs: 2.0,
                duration_secs: 100.0,
                play_count: 1,
                ..Default::default()
            },
        )]);
        write_progress_map(&map);
        map.get_mut(&key).unwrap().watched = true;
        write_progress_map(&map);
        map.get_mut(&key).unwrap().watched = false;
        map.get_mut(&key).unwrap().position_secs = 0.0;
        map.get_mut(&key).unwrap().unwatched_at_secs = now_secs();
        write_progress_map(&map);
        assert!(!read_progress_map()[&key].watched);
        // Unrelated synchronous app writes drain the queue first.
        write_json("continue_hidden", &HashMap::<String, u64>::new());
        assert!(worker.queue.state.lock().unwrap().pending.is_empty());
        let persisted =
            read_json::<HashMap<String, EpisodeProgress>>(EPISODE_PROGRESS_KEY).unwrap();
        assert!(!persisted[&key].watched);
        let events = storage::try_scan_prefix(nova_tracking::EVENT_PREFIX)
            .unwrap()
            .into_iter()
            .map(|(_, raw)| serde_json::from_str::<nova_tracking::WatchEvent>(&raw).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 3);
        assert!(events[0].changes[0].started);
        assert_eq!(
            events
                .iter()
                .map(|event| event.changes[0].watched)
                .collect::<Vec<_>>(),
            vec![false, true, false]
        );
        assert!(
            events
                .iter()
                .all(|event| event.origin == nova_tracking::EventOrigin::Local)
        );
        let owner = nova_sync::local_store().unwrap();
        let digest = owner.lock().unwrap().digest();
        {
            let _guard = sync::ApplyingGuard::new();
            map.get_mut(&key).unwrap().watched = true;
            write_progress_map(&map);
            assert!(
                read_json::<HashMap<String, EpisodeProgress>>(EPISODE_PROGRESS_KEY).unwrap()[&key]
                    .watched
            );
        }
        assert_eq!(
            owner.lock().unwrap().digest(),
            digest,
            "projection must not echo"
        );
        let last = storage::try_scan_prefix(nova_tracking::EVENT_PREFIX)
            .unwrap()
            .pop()
            .unwrap()
            .1;
        assert_eq!(
            serde_json::from_str::<nova_tracking::WatchEvent>(&last)
                .unwrap()
                .origin,
            nova_tracking::EventOrigin::PairedDevice
        );
        map.get_mut(&key).unwrap().watched = false;
        write_progress_map(&map);
        drop(worker);
        assert!(
            !read_json::<HashMap<String, EpisodeProgress>>(EPISODE_PROGRESS_KEY).unwrap()[&key]
                .watched
        );
    }
}

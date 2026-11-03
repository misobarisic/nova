//! Ordered off-thread Library/history serialization and atomic persistence.
//! Queued snapshots are visible to app readers; only the worker reads the
//! committed previous value when deriving activity and tracking journal edits.
use super::*;
use std::collections::VecDeque;
use std::sync::{Condvar, mpsc};

const MAX_PENDING: usize = 64;
static ACTIVE: Mutex<Option<Arc<Queue>>> = Mutex::new(None);
#[cfg(test)]
static NEXT_TEST_SAVE: Mutex<Option<Arc<TestSaveGate>>> = Mutex::new(None);

#[cfg(test)]
pub(super) struct TestSaveGate {
    entered: mpsc::Sender<()>,
    released: Mutex<bool>,
    changed: Condvar,
}
#[cfg(test)]
impl TestSaveGate {
    pub(super) fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        self.entered.send(()).unwrap();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
}
#[cfg(test)]
pub(super) fn hold_next_save() -> (Arc<TestSaveGate>, mpsc::Receiver<()>) {
    let (entered, receipt) = mpsc::channel();
    let gate = Arc::new(TestSaveGate {
        entered,
        released: Mutex::new(false),
        changed: Condvar::new(),
    });
    assert!(
        NEXT_TEST_SAVE
            .lock()
            .unwrap()
            .replace(gate.clone())
            .is_none()
    );
    (gate, receipt)
}
thread_local! {
    static IN_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RESERVATIONS: std::cell::RefCell<Vec<Reserved>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
thread_local! {
    static BEFORE_QUEUE_ADMISSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn before_queue_admission() {
    let hook = BEFORE_QUEUE_ADMISSION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
#[cfg(test)]
pub(super) fn with_admission_pause<R>(
    hook: impl FnOnce() + 'static,
    action: impl FnOnce() -> R,
) -> R {
    BEFORE_QUEUE_ADMISSION
        .with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
    let result = action();
    BEFORE_QUEUE_ADMISSION.with(|slot| slot.borrow_mut().take());
    result
}
#[derive(Clone)]
struct Reserved {
    queue: Arc<Queue>,
    owner: profiles::ProfileStore,
    remaining: std::rc::Rc<std::cell::Cell<usize>>,
}
/// Reserve bounded capacity before changing authoritative app state. The
/// reservation stays on this thread and is consumed by the normal write API.
/// Dropping an unused reservation releases its capacity without queuing work.
pub(super) struct Reservation(Option<Reserved>);
impl Drop for Reservation {
    fn drop(&mut self) {
        let Some(reserved) = self.0.take() else {
            return;
        };
        RESERVATIONS.with(|slots| {
            slots
                .borrow_mut()
                .retain(|slot| !std::rc::Rc::ptr_eq(&slot.remaining, &reserved.remaining))
        });
        let mut state = reserved.queue.state.lock().unwrap();
        state.reserved -= reserved.remaining.get();
        let continuation = (state.reserved == 0 && state.pending.len() < MAX_PENDING)
            .then(|| state.capacity_continuation.take())
            .flatten();
        reserved.queue.changed.notify_all();
        drop(state);
        if let Some(continuation) = continuation {
            continuation();
        }
    }
}

pub(super) fn reserve(count: usize) -> Option<Reservation> {
    if !profiles::authoritative_edits_allowed() {
        io::record_write_failure(storage::Error::new(
            storage::ErrorKind::Transaction,
            "Profile reload in progress; edit not accepted",
        ));
        return None;
    }
    if applying() || IN_WORKER.get() {
        return Some(Reservation(None));
    }
    let Some(queue) = ACTIVE.lock().unwrap().clone() else {
        return Some(Reservation(None));
    };
    match queue.reserve(count) {
        Ok(reserved) => Some(reserved),
        Err(error) => {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
            None
        }
    }
}

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
    Transition(Arc<profile_actions::OutgoingTransition>),
    Operation(Arc<dyn OrderedOperation>),
}

/// Plain captured work on the same bounded writer as local snapshots. Failed
/// attempts retain this exact Arc at the head until success or shutdown.
pub(super) trait OrderedOperation: Send + Sync {
    fn save(&self) -> Result<(), String>;
    fn failed(&self, _error: &str) {}
}
impl<F: Fn() -> Result<(), String> + Send + Sync> OrderedOperation for F {
    fn save(&self) -> Result<(), String> {
        self()
    }
}
pub(super) fn retry_ordered_head() {
    if let Some(queue) = ACTIVE.lock().unwrap().clone() {
        let mut state = queue.state.lock().unwrap();
        if !state.pending.is_empty() {
            state.error = None;
            state.retry = true;
            queue.changed.notify_all();
        }
    }
}
pub(super) fn has_ordered_writer() -> bool {
    ACTIVE.lock().unwrap().is_some()
}
/// No UI mutation has occurred. Busy/capacity/reservation contention retains the
/// coordinator continuation; it may retry without overtaking admitted edits.
pub(super) fn try_ordered_operation(
    operation: Arc<dyn OrderedOperation>,
    source: Option<&profiles::Context>,
) -> Result<Option<u64>, String> {
    let queue = ACTIVE
        .lock()
        .unwrap()
        .clone()
        .ok_or("persistence worker unavailable")?;
    let snapshot = Arc::new(Snapshot::Operation(operation));
    let mut state = match queue.state.try_lock() {
        Ok(state) => state,
        Err(_) => return Ok(None),
    };
    if source
        .and_then(|source| source.session.as_ref())
        .is_some_and(|session| !session.authoritative_edits_allowed())
    {
        return Err("Profile reload in progress; edit not accepted".into());
    }
    if state.stop || state.closing {
        return Err("persistence worker closing".into());
    }
    if state.reserved > 0 || state.pending.len() + state.reserved >= MAX_PENDING {
        return Ok(None);
    }
    state.next += 1;
    let sequence = state.next;
    state
        .pending
        .push_back((sequence, profiles::ProfileStore::account(), snapshot));
    queue.changed.notify_all();
    Ok(Some(sequence))
}

pub(super) fn when_ordered_capacity(callback: impl FnOnce() + Send + 'static) {
    let Some(queue) = ACTIVE.lock().unwrap().clone() else {
        callback();
        return;
    };
    let Ok(mut state) = queue.state.try_lock() else {
        let _ = profiles::invoke_account(callback);
        return;
    };
    if state.stop || state.closing || (state.reserved == 0 && state.pending.len() < MAX_PENDING) {
        drop(state);
        callback();
    } else {
        // Exactly one logical sync coordinator has one waiting phase. Latest
        // requests coalesce there rather than accumulating queue waiters.
        debug_assert!(state.capacity_continuation.is_none());
        state.capacity_continuation = Some(Box::new(callback));
    }
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
        #[cfg(test)]
        {
            let gate = NEXT_TEST_SAVE.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait();
            }
        }
        let (key, raw) = match self {
            Self::Library(entries) => ("library", serde_json::to_string(entries)),
            Self::Progress(map, _) => (EPISODE_PROGRESS_KEY, serde_json::to_string(map)),
            Self::External(patch, _, projection) => return patch.save(owner, *projection),
            Self::Transition(capture) => return capture.save(owner),
            Self::Operation(operation) => return operation.save(),
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
    pending: VecDeque<(u64, profiles::ProfileStore, Arc<Snapshot>)>,
    capacity_continuation: Option<Box<dyn FnOnce() + Send>>,
    reserved: usize,
    next: u64,
    completed: u64,
    sync_requested: u64,
    sync_finished: u64,
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
    fn request_sync(&self) {
        let mut state = self.state.lock().unwrap();
        // One watermark covers repeated paused ticks without consuming queue
        // capacity or starting another waiter. A failed head keeps it pending.
        if !state.stop && !state.closing && state.next > state.sync_requested {
            state.sync_requested = state.next;
            self.changed.notify_all();
        }
    }

    fn reserve(self: &Arc<Self>, count: usize) -> Result<Reservation, String> {
        #[cfg(test)]
        before_queue_admission();
        let mut state = self.state.lock().unwrap();
        if !profiles::authoritative_edits_allowed() {
            return Err("Profile reload in progress; edit not accepted".into());
        }
        if state.stop
            || state.closing
            || state.error.is_some()
            || count > MAX_PENDING.saturating_sub(state.pending.len() + state.reserved)
        {
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| "persistence queue unavailable; edit not accepted".into()));
        }
        state.reserved += count;
        let reserved = Reserved {
            queue: self.clone(),
            owner: profiles::ProfileStore::capture(),
            remaining: std::rc::Rc::new(std::cell::Cell::new(count)),
        };
        RESERVATIONS.with(|slots| slots.borrow_mut().push(reserved.clone()));
        Ok(Reservation(Some(reserved)))
    }

    fn enqueue(&self, snapshot: Snapshot) -> Result<bool, String> {
        // Allocate immutable ownership before taking the queue lock. Worker
        // attempts and pending readers retain handles, never copy this payload.
        let snapshot = Arc::new(snapshot);
        let owner = profiles::ProfileStore::capture();
        let reserved = RESERVATIONS.with(|slots| {
            slots
                .borrow()
                .iter()
                .rev()
                .find(|slot| {
                    std::ptr::eq(slot.queue.as_ref(), self)
                        && slot.owner == owner
                        && slot.remaining.get() > 0
                })
                .cloned()
        });
        #[cfg(test)]
        before_queue_admission();
        let mut state = self.state.lock().unwrap();
        if !profiles::authoritative_edits_allowed() {
            return Err("Profile reload in progress; edit not accepted".into());
        }
        if let Some(reserved) = reserved {
            // A producer admitted before shutdown must finish in this queue;
            // a synchronous fallback would overtake its earlier snapshots.
            reserved.remaining.set(reserved.remaining.get() - 1);
            state.reserved -= 1;
        } else {
            if state.stop || state.closing {
                return Ok(false);
            }
            if state.pending.len() + state.reserved >= MAX_PENDING {
                return Err(state
                    .error
                    .clone()
                    .unwrap_or_else(|| "persistence queue full; edit not accepted".into()));
            }
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
        let (target, sync_target) = {
            let state = self.state.lock().unwrap();
            (state.next, state.sync_requested)
        };
        self.wait_through(target, sync_target)
    }

    fn wait_through(&self, target: u64, sync_target: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if state.completed >= target && state.sync_finished >= sync_target {
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
        while state.completed < target || state.sync_finished < sync_target {
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
        Self::spawn_with_sync(save, nova_sync::flush_local_sync)
    }

    fn spawn_with_sync(
        save: impl Fn(&profiles::ProfileStore, &Snapshot) -> Result<(), String> + Send + 'static,
        notify_sync: impl Fn() + Send + 'static,
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
                            && !(state.sync_requested > state.sync_finished
                                && state.completed >= state.sync_requested)
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
                        if state.sync_requested > state.sync_finished
                            && state.completed >= state.sync_requested
                        {
                            let target = state.sync_requested;
                            drop(state);
                            // IN_WORKER makes the registered persistence
                            // barrier a no-op here: the requested snapshots
                            // have already committed on this ordered writer.
                            notify_sync();
                            let mut state = worker.state.lock().unwrap();
                            state.sync_finished = target;
                            worker.changed.notify_all();
                            continue;
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
                    if let Snapshot::External(_, done, _) = snapshot.as_ref() {
                        let _ = done.send(result.clone());
                    }
                    let mut state = worker.state.lock().unwrap();
                    let failure = result.as_ref().err().cloned();
                    match result {
                        Ok(()) => {
                            state.pending.pop_front();
                            state.head_started = false;
                            state.completed = sequence;
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error),
                    }
                    let blocked: Vec<_> = if failure.is_some() {
                        state
                            .pending
                            .iter()
                            .filter_map(|(_, _, snapshot)| match snapshot.as_ref() {
                                Snapshot::Operation(operation) => Some(operation.clone()),
                                _ => None,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let continuation = (state.reserved == 0 && state.pending.len() < MAX_PENDING)
                        .then(|| state.capacity_continuation.take())
                        .flatten();
                    worker.changed.notify_all();
                    drop(state);
                    if let Some(error) = failure {
                        for operation in blocked {
                            operation.failed(&error);
                        }
                    }
                    if let Some(continuation) = continuation {
                        continuation();
                    }
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
        // Admitted producers may still be completing their mutation. They
        // consume their reserved capacity even after closing is set.
        {
            let mut state = self.queue.state.lock().unwrap();
            while state.reserved > 0 {
                state = self.queue.changed.wait(state).unwrap();
            }
        }
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
        {
            let mut state = self.queue.state.lock().unwrap();
            state.stop = true;
            state.capacity_continuation.take();
        }
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

/// A fixed admission watermark: work arriving later cannot extend this wait.
/// It is captured on UI, but may only be waited on by a coordinator worker.
#[derive(Clone)]
pub(super) struct Watermark {
    queue: Option<Arc<Queue>>,
    sequence: u64,
    sync: u64,
}
impl Watermark {
    pub(super) fn wait(&self) -> Result<(), String> {
        self.queue
            .as_ref()
            .map_or(Ok(()), |queue| queue.wait_through(self.sequence, self.sync))
    }
}

pub(super) fn watermark() -> Watermark {
    let queue = ACTIVE.lock().unwrap().clone();
    let (sequence, sync) = queue.as_ref().map_or((0, 0), |queue| {
        let state = queue.state.lock().unwrap();
        (state.next, state.sync_requested)
    });
    Watermark {
        queue,
        sequence,
        sync,
    }
}

pub(super) fn try_admission_revision() -> Option<u64> {
    let queue = ACTIVE.try_lock().ok()?.clone();
    match queue {
        Some(queue) => Some(queue.state.try_lock().ok()?.next),
        None => Some(0),
    }
}
pub(super) fn admission_revision() -> u64 {
    watermark().sequence
}

pub(super) struct TransitionWait {
    watermark: Watermark,
    fallback: Option<Arc<profile_actions::OutgoingTransition>>,
    owner: profiles::ProfileStore,
}
impl TransitionWait {
    pub(super) fn queued(&self) -> bool {
        self.fallback.is_none()
    }
    pub(super) fn wait(self) -> Result<(), String> {
        self.watermark.wait()?;
        if let Some(capture) = self.fallback {
            capture.save(&self.owner)?;
        }
        Ok(())
    }
}

pub(super) fn enqueue_transition(
    capture: profile_actions::OutgoingTransition,
) -> Result<TransitionWait, String> {
    let owner = profiles::ProfileStore::capture();
    let capture = Arc::new(capture);
    let queued = enqueue(Snapshot::Transition(capture.clone()))?;
    Ok(TransitionWait {
        watermark: watermark(),
        fallback: (!queued).then_some(capture),
        owner,
    })
}
pub(super) fn flush_report() {
    if let Err(error) = flush() {
        io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
    }
}

/// Hand off pause/completion/close sync without waiting on the event loop.
/// Real lifecycle/profile barriers still use flush/flush_report. Notification
/// follows successful persistence, including retries, on the existing writer.
pub(super) fn request_local_sync() {
    let queue = ACTIVE.lock().unwrap().clone();
    if let Some(queue) = queue.filter(|_| !IN_WORKER.get()) {
        queue.request_sync();
    } else {
        // Bootstrap/no-writer paths already persist synchronously.
        nova_sync::flush_local_sync();
    }
}
pub(super) fn failed() -> bool {
    let queue = ACTIVE.lock().unwrap().clone();
    queue.is_some_and(|queue| queue.state.lock().unwrap().error.is_some())
}

/// Close under the same lock used for the final admission checks. A producer
/// either admitted before this close (and appears in the following watermark),
/// or observes the closed source and cannot enqueue a stale snapshot.
pub(super) fn close_import_admission(
    session: &Arc<profile_session::ProfileSession>,
) -> Option<u64> {
    let queue = ACTIVE.lock().unwrap().clone();
    if let Some(queue) = queue {
        let _state = queue.state.lock().unwrap();
        session.close_edit_admission()
    } else {
        session.close_edit_admission()
    }
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
    if !profiles::authoritative_edits_allowed() {
        return None;
    }
    // Without the ordered writer there is nowhere to retain a failed save.
    // Leave Shared untouched and delivery pending rather than launch a
    // one-shot write whose failure could later look like a successful no-op.
    let queue = ACTIVE.lock().unwrap().clone()?;
    let (done, receipt) = mpsc::channel();
    let snapshot = Arc::new(Snapshot::External(Box::new(patch), done, projection));
    let owner = profiles::ProfileStore::capture();
    #[cfg(test)]
    before_queue_admission();
    let mut state = queue.state.try_lock().ok()?;
    if !profiles::authoritative_edits_allowed()
        || state.pending.len() + state.reserved >= MAX_PENDING
        || state.reserved > 0
        || state.stop
        || state.closing
    {
        return None;
    }
    state.next += 1;
    let sequence = state.next;
    state.pending.push_back((sequence, owner, snapshot));
    queue.changed.notify_all();
    Some(receipt)
}

#[cfg(test)]
pub(super) fn running() -> bool {
    ACTIVE.lock().unwrap().is_some()
}
fn enqueue(snapshot: Snapshot) -> Result<bool, String> {
    if !profiles::authoritative_edits_allowed() {
        return Err("Profile reload in progress; edit not accepted".into());
    }
    if applying() || IN_WORKER.get() {
        return Ok(false);
    }
    let queue = ACTIVE.lock().unwrap().clone();
    let Some(queue) = queue else {
        return Ok(false);
    };
    if queue.enqueue(snapshot)? {
        Ok(true)
    } else {
        Err("persistence worker closing; edit not accepted".into())
    }
}
pub(super) fn write_library(entries: &[LibraryEntry]) {
    // Callers reserve capacity before touching Shared. Invalidate projections
    // before the admitted snapshot can become visible to other readers.
    progress_projection::invalidate();
    match enqueue(Snapshot::Library(entries.to_vec())) {
        Ok(true) => {}
        Ok(false) => write_json("library", entries),
        Err(error) => {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error))
        }
    }
}
pub(super) fn write_progress(map: &HashMap<String, EpisodeProgress>) {
    progress_projection::invalidate();
    match enqueue(Snapshot::Progress(
        map.clone(),
        nova_sync::is_playback_checkpoint(),
    )) {
        Ok(true) => unified_search::progress_admitted(&profiles::ProfileStore::capture(), map),
        Ok(false) => write_json(EPISODE_PROGRESS_KEY, map),
        Err(error) => {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error))
        }
    }
}
pub(super) fn pending_library() -> Option<Vec<LibraryEntry>> {
    if IN_WORKER.get() {
        return None;
    }
    let queue = ACTIVE.lock().unwrap().clone()?;
    let current = profiles::ProfileStore::capture();
    let (base, edits) = {
        let state = queue.state.lock().unwrap();
        let mut base = None;
        let mut edits = Vec::new();
        for (_, owner, snapshot) in state.pending.iter().rev() {
            if owner != &current {
                continue;
            }
            match snapshot.as_ref() {
                Snapshot::Library(_) => {
                    base = Some(snapshot.clone());
                    break;
                }
                Snapshot::External(patch, _, _) if patch.library.is_some() => {
                    edits.push(snapshot.clone())
                }
                _ => {}
            }
        }
        (base, edits)
    };
    if base.is_none() && edits.is_empty() {
        return None;
    }
    // The handles above describe one coherent queue view. Clone and apply its
    // immutable payloads after releasing the mutex, preserving chronology.
    let mut entries = base.map_or_else(
        || read_json("library").unwrap_or_default(),
        |snapshot| match snapshot.as_ref() {
            Snapshot::Library(entries) => entries.clone(),
            _ => unreachable!(),
        },
    );
    for snapshot in edits.into_iter().rev() {
        if let Snapshot::External(patch, _, _) = snapshot.as_ref()
            && let Some(edit) = &patch.library
        {
            edit.apply(&mut entries);
        }
    }
    Some(entries)
}
pub(super) fn pending_progress() -> Option<HashMap<String, EpisodeProgress>> {
    if IN_WORKER.get() {
        return None;
    }
    let queue = ACTIVE.lock().unwrap().clone()?;
    let current = profiles::ProfileStore::capture();
    let (base, edits) = {
        let state = queue.state.lock().unwrap();
        let mut base = None;
        let mut edits = Vec::new();
        for (_, owner, snapshot) in state.pending.iter().rev() {
            if owner != &current {
                continue;
            }
            match snapshot.as_ref() {
                Snapshot::Progress(_, _) => {
                    base = Some(snapshot.clone());
                    break;
                }
                Snapshot::Transition(capture) if capture.progress.is_some() => {
                    edits.push(snapshot.clone());
                }
                Snapshot::External(patch, _, _) if !patch.progress.is_empty() => {
                    edits.push(snapshot.clone())
                }
                _ => {}
            }
        }
        (base, edits)
    };
    if base.is_none() && edits.is_empty() {
        return None;
    }
    let mut progress = base.map_or_else(
        || read_json(EPISODE_PROGRESS_KEY).unwrap_or_default(),
        |snapshot| match snapshot.as_ref() {
            Snapshot::Progress(map, _) => map.clone(),
            _ => unreachable!(),
        },
    );
    for snapshot in edits.into_iter().rev() {
        if let Snapshot::External(patch, _, _) = snapshot.as_ref() {
            progress.extend(patch.progress.clone());
        }
        if let Snapshot::Transition(capture) = snapshot.as_ref() {
            progress.extend(capture.progress.as_ref().unwrap().clone());
        }
    }
    Some(progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_closes_before_watermark_and_rejects_delayed_old_context_admissions() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::import_closes_before_watermark_and_rejects_delayed_old_context_admissions"
            ),
            |fixture| {
                for mode in 0..3 {
                    fixture.stop_actors();
                    flush().unwrap();
                    let bridge = &fixture.bridge;
                    let source = profiles::active_context();
                    let owner = profiles::ProfileStore::for_profile(&source.id);
                    let stale: LibraryEntry = serde_json::from_value(serde_json::json!({
                        "id":"stale", "type_":"movie", "name":"Before import", "added_at_secs":1,
                    }))
                    .unwrap();
                    owner
                        .persist_snapshot(
                            "library",
                            &serde_json::to_string(&vec![stale.clone()]).unwrap(),
                            false,
                        )
                        .unwrap();
                    bridge.shared.lock().unwrap().entries = vec![stale.clone()];
                    let (paused, pausing) = mpsc::channel();
                    let (release, released) = mpsc::channel();
                    let (admitted, admission) = mpsc::channel();
                    let worker = std::thread::spawn({
                        let source = source.clone();
                        move || {
                            profiles::with_context(source, || {
                                let accepted = with_admission_pause(
                                    move || {
                                        paused.send(()).unwrap();
                                        released.recv_timeout(Duration::from_secs(5)).unwrap();
                                    },
                                    || match mode {
                                        0 => reserve(1).is_some(),
                                        1 => enqueue(Snapshot::Library(vec![stale.clone()]))
                                            .is_ok_and(|queued| queued),
                                        _ => try_external(ExternalPatch {
                                            library: Some(ExternalLibraryEdit {
                                                id: stale.id.clone(),
                                                media_type: stale.type_.clone(),
                                                entry: Some(stale),
                                            }),
                                            ..Default::default()
                                        })
                                        .is_some(),
                                    },
                                );
                                admitted.send(accepted).unwrap();
                            })
                        }
                    });
                    pausing.recv_timeout(Duration::from_secs(5)).unwrap();
                    let closed = Arc::new(AtomicBool::new(false));
                    profile_policy::with_import_wait_hook(
                        {
                            let closed = closed.clone();
                            move || {
                                assert!(
                                    !profiles::authoritative_edits_allowed(),
                                    "source must close before the final wait"
                                );
                                closed.store(true, Ordering::Release);
                                release.send(()).unwrap();
                                assert!(
                                    !admission.recv_timeout(Duration::from_secs(5)).unwrap(),
                                    "late admission mode {mode} passed the closed source"
                                );
                            }
                        },
                        || bridge.profile_import_prepare(std::slice::from_ref(&source.id)),
                    )
                    .unwrap();
                    worker.join().unwrap();
                    assert!(closed.load(Ordering::Acquire));
                    let imported: LibraryEntry = serde_json::from_value(serde_json::json!({
                        "id":"imported", "type_":"movie", "name":format!("Imported {mode}"), "added_at_secs":2,
                    })).unwrap();
                    let raw = serde_json::to_string(&vec![imported.clone()]).unwrap();
                    owner.persist_snapshot("library", &raw, false).unwrap();
                    // Registered collections canonicalize object-key order.
                    // Retain the committed bytes as the exact no-overwrite
                    // receipt, after checking the imported typed contents.
                    let raw = owner.read("library").unwrap().unwrap();
                    assert_eq!(
                        serde_json::from_str::<Vec<LibraryEntry>>(&raw).unwrap(),
                        vec![imported.clone()]
                    );
                    flush().unwrap();
                    assert_eq!(
                        owner.read("library").unwrap().as_deref(),
                        Some(raw.as_str())
                    );
                    bridge.profile_import_committed(std::slice::from_ref(&source.id));
                    bridge.wait_profile_activation();
                    assert_ne!(profiles::active_context().epoch, source.epoch);
                    assert_eq!(bridge.shared.lock().unwrap().entries, vec![imported]);
                    assert_eq!(
                        owner.read("library").unwrap().as_deref(),
                        Some(raw.as_str())
                    );
                }
            },
        );
    }

    #[test]
    fn failed_pre_import_wait_reopens_only_its_owned_current_allocation() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::failed_pre_import_wait_reopens_only_its_owned_current_allocation"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let source = profiles::active_context();
                bridge.app().unwrap().set_player_open(true);
                let key = progress_map_key("schema-fixture", "episode");
                let mut failure_map = HashMap::from([(
                    key.clone(),
                    EpisodeProgress {
                        series_id: "schema-fixture".into(),
                        episode_id: "episode".into(),
                        position_secs: 1.0,
                        duration_secs: 100.0,
                        ..Default::default()
                    },
                )]);
                nova_storage::register_collection(
                    "sync:records:hlc",
                    nova_storage::CollectionKind::Array {
                        id_field: "id".into(),
                    },
                );
                write_progress(&failure_map);
                assert!(flush().is_err());
                assert!(
                    bridge
                        .profile_import_prepare(std::slice::from_ref(&source.id))
                        .is_err()
                );
                assert_eq!(profiles::active_context(), source);
                assert!(
                    profiles::authoritative_edits_allowed(),
                    "failed quiescence must restore its prior usable admission state"
                );
                assert!(bridge.app().unwrap().get_player_open());
                assert_eq!(
                    bridge.profile_loaded_epoch.load(Ordering::Acquire),
                    source.epoch
                );
                nova_storage::register_collection(
                    "sync:records:hlc",
                    nova_storage::CollectionKind::Object,
                );
                flush().unwrap();
                assert!(reserve(1).is_some());

                // A newer close invalidates the failing attempt's ticket.
                nova_storage::register_collection(
                    "sync:records:hlc",
                    nova_storage::CollectionKind::Array {
                        id_field: "id".into(),
                    },
                );
                failure_map.get_mut(&key).unwrap().position_secs = 2.0;
                write_progress(&failure_map);
                assert!(flush().is_err());
                let allocation = bridge.sessions.active();
                let newer = allocation.clone();
                assert!(
                    profile_policy::with_import_wait_hook(
                        move || newer.block_authoritative_edits(),
                        || bridge.profile_import_prepare(std::slice::from_ref(&source.id))
                    )
                    .is_err()
                );
                assert!(
                    !allocation.authoritative_edits_allowed(),
                    "an older failure cannot undo a newer close"
                );
                assert!(
                    bridge
                        .profile_import_prepare(std::slice::from_ref(&source.id))
                        .is_err()
                );
                assert!(
                    !allocation.authoritative_edits_allowed(),
                    "an already closed reload is never reopened by a failed import"
                );
                nova_storage::register_collection(
                    "sync:records:hlc",
                    nova_storage::CollectionKind::Object,
                );
                flush().unwrap();
            },
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn pause_progress_hands_sync_to_the_writer_without_waiting_for_disk() {
        const ROOT: &str = "NOVA_PAUSE_PERSISTENCE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-pause-persistence-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::persistence::tests::pause_progress_hands_sync_to_the_writer_without_waiting_for_disk", "--nocapture"])
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
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, beginning) = mpsc::channel();
        let (notified, notification) = mpsc::channel();
        let owner = profiles::ProfileStore::capture();
        let key = progress_map_key("series", "episode");
        let notify_key = key.clone();
        let worker = Worker::spawn_with_sync(
            {
                let gate = gate.clone();
                move |owner, snapshot| {
                    let _ = started.send(());
                    let mut released = gate.0.lock().unwrap();
                    while !*released {
                        released = gate.1.wait(released).unwrap();
                    }
                    drop(released);
                    snapshot.save(owner)
                }
            },
            move || {
                // Exercise the real registered barrier on the writer, where it
                // must never recursively wait for this notification to finish.
                nova_sync::flush_local_sync();
                let raw = owner.read(EPISODE_PROGRESS_KEY).unwrap().unwrap();
                let map: HashMap<String, EpisodeProgress> = serde_json::from_str(&raw).unwrap();
                notified.send(map[&notify_key].position_secs).unwrap();
            },
        )
        .unwrap();
        *ACTIVE.lock().unwrap() = Some(worker.queue.clone());
        nova_sync::set_persistence_barrier(Arc::new(|| {
            flush()
                .map_err(|error| storage::Error::new(storage::ErrorKind::Transaction, error).into())
        }));
        let (returned, waiting) = mpsc::channel();
        let waited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = thread::spawn({
            let gate = gate.clone();
            let waited = waited.clone();
            move || {
                // Old synchronous pause code can finish only after this
                // watchdog releases disk; that path deterministically fails.
                if waiting.recv_timeout(Duration::from_secs(2)).is_err() {
                    waited.store(true, Ordering::Release);
                }
                *gate.0.lock().unwrap() = true;
                gate.1.notify_all();
            }
        });
        worker.queue.enqueue(progress(1.0, true)).unwrap();
        beginning.recv_timeout(Duration::from_secs(5)).unwrap();
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_is_paused(true);
        app.set_position(25.0);
        app.set_duration(1000.0);
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode".into(),
            last_saved_pos: 10.0,
            last_saved_at: now_secs(),
            ..Default::default()
        });
        bridge
            .dispatch_playback_action(nova_playback_api::PlaybackAction::SetPaused(true))
            .unwrap();
        assert!(app.get_is_paused());
        assert_eq!(
            bridge.shared.lock().unwrap().progress[&key].position_secs,
            25.0
        );
        for _ in 0..20 {
            bridge.note_player_progress_from_ui();
        }
        assert!(
            !waited.load(Ordering::Acquire),
            "Pause UI must return before the writer is released"
        );
        assert!(
            notification.try_recv().is_err(),
            "Sync cannot observe a merely queued checkpoint"
        );
        {
            let state = worker.queue.state.lock().unwrap();
            assert_eq!(
                state.next, 2,
                "Stationary paused ticks must not enqueue repeated snapshots"
            );
            assert_eq!(state.sync_requested, 2);
            assert_eq!(state.sync_finished, 0);
        }
        returned.send(()).unwrap();
        release.join().unwrap();
        worker.queue.flush().unwrap();
        assert_eq!(
            notification.recv_timeout(Duration::from_secs(5)).unwrap(),
            25.0,
            "Notification follows the durable pause snapshot"
        );
        assert!(
            notification.try_recv().is_err(),
            "Repeated pause ticks coalesce into one handoff"
        );
        drop(worker);
    }

    #[test]
    fn sync_watermarks_survive_inflight_notification_and_coalesced_profile_saves() {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let (began, beginning) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (notified, notifications) = mpsc::channel();
        let calls = std::cell::Cell::new(0);
        let worker = Worker::spawn_with_sync(
            {
                let saved = saved.clone();
                move |owner, snapshot| {
                    saved
                        .lock()
                        .unwrap()
                        .push((owner.id().to_string(), position(snapshot)));
                    Ok(())
                }
            },
            {
                let saved = saved.clone();
                move || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 1 {
                        began.send(()).unwrap();
                        let _ = gate.recv_timeout(Duration::from_secs(5));
                    }
                    notified.send(saved.lock().unwrap().clone()).unwrap();
                }
            },
        )
        .unwrap();
        profiles::with_context(profiles::context_for("alice"), || {
            worker.queue.enqueue(progress(1.0, false)).unwrap()
        });
        worker.queue.request_sync();
        beginning.recv_timeout(Duration::from_secs(5)).unwrap();
        profiles::with_context(profiles::context_for("bob"), || {
            worker.queue.enqueue(progress(2.0, true)).unwrap();
            worker.queue.request_sync();
            worker.queue.enqueue(progress(3.0, true)).unwrap();
        });
        assert_eq!(worker.queue.state.lock().unwrap().pending.len(), 1);
        release.send(()).unwrap();
        worker.queue.flush().unwrap();
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(5)).unwrap(),
            vec![("alice".into(), 1.0)]
        );
        assert_eq!(
            notifications.recv_timeout(Duration::from_secs(5)).unwrap(),
            vec![("alice".into(), 1.0), ("bob".into(), 3.0)],
            "Completing old notification cannot erase a newer request; a coalesced sequence satisfies the older watermark"
        );
        assert!(notifications.try_recv().is_err());
    }

    #[test]
    fn failed_checkpoint_retains_sync_handoff_until_retry_commits() {
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (attempted, attempts) = mpsc::channel();
        let (notified, notification) = mpsc::channel();
        let worker = Worker::spawn_with_sync(
            {
                let failing = failing.clone();
                move |_, _| {
                    attempted.send(()).unwrap();
                    if failing.load(Ordering::Acquire) {
                        Err("injected disk failure".into())
                    } else {
                        Ok(())
                    }
                }
            },
            move || {
                notified.send(()).unwrap();
            },
        )
        .unwrap();
        worker.queue.enqueue(progress(1.0, false)).unwrap();
        worker.queue.request_sync();
        attempts.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(worker.queue.flush().is_err());
        assert!(
            notification.try_recv().is_err(),
            "A failed transaction cannot notify sync"
        );
        assert_eq!(worker.queue.state.lock().unwrap().sync_requested, 1);
        failing.store(false, Ordering::Release);
        worker.queue.flush().unwrap();
        notification.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(worker.queue.state.lock().unwrap().sync_finished, 1);
    }

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
    fn reservations_are_atomic_bounded_and_owned_by_the_issuing_profile() {
        let queue = Arc::new(Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        for n in 0..MAX_PENDING - 1 {
            queue.enqueue(progress(n as f64, false)).unwrap();
        }
        assert!(
            queue.reserve(2).is_err(),
            "compound edits need all slots before changing either map"
        );
        assert_eq!(queue.state.lock().unwrap().reserved, 0);
        let reservation =
            profiles::with_context(profiles::context_for("alice"), || queue.reserve(1).unwrap());
        assert!(queue.reserve(1).is_err());
        profiles::with_context(profiles::context_for("bob"), || {
            assert!(
                queue.enqueue(progress(999.0, false)).is_err(),
                "another owner cannot consume Alice's slot"
            );
        });
        profiles::with_context(profiles::context_for("alice"), || {
            queue.enqueue(progress(64.0, false)).unwrap();
        });
        assert_eq!(queue.state.lock().unwrap().pending.len(), MAX_PENDING);
        assert_eq!(queue.state.lock().unwrap().reserved, 0);
        drop(reservation);
        let fresh = Arc::new(Queue {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let reservation = fresh.reserve(2).unwrap();
        assert_eq!(fresh.state.lock().unwrap().reserved, 2);
        drop(reservation);
        assert_eq!(fresh.state.lock().unwrap().reserved, 0);
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn full_failed_queue_rejects_public_edits_before_shared_changes_and_recovers_order() {
        const ROOT: &str = "NOVA_PERSISTENCE_ADMISSION_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-persistence-admission-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::persistence::tests::full_failed_queue_rejects_public_edits_before_shared_changes_and_recovers_order", "--nocapture"])
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
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
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
        });
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let failing = Arc::new(AtomicBool::new(true));
        let saved = Arc::new(Mutex::new(Vec::new()));
        let (started, entered) = mpsc::channel();
        let worker = Worker::spawn({
            let gate = gate.clone();
            let failing = failing.clone();
            let saved = saved.clone();
            move |_, snapshot| {
                let _ = started.send(());
                let mut ready = gate.0.lock().unwrap();
                while !*ready {
                    ready = gate.1.wait(ready).unwrap();
                }
                if failing.load(Ordering::Acquire) {
                    return Err("injected persistence failure".into());
                }
                saved.lock().unwrap().push(snapshot.clone());
                Ok(())
            }
        })
        .unwrap();
        *ACTIVE.lock().unwrap() = Some(worker.queue.clone());
        // Hold the head in flight so exactly 64 nonperiodic saves are admitted.
        for n in 1..=MAX_PENDING {
            let _reservation = reserve(1).unwrap();
            let Snapshot::Progress(map, _) = progress(n as f64, false) else {
                unreachable!()
            };
            write_progress(&map);
        }
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        // An external patch cannot cross a reserved local mutation, even when
        // there would otherwise be room for that patch.
        assert!(try_external(ExternalPatch::default()).is_none());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(flush().is_err());
        let before = bridge.shared.lock().unwrap().progress.clone();
        let failures = io::write_failures();
        bridge.episode_toggle_watched(0);
        bridge.toggle_current_in_library();
        assert_eq!(bridge.shared.lock().unwrap().progress, before);
        assert!(bridge.shared.lock().unwrap().entries.is_empty());
        assert_eq!(io::write_failures(), failures + 2);
        assert_eq!(
            worker.queue.state.lock().unwrap().pending.len(),
            MAX_PENDING
        );
        assert!(
            enqueue(progress(65.0, false)).is_err(),
            "the public admission wrapper must report rejection"
        );
        failing.store(false, Ordering::Release);
        flush().unwrap();
        assert_eq!(
            saved
                .lock()
                .unwrap()
                .iter()
                .map(position)
                .collect::<Vec<_>>(),
            (1..=MAX_PENDING).map(|n| n as f64).collect::<Vec<_>>()
        );
        {
            let _reservation = reserve(2).unwrap();
            assert!(
                try_external(ExternalPatch::default()).is_none(),
                "external projection must retry until local reservations are enqueued"
            );
            // A barrier may run during an admitted action (e.g. settings or
            // download cleanup); it must not wait on this thread's own slots.
            flush().unwrap();
        }
        bridge.episode_watch_action(0, 1);
        bridge.episode_watch_action(0, 2);
        flush().unwrap();
        let snapshots = saved.lock().unwrap();
        let tail = snapshots[snapshots.len() - 2..]
            .iter()
            .map(|snapshot| {
                let Snapshot::Progress(map, _) = snapshot else {
                    unreachable!()
                };
                map[&progress_map_key("source", "episode")].clone()
            })
            .collect::<Vec<_>>();
        assert!(tail[0].watched);
        assert!(!tail[1].watched);
        assert_ne!(tail[0].manual_watch_action, tail[1].manual_watch_action);
        drop(snapshots);
        drop(worker);
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
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::spawn({
            let failing = failing.clone();
            let saved = saved.clone();
            let attempts = attempts.clone();
            move |_, snapshot| {
                attempts
                    .lock()
                    .unwrap()
                    .push((position(snapshot), std::ptr::from_ref(snapshot) as usize));
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
        let queued_head = {
            let state = worker.queue.state.lock().unwrap();
            assert_eq!(state.pending.len(), 2);
            state.pending.front().unwrap().2.clone()
        };
        assert_eq!(position(&queued_head), 1.0);
        assert!(saved.lock().unwrap().is_empty());
        failing.store(false, Ordering::Relaxed);
        worker.queue.flush().unwrap();
        assert_eq!(*saved.lock().unwrap(), vec![1.0, 0.0]);
        let attempts = attempts.lock().unwrap();
        let head_attempts = attempts.iter().filter(|(position, _)| *position == 1.0);
        assert!(head_attempts.clone().count() >= 2);
        for (_, pointer) in head_attempts {
            assert_eq!(
                *pointer,
                Arc::as_ptr(&queued_head) as usize,
                "failed and successful attempts must borrow the queued payload itself"
            );
        }
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

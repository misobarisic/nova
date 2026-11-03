//! Bounded desktop intake. This module owns scheduling only, never pixels or UI.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) const VISIBLE_CAPACITY: usize = 128;
pub(super) const BACKGROUND_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Key {
    pub owner: String,
    pub epoch: u64,
    pub generation: u64,
    pub revision: u64,
    pub authority: (u64, u64, Arc<str>),
    pub grid: &'static str,
    pub media_type: String,
    pub id: String,
    pub source: String,
    pub projection: String,
    pub index: usize,
    pub url: String,
    pub recipe: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Priority {
    Visible,
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Queued,
    Promoted,
    AlreadyActive,
    DeferredCapacity,
    Closed,
    Obsolete,
}

struct Pending<T> {
    job: T,
    priority: Priority,
    entered: Option<Instant>,
}
struct Active {
    ticket: u64,
    live: Arc<AtomicBool>,
}
struct State<T> {
    high: VecDeque<Key>,
    low: VecDeque<Key>,
    pending: HashMap<Key, Pending<T>>,
    active: HashMap<Key, Active>,
    closed: bool,
    ticket: u64,
    retry_posted: bool,
    retry_needed: bool,
    peak_pending: usize,
    peak_active: usize,
    deferred: u64,
    obsolete: u64,
    promoted: u64,
    deduplicated: u64,
}

pub(super) struct Queue<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
    generation: Arc<AtomicU64>,
    revision: Arc<dyn Fn() -> u64 + Send + Sync>,
    workers: usize,
    timing: bool,
    retry: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(super) struct Work<T> {
    pub job: T,
    pub completion: Completion<T>,
    pub priority: Priority,
    pub queue_wait: Option<Duration>,
}

/// The final UI delivery (or its failed dispatch) owns this registration. A
/// canceled running request still spends its lane until its work really ends.
pub(super) struct Completion<T> {
    queue: Arc<Queue<T>>,
    key: Key,
    ticket: u64,
    live: Arc<AtomicBool>,
}
impl<T> Completion<T> {
    pub fn retry_current(&self) {
        self.queue.request_retry();
    }
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire) && self.queue.current(&self.key)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    pub visible: usize,
    pub background: usize,
    pub active: usize,
    pub peak_pending: usize,
    pub peak_active: usize,
    pub deferred: u64,
    pub obsolete: u64,
    pub promoted: u64,
    pub deduplicated: u64,
}

impl<T> Queue<T> {
    pub fn new(
        generation: Arc<AtomicU64>,
        revision: Arc<dyn Fn() -> u64 + Send + Sync>,
        workers: usize,
        timing: bool,
    ) -> Arc<Self> {
        assert!((1..=8).contains(&workers));
        Arc::new(Self {
            state: Mutex::new(State {
                high: VecDeque::new(),
                low: VecDeque::new(),
                pending: HashMap::new(),
                active: HashMap::new(),
                closed: false,
                ticket: 0,
                retry_posted: false,
                retry_needed: false,
                peak_pending: 0,
                peak_active: 0,
                deferred: 0,
                obsolete: 0,
                promoted: 0,
                deduplicated: 0,
            }),
            ready: Condvar::new(),
            generation,
            revision,
            workers,
            timing,
            retry: Mutex::new(None),
        })
    }
    fn current(&self, key: &Key) -> bool {
        key.generation == self.generation.load(Ordering::Acquire)
            && key.revision == (self.revision)()
    }
    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }
    pub fn set_retry(&self, retry: Arc<dyn Fn() + Send + Sync>) {
        *self.retry.lock().unwrap() = Some(retry);
    }
    pub fn retry_started(&self) {
        let mut state = self.state.lock().unwrap();
        state.retry_posted = false;
        state.retry_needed = false;
    }
    pub fn submit(&self, key: Key, job: T, priority: Priority) -> Admission {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Admission::Closed;
        }
        if !self.current(&key) {
            state.obsolete += 1;
            return Admission::Obsolete;
        }
        if state.active.contains_key(&key) {
            state.deduplicated += 1;
            return Admission::AlreadyActive;
        }
        if let Some(existing) = state.pending.get(&key) {
            if existing.priority == Priority::Background && priority == Priority::Visible {
                if state.high.len() == VISIBLE_CAPACITY {
                    state.deferred += 1;
                    state.retry_needed = true;
                    return Admission::DeferredCapacity;
                }
                state.low.retain(|old| old != &key);
                let timing = self.timing.then(Instant::now);
                let existing = state.pending.get_mut(&key).unwrap();
                existing.priority = Priority::Visible;
                existing.entered = timing;
                state.high.push_back(key);
                state.promoted += 1;
                drop(state);
                self.ready.notify_one();
                return Admission::Promoted;
            }
            state.deduplicated += 1;
            return Admission::Queued;
        }
        let full = match priority {
            Priority::Visible => state.high.len() == VISIBLE_CAPACITY,
            Priority::Background => state.low.len() == BACKGROUND_CAPACITY,
        };
        if full {
            state.deferred += 1;
            if priority == Priority::Visible {
                state.retry_needed = true;
            }
            return Admission::DeferredCapacity;
        }
        state.pending.insert(
            key.clone(),
            Pending {
                job,
                priority,
                entered: self.timing.then(Instant::now),
            },
        );
        match priority {
            Priority::Visible => state.high.push_back(key),
            Priority::Background => state.low.push_back(key),
        }
        state.peak_pending = state.peak_pending.max(state.pending.len());
        drop(state);
        self.ready.notify_one();
        Admission::Queued
    }
    pub fn invalidate(&self) {
        let mut state = self.state.lock().unwrap();
        let stale = state
            .pending
            .keys()
            .filter(|key| !self.current(key))
            .cloned()
            .collect::<Vec<_>>();
        let removed = stale
            .into_iter()
            .filter_map(|key| state.pending.remove(&key))
            .collect::<Vec<_>>();
        state.high.retain(|key| self.current(key));
        state.low.retain(|key| self.current(key));
        state.obsolete += removed.len() as u64;
        if !removed.is_empty() || state.active.keys().any(|key| !self.current(key)) {
            state.retry_needed = true;
        }
        for (key, entry) in &state.active {
            if !self.current(key) {
                entry.live.store(false, Ordering::Release);
            }
        }
        drop(state);
        drop(removed);
        self.ready.notify_all();
        self.post_retry();
    }
    pub fn take(self: &Arc<Self>) -> Option<Work<T>> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.closed {
                return None;
            }
            if state.active.len() < self.workers
                && let Some(key) = state.high.pop_front().or_else(|| state.low.pop_front())
            {
                let pending = state.pending.remove(&key).unwrap();
                if !self.current(&key) {
                    state.obsolete += 1;
                    // Release ownership outside the scheduler lock.
                    drop(state);
                    drop(pending);
                    self.request_retry();
                    state = self.state.lock().unwrap();
                    continue;
                }
                state.ticket = state.ticket.wrapping_add(1);
                let ticket = state.ticket;
                let live = Arc::new(AtomicBool::new(true));
                state.active.insert(
                    key.clone(),
                    Active {
                        ticket,
                        live: live.clone(),
                    },
                );
                state.peak_active = state.peak_active.max(state.active.len());
                return Some(Work {
                    job: pending.job,
                    priority: pending.priority,
                    queue_wait: pending.entered.map(|entered| entered.elapsed()),
                    completion: Completion {
                        queue: self.clone(),
                        key,
                        ticket,
                        live,
                    },
                });
            }
            // No periodic polling. Either lane, close or a released delivery
            // permit wakes this same condition variable.
            state = self.ready.wait(state).unwrap();
        }
    }
    fn request_retry(&self) {
        self.state.lock().unwrap().retry_needed = true;
        self.post_retry();
    }
    fn post_retry(&self) {
        let post = {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.retry_posted || !state.retry_needed {
                false
            } else {
                state.retry_posted = true;
                true
            }
        };
        if post {
            let retry = self.retry.lock().unwrap().clone();
            if let Some(retry) = retry {
                retry();
            }
        }
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.high.clear();
        state.low.clear();
        let pending = std::mem::take(&mut state.pending);
        for entry in state.active.values() {
            entry.live.store(false, Ordering::Release);
        }
        drop(state);
        drop(pending);
        self.ready.notify_all();
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        Snapshot {
            visible: state.high.len(),
            background: state.low.len(),
            active: state.active.len(),
            peak_pending: state.peak_pending,
            peak_active: state.peak_active,
            deferred: state.deferred,
            obsolete: state.obsolete,
            promoted: state.promoted,
            deduplicated: state.deduplicated,
        }
    }
}
impl<T> Drop for Completion<T> {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock().unwrap();
        if state
            .active
            .get(&self.key)
            .is_some_and(|entry| entry.ticket == self.ticket)
        {
            state.active.remove(&self.key);
            self.live.store(false, Ordering::Release);
            if !self.queue.current(&self.key) {
                state.retry_needed = true;
            }
        }
        drop(state);
        self.queue.ready.notify_all();
        self.queue.post_retry();
    }
}

pub(super) fn worker_count(requested: Option<&str>, parallelism: Option<usize>) -> usize {
    requested
        .and_then(|value| value.parse().ok())
        .filter(|count| (1..=8).contains(count))
        .unwrap_or_else(|| parallelism.unwrap_or(4).clamp(1, 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn key(index: usize) -> Key {
        Key {
            owner: "alice".into(),
            epoch: 1,
            generation: 1,
            revision: 1,
            authority: (1, 1, "inventory".into()),
            grid: "catalog",
            media_type: "movie".into(),
            id: index.to_string(),
            source: "source".into(),
            projection: index.to_string(),
            index,
            url: format!("https://test/{index}"),
            recipe: "poster".into(),
        }
    }
    fn queue(workers: usize) -> Arc<Queue<usize>> {
        Queue::new(Arc::new(AtomicU64::new(1)), Arc::new(|| 1), workers, false)
    }

    #[test]
    fn poster_queue_bounds_and_visible_priority() {
        let queue = queue(2);
        for index in 0..BACKGROUND_CAPACITY {
            assert_eq!(
                queue.submit(key(index), index, Priority::Background),
                Admission::Queued
            );
        }
        for index in BACKGROUND_CAPACITY..BACKGROUND_CAPACITY + VISIBLE_CAPACITY {
            assert_eq!(
                queue.submit(key(index), index, Priority::Visible),
                Admission::Queued
            );
        }
        assert_eq!(
            queue.submit(key(999), 999, Priority::Visible),
            Admission::DeferredCapacity
        );
        assert_eq!(
            queue.submit(key(998), 998, Priority::Background),
            Admission::DeferredCapacity
        );
        let first = queue.take().unwrap();
        assert_eq!(first.job, BACKGROUND_CAPACITY);
        let second = queue.take().unwrap();
        assert_eq!(second.job, BACKGROUND_CAPACITY + 1);
        assert_eq!(queue.snapshot().peak_pending, 384);
        assert_eq!(queue.snapshot().active, 2);
        queue.close();
        assert!(queue.take().is_none());
    }

    #[test]
    fn poster_queue_deduplicates_and_promotes_exact_request() {
        let queue = queue(1);
        queue.submit(key(1), 1, Priority::Background);
        queue.submit(key(2), 2, Priority::Background);
        assert_eq!(
            queue.submit(key(2), 9, Priority::Visible),
            Admission::Promoted
        );
        assert_eq!(
            queue.submit(key(2), 9, Priority::Visible),
            Admission::Queued
        );
        let work = queue.take().unwrap();
        assert_eq!(work.job, 2);
        assert_eq!(
            queue.submit(key(2), 9, Priority::Visible),
            Admission::AlreadyActive
        );
        drop(work);
        assert_eq!(queue.snapshot().active, 0);
        assert_eq!(queue.take().unwrap().job, 1);
    }

    #[test]
    fn poster_queue_keeps_typed_profile_source_recipe_authority_distinct() {
        let queue = queue(1);
        let original = key(1);
        let mut keys = vec![original.clone()];
        let mut changed = original.clone();
        changed.media_type = "series".into();
        keys.push(changed);
        let mut changed = original.clone();
        changed.owner = "bob".into();
        keys.push(changed);
        let mut changed = original.clone();
        changed.epoch += 1;
        keys.push(changed);
        let mut changed = original.clone();
        changed.source = "other".into();
        keys.push(changed);
        let mut changed = original.clone();
        changed.recipe = "other".into();
        keys.push(changed);
        let mut changed = original;
        changed.authority.0 += 1;
        keys.push(changed);
        for (index, key) in keys.into_iter().enumerate() {
            assert_eq!(
                queue.submit(key, index, Priority::Background),
                Admission::Queued
            );
        }
        assert_eq!(queue.snapshot().background, 7);
    }

    #[test]
    fn poster_queue_prunes_generation_and_cancels_active_without_releasing_lane() {
        let queue = queue(1);
        queue.submit(key(1), 1, Priority::Background);
        queue.submit(key(2), 2, Priority::Background);
        let work = queue.take().unwrap();
        queue.generation.store(2, Ordering::Release);
        queue.invalidate();
        assert!(!work.completion.is_live());
        assert_eq!(queue.snapshot().background, 0);
        assert_eq!(queue.snapshot().active, 1);
        assert_eq!(
            queue.submit(key(3), 3, Priority::Visible),
            Admission::Obsolete
        );
        drop(work);
        assert_eq!(queue.snapshot().active, 0);
    }

    #[test]
    fn poster_queue_rejects_stale_revision() {
        let queue = queue(1);
        let mut stale = key(1);
        stale.revision = 0;
        assert_eq!(
            queue.submit(stale, 1, Priority::Visible),
            Admission::Obsolete
        );
        assert_eq!(queue.snapshot().visible, 0);
    }

    #[test]
    fn poster_queue_visible_deferral_retries_after_completion_without_scroll() {
        let queue = queue(1);
        let retries = Arc::new(AtomicUsize::new(0));
        let counter = retries.clone();
        queue.set_retry(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        for index in 0..VISIBLE_CAPACITY {
            queue.submit(key(index), index, Priority::Visible);
        }
        assert_eq!(
            queue.submit(key(999), 999, Priority::Visible),
            Admission::DeferredCapacity
        );
        let work = queue.take().unwrap();
        drop(work);
        assert_eq!(retries.load(Ordering::SeqCst), 1);
        queue.retry_started();
        assert_eq!(
            queue.submit(key(999), 999, Priority::Visible),
            Admission::Queued
        );
    }

    #[test]
    fn poster_queue_raii_releases_after_panic_or_failed_dispatch() {
        let queue = queue(1);
        queue.submit(key(1), 1, Priority::Visible);
        let work = queue.take().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _delivery = work.completion;
            panic!("injected fetch panic");
        }));
        assert!(result.is_err());
        assert_eq!(queue.snapshot().active, 0);
        assert_eq!(
            queue.submit(key(1), 1, Priority::Visible),
            Admission::Queued
        );
        let work = queue.take().unwrap();
        // A failed event-loop dispatch drops its closure and delivery lease.
        drop(move || drop(work));
        assert_eq!(queue.snapshot().active, 0);
    }

    #[test]
    fn poster_queue_wakes_for_visible_intake_and_close() {
        let queue = queue(1);
        let worker_queue = queue.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sent.send(worker_queue.take().map(|work| work.job)).unwrap();
            sent.send(worker_queue.take().map(|work| work.job)).unwrap();
        });
        assert_eq!(
            queue.submit(key(1), 1, Priority::Visible),
            Admission::Queued
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(1)
        );
        queue.close();
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), None);
        worker.join().unwrap();
        assert_eq!(
            queue.submit(key(2), 2, Priority::Visible),
            Admission::Closed
        );
    }

    #[test]
    fn poster_queue_full_visible_lane_keeps_background_promotion_retryable() {
        let queue = queue(1);
        queue.submit(key(999), 999, Priority::Background);
        for index in 0..VISIBLE_CAPACITY {
            queue.submit(key(index), index, Priority::Visible);
        }
        assert_eq!(
            queue.submit(key(999), 999, Priority::Visible),
            Admission::DeferredCapacity
        );
        assert_eq!(queue.snapshot().background, 1);
        drop(queue.take().unwrap());
        assert_eq!(
            queue.submit(key(999), 999, Priority::Visible),
            Admission::Promoted
        );
        assert_eq!(queue.snapshot().background, 0);
        assert_eq!(queue.snapshot().visible, VISIBLE_CAPACITY);
    }

    #[test]
    fn poster_queue_actual_two_four_eight_lane_matrix() {
        for workers in [2, 4, 8] {
            let queue = queue(workers);
            for index in 0..workers {
                queue.submit(key(index), index, Priority::Visible);
            }
            let deliveries: Vec<_> = (0..workers).map(|_| queue.take().unwrap()).collect();
            assert_eq!(queue.snapshot().active, workers);
            assert_eq!(queue.snapshot().peak_active, workers);
            queue.close();
            assert!(deliveries.iter().all(|work| !work.completion.is_live()));
            drop(deliveries);
            assert_eq!(queue.snapshot().active, 0);
        }
    }

    #[test]
    fn poster_queue_worker_configuration_stays_bounded() {
        assert_eq!(worker_count(Some("2"), Some(32)), 2);
        assert_eq!(worker_count(Some("4"), Some(32)), 4);
        assert_eq!(worker_count(Some("8"), Some(32)), 8);
        for invalid in ["0", "9", "invalid"] {
            assert_eq!(worker_count(Some(invalid), Some(32)), 8);
        }
        assert_eq!(worker_count(None, None), 4);
        assert_eq!(worker_count(None, Some(0)), 1);
    }
}

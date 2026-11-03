//! Bounded native workers. Rejections complete synchronously after releasing the
//! queue lock; accepted jobs complete on a worker, including cancellation and
//! shutdown. Callbacks must therefore tolerate synchronous terminal rejection.
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Busy,
    Stopped,
    Unavailable,
    Cancelled,
    Panicked,
}

/// Cancelling an accepted job never suppresses its terminal callback. It skips
/// queued work; already-running blocking transport observes its existing timeout.
#[derive(Clone)]
pub struct FetchHandle(Arc<AtomicBool>);
impl FetchHandle {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

type Task = Box<dyn FnOnce() + Send>;

pub(super) struct Job(Box<dyn FnOnce(Result<(), Failure>) + Send>);
impl Job {
    pub(super) fn new<T: Send + 'static>(
        live: impl Fn() -> bool + Send + 'static,
        operation: impl FnOnce() -> T + Send + 'static,
        completion: impl FnOnce(Result<T, Failure>) + Send + 'static,
    ) -> (Self, FetchHandle) {
        let handle = FetchHandle(Arc::new(AtomicBool::new(false)));
        let cancelled = handle.0.clone();
        let job = Self(Box::new(move |admission| {
            // Keep operation and callback guards separate. A callback panic is
            // one attempted delivery, never grounds to invoke that callback twice.
            let result = match admission {
                Err(error) => Err(error),
                Ok(()) => catch_unwind(AssertUnwindSafe(|| {
                    if cancelled.load(Ordering::Acquire) || !live() {
                        return Err(Failure::Cancelled);
                    }
                    let result = operation();
                    if cancelled.load(Ordering::Acquire) {
                        Err(Failure::Cancelled)
                    } else {
                        Ok(result)
                    }
                }))
                .unwrap_or(Err(Failure::Panicked)),
            };
            let _ = catch_unwind(AssertUnwindSafe(|| completion(result)));
        }));
        (job, handle)
    }

    fn finish(self, result: Result<(), Failure>) {
        // Also guard captured-value destructors after terminal delivery so an
        // unusual Drop panic cannot retire a worker and strand queued jobs.
        let _ = catch_unwind(AssertUnwindSafe(|| (self.0)(result)));
    }
}

struct State {
    queue: VecDeque<Job>,
    stopped: bool,
    workers: usize,
    active: usize,
    #[cfg(test)]
    peak_active: usize,
    #[cfg(test)]
    peak_queued: usize,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
    availability: AtomicU64,
}

pub(super) struct Executor {
    shared: Arc<Shared>,
}
impl Executor {
    pub(super) fn new(name: &str, workers: usize, capacity: usize) -> Self {
        Self::with_launcher(name, workers, capacity, |name, task| {
            std::thread::Builder::new()
                .name(name)
                .spawn(task)
                .map(|_| ())
        })
    }

    fn with_launcher(
        name: &str,
        workers: usize,
        capacity: usize,
        mut launch: impl FnMut(String, Task) -> std::io::Result<()>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                stopped: false,
                workers: 0,
                active: 0,
                #[cfg(test)]
                peak_active: 0,
                #[cfg(test)]
                peak_queued: 0,
            }),
            changed: Condvar::new(),
            capacity,
            availability: AtomicU64::new(1),
        });
        for index in 0..workers {
            shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .workers += 1;
            let worker = shared.clone();
            if launch(
                format!("{name}-{index}"),
                Box::new(move || worker_loop(worker)),
            )
            .is_err()
            {
                shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .workers -= 1;
            }
        }
        Self { shared }
    }

    /// Nonblocking queue admission. A rejected job runs only its terminal
    /// callback, synchronously, after the queue mutex has been released.
    pub(super) fn submit(&self, job: Job) -> bool {
        let rejected = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let failure = if state.stopped {
                Some(Failure::Stopped)
            } else if state.workers == 0 {
                Some(Failure::Unavailable)
            } else if state.queue.len() >= self.shared.capacity {
                Some(Failure::Busy)
            } else {
                None
            };
            if let Some(failure) = failure {
                Some((job, failure))
            } else {
                state.queue.push_back(job);
                #[cfg(test)]
                {
                    state.peak_queued = state.peak_queued.max(state.queue.len());
                }
                self.shared.changed.notify_one();
                None
            }
        };
        if let Some((job, failure)) = rejected {
            job.finish(Err(failure));
            false
        } else {
            true
        }
    }

    pub(super) fn availability_revision(&self) -> u64 {
        self.shared.availability.load(Ordering::Acquire)
    }

    fn shutdown(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        self.shared.availability.fetch_add(1, Ordering::Release);
        self.shared.changed.notify_all();
    }
}
impl Drop for Executor {
    fn drop(&mut self) {
        // Workers own Shared rather than Executor: dropping the pool closes
        // admission and workers drain queued terminal callbacks before exiting.
        self.shutdown();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let (job, stopped) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            loop {
                if let Some(job) = state.queue.pop_front() {
                    shared.availability.fetch_add(1, Ordering::Release);
                    state.active += 1;
                    #[cfg(test)]
                    {
                        state.peak_active = state.peak_active.max(state.active);
                    }
                    break (job, state.stopped);
                }
                if state.stopped {
                    state.workers -= 1;
                    shared.changed.notify_all();
                    return;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        job.finish(if stopped {
            Err(Failure::Stopped)
        } else {
            Ok(())
        });
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active -= 1;
        shared.availability.fetch_add(1, Ordering::Release);
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::AtomicUsize, mpsc},
        time::{Duration, Instant},
    };

    type Gate = Arc<(Mutex<bool>, Condvar)>;
    fn gate() -> Gate {
        Arc::new((Mutex::new(false), Condvar::new()))
    }
    fn wait(gate: &Gate) {
        let mut open = gate.0.lock().unwrap();
        while !*open {
            open = gate.1.wait(open).unwrap();
        }
    }
    fn release(gate: &Gate) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }
    fn close(pool: &Executor) {
        pool.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = pool.shared.state.lock().unwrap();
        while state.workers != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "executor did not finish accepted callbacks"
            );
            state = pool
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap()
                .0;
        }
        assert!(state.queue.is_empty());
        assert_eq!(state.active, 0);
    }

    #[test]
    fn availability_revision_precedes_busy_and_changes_on_capacity_release_and_close() {
        let pool = Executor::new("availability", 1, 1);
        let blocked = gate();
        let worker_gate = blocked.clone();
        let (started, observed) = mpsc::channel();
        let (first, _) = Job::new(
            || true,
            move || {
                started.send(()).unwrap();
                wait(&worker_gate);
            },
            |_| {},
        );
        assert!(pool.submit(first));
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (second, _) = Job::new(|| true, || (), |_| {});
        assert!(pool.submit(second));
        let before_busy = pool.availability_revision();
        let (busy, observed_busy) = mpsc::channel();
        let (third, _) = Job::new(
            || true,
            || panic!("busy work must not start"),
            move |result| {
                busy.send(result.err()).unwrap();
            },
        );
        assert!(!pool.submit(third));
        assert_eq!(observed_busy.recv().unwrap(), Some(Failure::Busy));
        release(&blocked);
        let deadline = Instant::now() + Duration::from_secs(2);
        while pool.availability_revision() == before_busy {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let before_close = pool.availability_revision();
        close(&pool);
        assert_ne!(pool.availability_revision(), before_close);
    }

    #[test]
    fn bounded_workers_queue_and_each_rejection_complete_once() {
        let pool = Executor::new("capacity", 2, 3);
        let blocked = gate();
        let (started_tx, started_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let caller = std::thread::current().id();
        for id in 0..2 {
            let blocked = blocked.clone();
            let started = started_tx.clone();
            let tx = tx.clone();
            let (job, _) = Job::new(
                || true,
                move || {
                    started.send(()).unwrap();
                    wait(&blocked);
                    id
                },
                move |result| {
                    tx.send((id, result, std::thread::current().id())).unwrap();
                },
            );
            assert!(pool.submit(job));
        }
        for _ in 0..2 {
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        for id in 2..12 {
            let tx = tx.clone();
            let (job, _) = Job::new(
                || true,
                move || id,
                move |result| {
                    tx.send((id, result, std::thread::current().id())).unwrap();
                },
            );
            assert_eq!(pool.submit(job), id < 5);
        }
        {
            let state = pool.shared.state.lock().unwrap();
            assert_eq!((state.active, state.queue.len()), (2, 3));
            assert_eq!((state.peak_active, state.peak_queued), (2, 3));
        }
        release(&blocked);
        drop(tx);
        let mut seen = vec![0; 12];
        for _ in 0..12 {
            let (id, result, thread) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            seen[id] += 1;
            if id < 5 {
                assert_eq!(result, Ok(id));
                assert_ne!(thread, caller);
            } else {
                assert_eq!(result, Err(Failure::Busy));
                assert_eq!(thread, caller);
            }
        }
        assert!(seen.iter().all(|count| *count == 1));
        close(&pool);
    }

    #[test]
    fn partial_and_total_spawn_failure_degrade_without_losing_completion() {
        for successful_workers in [0, 1] {
            let mut launches = 0;
            let pool = Executor::with_launcher("spawn", 3, 2, |name, task| {
                launches += 1;
                if launches <= successful_workers {
                    std::thread::Builder::new()
                        .name(name)
                        .spawn(task)
                        .map(|_| ())
                } else {
                    Err(std::io::Error::other("injected spawn failure"))
                }
            });
            let (tx, rx) = mpsc::channel();
            let caller = std::thread::current().id();
            let (job, _) = Job::new(
                || true,
                || 7,
                move |result| {
                    tx.send((result, std::thread::current().id())).unwrap();
                },
            );
            assert_eq!(pool.submit(job), successful_workers != 0);
            let (result, thread) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            if successful_workers == 0 {
                assert_eq!(result, Err(Failure::Unavailable));
                assert_eq!(thread, caller);
            } else {
                assert_eq!(result, Ok(7));
                assert_ne!(thread, caller);
            }
            close(&pool);
        }
    }

    #[test]
    fn cancelled_stale_panicking_operations_and_callbacks_release_workers() {
        let pool = Executor::new("cancel", 1, 8);
        let blocked = gate();
        let held = blocked.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (job, _) = Job::new(
            || true,
            move || {
                started_tx.send(()).unwrap();
                wait(&held);
            },
            |_| {},
        );
        pool.submit(job);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let checked = live.clone();
        let started = Arc::new(AtomicUsize::new(0));
        let count = started.clone();
        let (tx, rx) = mpsc::channel();
        let out = tx.clone();
        let (job, _) = Job::new(
            move || checked.load(Ordering::Acquire),
            move || {
                count.fetch_add(1, Ordering::Relaxed);
            },
            move |result| {
                out.send(result).unwrap();
            },
        );
        pool.submit(job);
        live.store(false, Ordering::Release);
        let count = started.clone();
        let out = tx.clone();
        let (job, handle) = Job::new(
            || true,
            move || {
                count.fetch_add(1, Ordering::Relaxed);
            },
            move |result| {
                out.send(result).unwrap();
            },
        );
        pool.submit(job);
        handle.cancel();
        let out = tx.clone();
        let (job, _) = Job::new(
            || true,
            || panic!("injected operation panic"),
            move |result: Result<(), Failure>| {
                out.send(result).unwrap();
            },
        );
        pool.submit(job);
        let (job, _) = Job::new(
            || panic!("injected liveness panic"),
            || (),
            move |result: Result<(), Failure>| {
                tx.send(result).unwrap();
            },
        );
        pool.submit(job);
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let (job, _) = Job::new(
            || true,
            || (),
            move |_| {
                count.fetch_add(1, Ordering::Relaxed);
                panic!("injected callback panic");
            },
        );
        pool.submit(job);
        let (end_tx, end_rx) = mpsc::channel();
        let (job, _) = Job::new(
            || true,
            || (),
            move |_| {
                end_tx.send(()).unwrap();
            },
        );
        pool.submit(job);
        release(&blocked);
        let results: Vec<_> = (0..4)
            .map(|_| rx.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();
        assert_eq!(
            results,
            vec![
                Err(Failure::Cancelled),
                Err(Failure::Cancelled),
                Err(Failure::Panicked),
                Err(Failure::Panicked)
            ]
        );
        end_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(started.load(Ordering::Relaxed), 0);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        close(&pool);
    }

    #[test]
    fn shutdown_delivers_queued_callbacks_on_worker_and_rejects_new_calls() {
        let pool = Executor::new("shutdown", 1, 4);
        let blocked = gate();
        let held = blocked.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let out = tx.clone();
        let (job, _) = Job::new(
            || true,
            move || {
                started_tx.send(()).unwrap();
                wait(&held);
            },
            move |result| {
                out.send((result, std::thread::current().id())).unwrap();
            },
        );
        pool.submit(job);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..4 {
            let out = tx.clone();
            let (job, _) = Job::new(
                || true,
                || panic!("shutdown work must not start"),
                move |result: Result<(), Failure>| {
                    out.send((result, std::thread::current().id())).unwrap();
                },
            );
            assert!(pool.submit(job));
        }
        pool.shutdown();
        let out = tx.clone();
        let (job, _) = Job::new(
            || true,
            || (),
            move |result| {
                out.send((result, std::thread::current().id())).unwrap();
            },
        );
        assert!(!pool.submit(job));
        let (result, thread) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(result, Err(Failure::Stopped));
        assert_eq!(thread, std::thread::current().id());
        release(&blocked);
        let results: Vec<_> = (0..5)
            .map(|_| rx.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|(result, _)| *result == Err(Failure::Stopped))
                .count(),
            4
        );
        assert!(
            results
                .iter()
                .all(|(_, thread)| *thread != std::thread::current().id())
        );
        close(&pool);
    }

    #[test]
    fn slow_callback_keeps_artwork_bounded_without_delaying_urgent_pool() {
        let artwork = Executor::new("slow-art", 1, 2);
        let urgent = Executor::new("urgent", 1, 1);
        let blocked = gate();
        let held = blocked.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (job, _) = Job::new(
            || true,
            || (),
            move |_| {
                started_tx.send(()).unwrap();
                wait(&held);
            },
        );
        artwork.submit(job);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let completions = Arc::new(AtomicUsize::new(0));
        for id in 0..3 {
            let count = completions.clone();
            let (job, _) = Job::new(
                || true,
                || (),
                move |_| {
                    count.fetch_add(1, Ordering::Relaxed);
                },
            );
            assert_eq!(artwork.submit(job), id < 2);
        }
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let (job, _) = Job::new(
            || true,
            || 1,
            move |result| {
                tx.send(result).unwrap();
            },
        );
        assert!(urgent.submit(job));
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(1));
        assert_eq!(artwork.shared.state.lock().unwrap().active, 1);
        eprintln!(
            "MED07 urgent_completion_us_with_blocked_artwork_callback={}",
            started.elapsed().as_micros()
        );
        release(&blocked);
        close(&artwork);
        close(&urgent);
        assert_eq!(completions.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn mixed_slow_http_fixture_delivers_every_attempt() {
        use std::io::{Read, Write};
        for requests in [10, 100, 1000] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let gets = Arc::new(AtomicUsize::new(0));
            let counted = gets.clone();
            let server = std::thread::spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = [0; 1024];
                            socket.read(&mut request).unwrap();
                            counted.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(1));
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 256\r\nConnection: close\r\n\r\n").unwrap();
                            socket.write_all(&[71; 256]).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::yield_now()
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                }
            });
            let pools = [
                Executor::new("http-net", 4, 64),
                Executor::new("http-urgent", 2, 16),
                Executor::new("http-art", 2, 32),
            ];
            let (tx, rx) = mpsc::channel();
            let mut accepted = 0;
            let clock = Instant::now();
            for id in 0..requests {
                let out = tx.clone();
                let (job, _) = Job::new(
                    || true,
                    move || {
                        let mut socket = std::net::TcpStream::connect(address).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        socket
                            .write_all(b"GET /fixture HTTP/1.1\r\nHost: localhost\r\n\r\n")
                            .unwrap();
                        let mut response = Vec::new();
                        socket.read_to_end(&mut response).unwrap();
                        let start = response
                            .windows(4)
                            .position(|bytes| bytes == b"\r\n\r\n")
                            .unwrap()
                            + 4;
                        assert_eq!(&response[start..], &[71; 256]);
                        // Artwork's decoder fixture remains on this same worker.
                        if id % 3 == 2 {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    },
                    move |result| {
                        out.send((id, result, clock.elapsed().as_micros())).unwrap();
                    },
                );
                accepted += usize::from(pools[id % 3].submit(job));
            }
            let results: Vec<_> = (0..requests)
                .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
                .collect();
            assert_eq!(
                results
                    .iter()
                    .filter(|(_, result, _)| result.is_ok())
                    .count(),
                accepted
            );
            let mut ids: Vec<_> = results.iter().map(|(id, _, _)| *id).collect();
            ids.sort_unstable();
            assert_eq!(ids, (0..requests).collect::<Vec<_>>());
            for pool in &pools {
                close(pool);
            }
            stop.store(true, Ordering::Release);
            server.join().unwrap();
            assert_eq!(gets.load(Ordering::Relaxed), accepted);
            let first = results
                .iter()
                .filter(|(_, result, _)| result.is_ok())
                .map(|(_, _, micros)| *micros)
                .min()
                .unwrap();
            eprintln!(
                "MED07 slow-local-HTTP fixture requests={requests} accepted_GETs={accepted} rejected={} first_success_us={first} elapsed_us={} payload=256x0x47",
                requests - accepted,
                clock.elapsed().as_micros()
            );
        }
    }

    #[test]
    fn mixed_queue_workloads_10_100_1000_record_bounds_and_completions() {
        for requests in [10, 100, 1000] {
            let pools = [
                Executor::new("fixture-net", 4, 64),
                Executor::new("fixture-urgent", 2, 16),
                Executor::new("fixture-art", 2, 32),
            ];
            let live_bytes = Arc::new(AtomicUsize::new(0));
            let peak_bytes = Arc::new(AtomicUsize::new(0));
            struct Payload {
                bytes: Vec<u8>,
                live: Arc<AtomicUsize>,
            }
            impl Drop for Payload {
                fn drop(&mut self) {
                    self.live.fetch_sub(self.bytes.len(), Ordering::Relaxed);
                }
            }
            let (tx, rx) = mpsc::channel();
            let mut accepted = 0;
            let started = Instant::now();
            for id in 0..requests {
                let count = live_bytes.fetch_add(1024, Ordering::Relaxed) + 1024;
                peak_bytes.fetch_max(count, Ordering::Relaxed);
                let payload = Payload {
                    bytes: vec![7; 1024],
                    live: live_bytes.clone(),
                };
                let tx = tx.clone();
                let queued = Instant::now();
                let (job, _) = Job::new(
                    || true,
                    move || {
                        let queue_wait = queued.elapsed().as_micros();
                        std::thread::sleep(Duration::from_millis(1));
                        assert_eq!(
                            payload
                                .bytes
                                .iter()
                                .map(|byte| *byte as usize)
                                .sum::<usize>(),
                            7168
                        );
                        drop(payload);
                        queue_wait
                    },
                    move |result| {
                        tx.send((id, result, started.elapsed().as_micros()))
                            .unwrap();
                    },
                );
                accepted += usize::from(pools[id % 3].submit(job));
            }
            drop(tx);
            let results: Vec<_> = (0..requests)
                .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
                .collect();
            assert_eq!(
                results
                    .iter()
                    .filter(|(_, result, _)| result.is_ok())
                    .count(),
                accepted
            );
            let mut ids: Vec<_> = results.iter().map(|(id, _, _)| *id).collect();
            ids.sort_unstable();
            assert_eq!(ids, (0..requests).collect::<Vec<_>>());
            let stats: Vec<_> = pools
                .iter()
                .map(|pool| {
                    close(pool);
                    let state = pool.shared.state.lock().unwrap();
                    (state.peak_active, state.peak_queued)
                })
                .collect();
            assert!(stats[0].0 <= 4 && stats[0].1 <= 64);
            assert!(stats[1].0 <= 2 && stats[1].1 <= 16);
            assert!(stats[2].0 <= 2 && stats[2].1 <= 32);
            assert_eq!(live_bytes.load(Ordering::Relaxed), 0);
            assert!(peak_bytes.load(Ordering::Relaxed) <= 121 * 1024);
            let mut waits: Vec<_> = results
                .iter()
                .filter_map(|(_, result, _)| result.as_ref().ok().copied())
                .collect();
            waits.sort_unstable();
            let first = results
                .iter()
                .filter(|(_, result, _)| result.is_ok())
                .map(|(_, _, elapsed)| *elapsed)
                .min()
                .unwrap();
            let rss = std::fs::read_to_string("/proc/self/status")
                .ok()
                .map(|status| {
                    status
                        .lines()
                        .filter(|line| {
                            line.starts_with("VmRSS:")
                                || line.starts_with("VmHWM:")
                                || line.starts_with("Threads:")
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            eprintln!(
                "MED07 fixture=1KiB-captured/1ms-work requests={requests} accepted={accepted} rejected={} active_queue_peaks={stats:?} peak_fixture_bytes={} first_success_us={first} queue_wait_us={waits:?} rss={rss:?}",
                requests - accepted,
                peak_bytes.load(Ordering::Relaxed)
            );
        }
    }
}

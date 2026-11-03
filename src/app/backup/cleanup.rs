//! Four admitted immutable payloads share one process-wide destruction worker.
//! A keeper owns the final Arc until its preallocated retirement job is queued.
use std::any::Any;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

const CAPACITY: usize = 4;
type Notify = fn();
static NOTIFY: OnceLock<Notify> = OnceLock::new();
static WORKER: OnceLock<Result<Arc<Inner>, String>> = OnceLock::new();

pub(super) fn set_notify(notify: Notify) {
    let _ = NOTIFY.set(notify);
}
fn notify() {
    if let Some(notify) = NOTIFY.get() {
        notify();
    }
}
struct Inner {
    used: AtomicUsize,
    failed: AtomicBool,
    tx: mpsc::SyncSender<Job>,
}
struct Job {
    // Only execute() may destroy these fields. An impossible channel failure
    // must fail admission closed rather than unwind a heavy payload on UI.
    payload: ManuallyDrop<Box<dyn Any + Send + Sync>>,
    permit: ManuallyDrop<Permit>,
}
impl Job {
    fn execute(mut self) {
        #[cfg(test)]
        let started = std::time::Instant::now();
        #[cfg(test)]
        let cpu_started = test_thread_cpu();
        let payload = unsafe { ManuallyDrop::take(&mut self.payload) };
        let permit = unsafe { ManuallyDrop::take(&mut self.permit) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)));
        if let Err(panic) = result {
            // A panic's own arbitrary payload may also have a panicking Drop.
            // Discarding the exception must not terminate the draining worker.
            std::mem::forget(panic);
        }
        #[cfg(test)]
        {
            TEST_DROP_WALL.fetch_add(started.elapsed().as_nanos() as u64, Ordering::AcqRel);
            TEST_DROP_CPU.fetch_add(
                test_thread_cpu().saturating_sub(cpu_started),
                Ordering::AcqRel,
            );
            TEST_COMPLETIONS.fetch_add(1, Ordering::AcqRel);
        }
        // Nested Shared handles can enqueue their jobs during payload Drop.
        // Their independent permits remain counted until those jobs finish.
        drop(permit);
    }
}
fn start_worker(
    spawn: impl FnOnce(mpsc::Receiver<Job>) -> Result<(), String>,
) -> Result<Arc<Inner>, String> {
    let (tx, rx) = mpsc::sync_channel::<Job>(CAPACITY);
    let inner = Arc::new(Inner {
        used: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        tx,
    });
    spawn(rx)?;
    Ok(inner)
}
fn worker() -> &'static Result<Arc<Inner>, String> {
    WORKER.get_or_init(|| {
        start_worker(|rx| {
            std::thread::Builder::new()
                .name("nova-backup-cleanup".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job.execute();
                    }
                })
                .map(|_| ())
                .map_err(|error| format!("Cannot start backup cleanup: {error}"))
        })
    })
}

/// One CAS reserves the entire pipeline, including Undo's two payloads. UI
/// never waits for a slot or partially reserves a pipeline that needs two.
pub(super) fn reserve(count: usize) -> Result<Option<Reservation>, String> {
    reserve_from(worker().as_ref().map_err(Clone::clone)?, count)
}
fn reserve_from(inner: &Arc<Inner>, count: usize) -> Result<Option<Reservation>, String> {
    if inner.failed.load(Ordering::Acquire) {
        return Err("Backup cleanup is unavailable".into());
    }
    assert!((1..=CAPACITY).contains(&count));
    let used = inner.used.load(Ordering::Acquire);
    if used + count > CAPACITY
        || inner
            .used
            .compare_exchange(used, used + count, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Ok(None);
    }
    Ok(Some(Reservation {
        inner: Arc::clone(inner),
        remaining: count,
    }))
}
pub(super) struct Reservation {
    inner: Arc<Inner>,
    remaining: usize,
}
impl Reservation {
    pub(super) fn take(&mut self) -> Permit {
        assert!(self.remaining > 0);
        self.remaining -= 1;
        Permit {
            inner: Arc::clone(&self.inner),
        }
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.remaining > 0 {
            self.inner.used.fetch_sub(self.remaining, Ordering::AcqRel);
            notify();
        }
    }
}
pub(super) struct Permit {
    inner: Arc<Inner>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.inner.used.fetch_sub(1, Ordering::AcqRel);
        notify();
    }
}
struct Keeper {
    job: ManuallyDrop<Job>,
}
impl Drop for Keeper {
    fn drop(&mut self) {
        let job = unsafe { ManuallyDrop::take(&mut self.job) };
        let inner = Arc::clone(&job.permit.inner);
        // Each queued/running/UI-held job has one of the same four permits.
        // A channel with four slots cannot be full when this job is not in it.
        if inner.tx.try_send(job).is_err() {
            // Job's ManuallyDrop fields retain at most the four admitted
            // payloads on this terminal invariant failure. No UI destruction,
            // permit reuse or further admission can hide a broken worker.
            inner.failed.store(true, Ordering::Release);
            notify();
        }
    }
}

/// Immutable value plus a shared retirement ticket. Field order matters: each
/// handle releases its visible Arc before the keeper can enqueue the final Arc.
pub(super) struct Shared<T: Send + Sync + 'static> {
    value: Arc<T>,
    keeper: Arc<Keeper>,
}
impl<T: Send + Sync + 'static> Shared<T> {
    pub(super) fn new(value: T, permit: Permit) -> Self {
        let value = Arc::new(value);
        let keeper = Arc::new(Keeper {
            job: ManuallyDrop::new(Job {
                payload: ManuallyDrop::new(Box::new(Arc::clone(&value))),
                permit: ManuallyDrop::new(permit),
            }),
        });
        Self { value, keeper }
    }
    #[cfg(test)]
    pub(super) fn ptr_eq(left: &Self, right: &Self) -> bool {
        Arc::ptr_eq(&left.value, &right.value)
    }
    #[cfg(test)]
    pub(super) fn weak(&self) -> std::sync::Weak<T> {
        Arc::downgrade(&self.value)
    }
}
impl<T: Send + Sync + 'static> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            keeper: Arc::clone(&self.keeper),
        }
    }
}
impl<T: Send + Sync + 'static> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
#[cfg(test)]
static TEST_DROP_WALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static TEST_DROP_CPU: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static TEST_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux", target_pointer_width = "64"))]
fn test_thread_cpu() -> u64 {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
    }
    let mut time = Timespec { sec: 0, nsec: 0 };
    if unsafe { clock_gettime(3, &mut time) } != 0 {
        return 0;
    }
    time.sec as u64 * 1_000_000_000 + time.nsec as u64
}
#[cfg(all(test, not(all(target_os = "linux", target_pointer_width = "64"))))]
fn test_thread_cpu() -> u64 {
    0
}
#[cfg(test)]
pub(super) fn drop_stats_for_test() -> (u64, u64, usize) {
    (
        TEST_DROP_WALL.load(Ordering::Acquire),
        TEST_DROP_CPU.load(Ordering::Acquire),
        TEST_COMPLETIONS.load(Ordering::Acquire),
    )
}
#[cfg(test)]
pub(super) fn admitted_for_test() -> usize {
    worker().as_ref().unwrap().used.load(Ordering::Acquire)
}
#[cfg(test)]
pub(super) fn share_for_test<T: Send + Sync + 'static>(value: T) -> Shared<T> {
    // Fixture construction is not a UI admission path. Waiting here keeps
    // parallel unit fixtures from spuriously exhausting the process pool.
    Shared::new(value, permit_for_test())
}
#[cfg(test)]
pub(super) fn permit_for_test() -> Permit {
    let mut reservation = loop {
        if let Some(reservation) = reserve(1).unwrap() {
            break reservation;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    reservation.take()
}
#[cfg(test)]
pub(super) fn wait_for_drop<T>(weak: &std::sync::Weak<T>) {
    let start = std::time::Instant::now();
    while weak.strong_count() > 0 {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn isolated(name: &str, test: impl FnOnce()) {
        const CASE: &str = "NOVA_BACKUP_CLEANUP_CASE";
        if std::env::var(CASE).as_deref() == Ok(name) {
            test();
            return;
        }
        let module = module_path!().split_once("::").unwrap().1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CASE, name)
            .args(["--exact", &format!("{module}::{name}"), "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn unused() -> usize {
        worker().as_ref().unwrap().used.load(Ordering::Acquire)
    }
    fn drained() {
        let start = Instant::now();
        while unused() != 0 {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    struct Probe {
        dropped: mpsc::Sender<std::thread::ThreadId>,
        block: Option<(mpsc::Sender<()>, std::sync::Mutex<mpsc::Receiver<()>>)>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some((started, release)) = self.block.take() {
                started.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            self.dropped.send(std::thread::current().id()).unwrap();
        }
    }

    #[test]
    fn bounded_cleanup_counts_ui_queued_running_and_atomic_undo_reservations() {
        isolated(
            "bounded_cleanup_counts_ui_queued_running_and_atomic_undo_reservations",
            || {
                let ui = std::thread::current().id();
                let (dropped, drops) = mpsc::channel();
                let (started, running) = mpsc::channel();
                let (release, blocked) = mpsc::channel();
                let mut reservation = reserve(4).unwrap().unwrap();
                let first = Shared::new(
                    Probe {
                        dropped: dropped.clone(),
                        block: Some((started, std::sync::Mutex::new(blocked))),
                    },
                    reservation.take(),
                );
                let mut held = Vec::new();
                for _ in 0..3 {
                    held.push(Shared::new(
                        Probe {
                            dropped: dropped.clone(),
                            block: None,
                        },
                        reservation.take(),
                    ));
                }
                let clone = first.clone();
                assert!(Shared::ptr_eq(&first, &clone));
                drop(first);
                assert!(
                    running.try_recv().is_err(),
                    "a clone keeps the same retirement ticket alive"
                );
                drop(clone);
                running.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!(unused(), 4);
                assert!(reserve(1).unwrap().is_none());
                assert!(reserve(2).unwrap().is_none());
                assert_eq!(
                    unused(),
                    4,
                    "failed Undo admission must not reserve one partial slot"
                );
                drop(held);
                assert_eq!(
                    unused(),
                    4,
                    "queued jobs retain permits until destruction really finishes"
                );
                assert!(
                    drops.try_recv().is_err(),
                    "no heavy destructor ran on the caller while cleanup was blocked"
                );
                release.send(()).unwrap();
                let worker = drops.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_ne!(worker, ui);
                for _ in 0..3 {
                    assert_eq!(drops.recv_timeout(Duration::from_secs(10)).unwrap(), worker);
                }
                drained();
                let reservation = reserve(4).unwrap().unwrap();
                assert_eq!(unused(), 4);
                drop(reservation); // cancelled descriptor/dispatch returns unused admission.
                assert_eq!(unused(), 0);
            },
        );
    }

    #[test]
    fn nested_preview_archive_and_rejected_results_destroy_on_one_worker() {
        isolated(
            "nested_preview_archive_and_rejected_results_destroy_on_one_worker",
            || {
                struct PreviewProbe {
                    archive: Shared<Probe>,
                    dropped: mpsc::Sender<std::thread::ThreadId>,
                }
                impl Drop for PreviewProbe {
                    fn drop(&mut self) {
                        let _ = &self.archive;
                        self.dropped.send(std::thread::current().id()).unwrap();
                    }
                }
                let ui = std::thread::current().id();
                let (dropped, drops) = mpsc::channel();
                let mut reservation = reserve(2).unwrap().unwrap();
                let archive = Shared::new(
                    Probe {
                        dropped: dropped.clone(),
                        block: None,
                    },
                    reservation.take(),
                );
                let preview = Shared::new(
                    PreviewProbe {
                        archive: archive.clone(),
                        dropped,
                    },
                    reservation.take(),
                );
                drop(archive);
                // This is also the stale event-loop completion rejection path:
                // dropping the last counted result must never drop its fields here.
                drop(preview);
                let worker = drops.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_ne!(worker, ui);
                assert_eq!(drops.recv_timeout(Duration::from_secs(10)).unwrap(), worker);
                drained();
                let (wall, cpu, completions) = drop_stats_for_test();
                assert!(wall > 0);
                #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
                assert!(cpu > 0);
                #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
                let _ = cpu;
                assert_eq!(completions, 2);
            },
        );
    }

    #[test]
    fn cleanup_catches_drop_panic_and_start_failure_precedes_admission() {
        isolated(
            "cleanup_catches_drop_panic_and_start_failure_precedes_admission",
            || {
                assert!(start_worker(|_| Err("injected worker startup failure".into())).is_err());
                struct Panic;
                impl Drop for Panic {
                    fn drop(&mut self) {
                        panic!("injected payload drop panic");
                    }
                }
                let mut reservation = reserve(2).unwrap().unwrap();
                let (dropped, drops) = mpsc::channel();
                let panic = Shared::new(Panic, reservation.take());
                let next = Shared::new(
                    Probe {
                        dropped,
                        block: None,
                    },
                    reservation.take(),
                );
                drop(panic);
                drop(next);
                assert_ne!(
                    drops.recv_timeout(Duration::from_secs(10)).unwrap(),
                    std::thread::current().id()
                );
                drained();
            },
        );
    }
}

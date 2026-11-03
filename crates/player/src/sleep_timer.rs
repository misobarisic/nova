//! Suspend-inclusive monotonic deadlines, enforced independently of Slint ticks.
use super::*;

/// Unlike `Instant` on Linux this clock includes system suspend. None of the
/// supported platform clocks is adjusted by setting the wall clock or timezone.
pub fn elapsed_realtime_ms() -> u64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        #[repr(C)]
        struct Timespec {
            seconds: std::os::raw::c_long,
            nanos: std::os::raw::c_long,
        }
        unsafe extern "C" {
            fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
        }
        let mut time = Timespec {
            seconds: 0,
            nanos: 0,
        };
        // CLOCK_BOOTTIME is 7 on Linux and Android (all supported ABIs).
        if unsafe { clock_gettime(7, &mut time) } == 0 {
            return (time.seconds as u64).saturating_mul(1000) + time.nanos as u64 / 1_000_000;
        }
        // Supported kernels provide CLOCK_BOOTTIME. Fail closed if unavailable.
        u64::MAX
    }
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetTickCount64() -> u64;
        }
        unsafe { GetTickCount64() }
    }
    #[cfg(target_os = "macos")]
    {
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        unsafe extern "C" {
            fn mach_continuous_time() -> u64;
            fn mach_timebase_info(info: *mut Timebase) -> i32;
        }
        let mut base = Timebase { numer: 0, denom: 0 };
        if unsafe { mach_timebase_info(&mut base) } != 0 || base.denom == 0 {
            return u64::MAX;
        }
        ((unsafe { mach_continuous_time() } as u128 * base.numer as u128 / base.denom as u128)
            / 1_000_000)
            .min(u64::MAX as u128) as u64
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "windows",
        target_os = "macos"
    )))]
    {
        u64::MAX
    }
}

#[derive(Default)]
pub(super) struct Deadline {
    pub at: Option<u64>,
    pub expired: bool,
    pub progress: Option<(f64, f64)>,
}

impl State {
    pub(super) fn enforce_sleep_deadline(&self) -> bool {
        // Serialize arming/cancellation with the stop. A cancelled old deadline
        // must never stop a freshly armed timer or a replacement session.
        let mut timer = self.sleep_deadline.lock().unwrap();
        if timer.expired {
            return true;
        }
        if timer.at.is_none_or(|at| elapsed_realtime_ms() < at) {
            return false;
        }
        timer.expired = true;
        timer.at = None;
        let observed_current = self.started.load(Ordering::SeqCst)
            && !self.source_waiting.load(Ordering::SeqCst)
            && self.pending.lock().unwrap().is_none();
        *self.pending.lock().unwrap() = None;
        if let Some(mpv) = self.mpv.lock().unwrap().as_ref() {
            if observed_current {
                timer.progress = mpv
                    .get_property::<f64>("time-pos")
                    .ok()
                    .zip(mpv.get_property::<f64>("duration").ok());
            }
            let _ = mpv.set_property("pause", true);
            let _ = mpv.command("stop", &[]);
        }
        true
    }
}

const WATCHDOG_INTERVAL: Duration = Duration::from_millis(200);

fn watchdog_loop<T>(
    weak: std::sync::Weak<T>,
    mut check: impl FnMut(&T) -> bool,
    mut wait: impl FnMut(bool),
) {
    loop {
        let armed = {
            let Some(state) = weak.upgrade() else { break };
            check(&state)
        }; // Never retain State while parked: final State drop must wake us.
        // Every return, including a spurious wake or a preexisting unpark token,
        // returns through the state check before selecting another wait.
        wait(armed);
    }
}

fn wait_for_change(armed: bool) {
    if armed {
        std::thread::park_timeout(WATCHDOG_INTERVAL);
    } else {
        std::thread::park();
    }
}

pub(super) fn start_watchdog(state: &Arc<State>) {
    let weak = Arc::downgrade(state);
    let worker = std::thread::Builder::new()
        .name("nova-sleep-timer".into())
        .spawn(move || {
            watchdog_loop(
                weak,
                |state| {
                    state.enforce_sleep_deadline();
                    let timer = state.sleep_deadline.lock().unwrap();
                    timer.at.is_some() && !timer.expired
                },
                wait_for_change,
            );
        });
    match worker {
        Ok(worker) => {
            // Publish before setup returns, so every later arm/cancel can wake
            // an idle worker. Dropping JoinHandle detaches without retaining State.
            let _ = state.sleep_watchdog.set(worker.thread().clone());
        }
        Err(error) => {
            // The existing UI/native enforcement paths remain available even
            // when the OS cannot allocate this independent watchdog thread.
            eprintln!("nova-player: failed to start sleep timer watchdog: {error}");
        }
    }
}

impl Player {
    pub fn set_sleep_deadline(&self, at: Option<u64>) {
        *self.state.sleep_deadline.lock().unwrap() = Deadline {
            at,
            ..Default::default()
        };
        if let Some(worker) = self.state.sleep_watchdog.get() {
            worker.unpark();
        }
    }
    pub fn sleep_deadline_expired(&self) -> bool {
        self.state.enforce_sleep_deadline()
    }
    pub fn sleep_timer_progress(&self) -> Option<(f64, f64)> {
        self.state.sleep_deadline.lock().unwrap().progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Condvar, OnceLock, mpsc};

    #[test]
    fn spurious_returns_recheck_without_scheduling_disarmed_timeouts_or_retaining_state() {
        let owner = RefCell::new(Some(Arc::new(())));
        let weak = Arc::downgrade(owner.borrow().as_ref().unwrap());
        let mut armed = [false, false, true, false].into_iter();
        let checks = Cell::new(0);
        let scheduled_timed_waits = Cell::new(0);
        let scheduled_indefinite_waits = Cell::new(0);
        let observed_wait_returns = Cell::new(0);
        watchdog_loop(
            weak.clone(),
            |_| {
                checks.set(checks.get() + 1);
                armed.next().unwrap()
            },
            |armed| {
                assert_eq!(
                    weak.strong_count(),
                    1,
                    "only the external owner may remain while waiting"
                );
                if armed {
                    scheduled_timed_waits.set(scheduled_timed_waits.get() + 1);
                } else {
                    scheduled_indefinite_waits.set(scheduled_indefinite_waits.get() + 1);
                }
                // Inject returns without a state change, including two successive
                // disarmed iterations: these model permitted spurious wakes.
                observed_wait_returns.set(observed_wait_returns.get() + 1);
                if observed_wait_returns.get() == 4 {
                    drop(owner.borrow_mut().take());
                }
            },
        );
        assert_eq!(checks.get(), 4);
        assert_eq!(scheduled_timed_waits.get(), 1);
        assert_eq!(scheduled_indefinite_waits.get(), 3);
        assert_eq!(observed_wait_returns.get(), 4);
        assert!(weak.upgrade().is_none());
        assert_eq!(WATCHDOG_INTERVAL, Duration::from_millis(200));
    }

    struct WatchdogState {
        armed: AtomicBool,
        worker: OnceLock<std::thread::Thread>,
    }
    impl Drop for WatchdogState {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.get() {
                worker.unpark();
            }
        }
    }

    #[test]
    fn arm_cancel_and_final_drop_tokens_before_park_are_not_lost() {
        let state = Arc::new(WatchdogState {
            armed: AtomicBool::new(false),
            worker: OnceLock::new(),
        });
        let weak = Arc::downgrade(&state);
        let check_retention = weak.clone();
        let gate = Arc::new((Mutex::new(0), Condvar::new()));
        let worker_gate = gate.clone();
        let (reached, waiting) = mpsc::channel();
        let (exited, exit) = mpsc::channel();
        let scheduled_timed_waits = Arc::new(AtomicUsize::new(0));
        let scheduled_indefinite_waits = Arc::new(AtomicUsize::new(0));
        let observed_wait_returns = Arc::new(AtomicUsize::new(0));
        let timed = scheduled_timed_waits.clone();
        let indefinite = scheduled_indefinite_waits.clone();
        let returns = observed_wait_returns.clone();
        let worker = std::thread::spawn(move || {
            let mut step = 0;
            watchdog_loop(
                weak,
                |state| state.armed.load(Ordering::SeqCst),
                |armed| {
                    step += 1;
                    reached
                        .send((armed, check_retention.strong_count()))
                        .unwrap();
                    // A condvar holds the worker immediately before park without
                    // consuming its Thread::unpark token as channel waits may do.
                    let (permit, changed) = &*worker_gate;
                    let permit = changed
                        .wait_while(permit.lock().unwrap(), |permit| *permit < step)
                        .unwrap();
                    drop(permit);
                    if armed {
                        timed.fetch_add(1, Ordering::SeqCst);
                    } else {
                        indefinite.fetch_add(1, Ordering::SeqCst);
                    }
                    wait_for_change(armed);
                    returns.fetch_add(1, Ordering::SeqCst);
                },
            );
            exited.send(()).unwrap();
        });
        state.worker.set(worker.thread().clone()).unwrap();
        let allow = |step| {
            let (permit, changed) = &*gate;
            *permit.lock().unwrap() = step;
            changed.notify_one();
        };
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(2)).unwrap(),
            (false, 1)
        );
        state.armed.store(true, Ordering::SeqCst);
        state.worker.get().unwrap().unpark();
        allow(1);
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(2)).unwrap(),
            (true, 1)
        );
        state.armed.store(false, Ordering::SeqCst);
        state.worker.get().unwrap().unpark();
        allow(2);
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(2)).unwrap(),
            (false, 1)
        );
        drop(state); // Final owner destruction supplies the token for idle exit.
        allow(3);
        exit.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(scheduled_timed_waits.load(Ordering::SeqCst), 1);
        assert_eq!(scheduled_indefinite_waits.load(Ordering::SeqCst), 2);
        assert_eq!(observed_wait_returns.load(Ordering::SeqCst), 3);
        // Joining proves final drop woke the parked loop and it exited.
    }

    #[test]
    fn expired_native_deadline_removes_pending_load_and_cancellation_disarms_guard() {
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = Player::setup(&app);
        *player.state.pending.lock().unwrap() = Some("https://example.invalid/stale".into());
        player.set_sleep_deadline(Some(0));
        assert!(player.sleep_deadline_expired());
        assert!(player.state.pending.lock().unwrap().is_none());
        assert!(player.sleep_deadline_expired()); // Latch survives repeated observers.
        player.set_sleep_deadline(None);
        assert!(!player.sleep_deadline_expired());
        player.set_sleep_deadline(Some(elapsed_realtime_ms().saturating_add(60_000)));
        assert!(!player.sleep_deadline_expired());
    }
    #[test]
    fn platform_clock_is_available_and_monotonic() {
        let before = elapsed_realtime_ms();
        assert_ne!(before, u64::MAX);
        assert!(elapsed_realtime_ms() >= before);
    }
}

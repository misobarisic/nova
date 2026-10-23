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

pub(super) fn start_watchdog(state: &Arc<State>) {
    let weak = Arc::downgrade(state);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let Some(state) = weak.upgrade() else { break };
            state.enforce_sleep_deadline();
        }
    });
}

impl Player {
    pub fn set_sleep_deadline(&self, at: Option<u64>) {
        *self.state.sleep_deadline.lock().unwrap() = Deadline {
            at,
            ..Default::default()
        };
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

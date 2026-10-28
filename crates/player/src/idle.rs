//! Desktop idle inhibition while video plays (Linux-only).
//!
//! Holds either an `org.freedesktop.ScreenSaver` cookie or a logind `idle`
//! inhibitor fd while the player wants the screen kept awake, releasing on
//! pause/EOF/close. Best-effort throughout: with no session bus (or a
//! compositor implementing neither interface) playback simply doesn't inhibit
//! and the screen may dim.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

#[derive(Clone, Copy, Default)]
struct InhibitRequest {
    want: bool,
    revision: u64,
}

/// Only this tiny desired-state mailbox is shared with the UI. The worker owns
/// the bus connections, cookies and blocking calls, including release calls.
struct InhibitWorker {
    latest: Arc<Mutex<InhibitRequest>>,
    wakeup: mpsc::SyncSender<()>,
}

impl InhibitWorker {
    fn spawn(
        mut converge: impl FnMut(bool) + Send + 'static,
    ) -> (Self, std::thread::JoinHandle<()>) {
        let latest = Arc::new(Mutex::new(InhibitRequest::default()));
        let requested = latest.clone();
        let (wakeup, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut applied_revision = 0;
            while receiver.recv().is_ok() {
                let request = *requested.lock().unwrap_or_else(|e| e.into_inner());
                if request.revision != applied_revision {
                    applied_revision = request.revision;
                    converge(request.want);
                }
            }
        });
        (Self { latest, wakeup }, worker)
    }

    fn request(&self, want: bool) {
        {
            let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            if latest.want == want {
                return;
            }
            latest.want = want;
            latest.revision = latest.revision.wrapping_add(1);
        }
        // One wake is enough: the worker reads the newest desired state. Never
        // queue one bus call per click or wait for a slow service on the UI.
        let _ = self.wakeup.try_send(());
    }
}

/// What the player wants vs. what is held: the only transitions that touch
/// D-Bus. Pure so the contract is unit-testable without a bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Acquire,
    Release,
    Keep,
}

fn action_for(want: bool, held: bool, attempted: bool) -> Action {
    match (want, held) {
        (true, false) if !attempted => Action::Acquire,
        (false, true) => Action::Release,
        _ => Action::Keep,
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    fn inhibit(&self, application_name: &str, reason_for_inhibit: &str) -> zbus::Result<u32>;
    fn uninhibit(&self, cookie: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;
}

struct InhibitState {
    conn: Option<zbus::blocking::Connection>,
    cookie: Option<u32>,
    /// logind inhibitor: dropping the fd releases the block.
    login_fd: Option<OwnedFd>,
    attempted: bool,
}

impl InhibitState {
    const fn empty() -> Self {
        Self {
            conn: None,
            cookie: None,
            login_fd: None,
            attempted: false,
        }
    }

    fn held(&self) -> bool {
        self.cookie.is_some() || self.login_fd.is_some()
    }
}

/// Request screen inhibition towards `want` (playing video), without waiting
/// for D-Bus. One worker coalesces transitions and owns all native resources.
pub fn converge_idle_inhibit(want: bool) {
    static WORKER: OnceLock<InhibitWorker> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let mut state = InhibitState::empty();
            InhibitWorker::spawn(move |want| {
                // Revisions retain pause/resume boundaries even if both were
                // coalesced during a slow acquisition. A failed old interval
                // must not suppress the next interval's acquisition attempt.
                state.attempted = false;
                converge_on_worker(&mut state, want);
            })
            .0
        })
        .request(want);
}

/// Transition-only: steady states make no D-Bus calls. Failed acquisition
/// is retried only after pause/close; a try may contact both buses.
fn converge_on_worker(state: &mut InhibitState, want: bool) {
    if !want {
        state.attempted = false;
    }
    match action_for(want, state.held(), state.attempted) {
        Action::Keep => {}
        Action::Release => {
            if let (Some(conn), Some(cookie)) = (state.conn.clone(), state.cookie)
                && let Ok(proxy) = ScreenSaverProxyBlocking::new(&conn)
                && let Err(e) = proxy.uninhibit(cookie)
            {
                eprintln!("nova player: release screensaver inhibit: {e}");
            }
            state.cookie = None;
            state.login_fd = None;
        }
        Action::Acquire => {
            // One attempt per playing interval; unavailable services must not
            // block/log on every player tick. Pause/close permits another try.
            state.attempted = true;
            let session = state
                .conn
                .clone()
                .map(Ok)
                .unwrap_or_else(zbus::blocking::Connection::session);
            match session.and_then(|conn| {
                let cookie =
                    ScreenSaverProxyBlocking::new(&conn)?.inhibit("nova", "Video playback")?;
                Ok((conn, cookie))
            }) {
                Ok((conn, cookie)) => {
                    state.conn = Some(conn);
                    state.cookie = Some(cookie);
                    return;
                }
                Err(e) => {
                    eprintln!("nova player: screensaver inhibit unavailable ({e}); trying logind");
                }
            }
            // logind is on the system bus, including when no session bus is
            // available. The returned fd owns the inhibitor independently.
            match zbus::blocking::Connection::system().and_then(|conn| {
                LoginManagerProxyBlocking::new(&conn)?.inhibit(
                    "idle",
                    "nova",
                    "Video playback",
                    "block",
                )
            }) {
                Ok(fd) => state.login_fd = Some(fd.into()),
                Err(e) => {
                    eprintln!("nova player: logind idle inhibit unavailable: {e}");
                    state.conn = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_only() {
        assert_eq!(action_for(true, false, false), Action::Acquire);
        assert_eq!(action_for(true, false, true), Action::Keep);
        assert_eq!(action_for(false, true, true), Action::Release);
        assert_eq!(action_for(true, true, true), Action::Keep);
        assert_eq!(action_for(false, false, false), Action::Keep);
    }

    #[test]
    fn slow_inhibit_does_not_block_requests_and_bursts_converge_to_latest() {
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (observed, observations) = mpsc::channel();
        let (worker, join) = InhibitWorker::spawn(move |want| {
            observed.send(want).unwrap();
            if want {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
            }
        });
        worker.request(true);
        started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // The fake service remains blocked throughout the UI-side burst.
        // Exactly one pending wake survives and its desired state is false.
        for _ in 0..1_000 {
            worker.request(false);
            worker.request(true);
        }
        worker.request(false);
        release.send(()).unwrap();
        assert!(
            observations
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
        );
        assert!(
            !observations
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
        );
        drop(worker);
        join.join().unwrap();
        assert!(observations.try_recv().is_err());
    }

    #[test]
    fn coalesced_pause_resume_preserves_retry_and_steady_ticks_are_ignored() {
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (observed, observations) = mpsc::channel();
        let (worker, join) = InhibitWorker::spawn(move |want| {
            observed.send(want).unwrap();
            entered.send(()).unwrap();
            blocked.recv().unwrap();
        });
        worker.request(true);
        started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        worker.request(false);
        worker.request(true);
        release.send(()).unwrap();
        started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        for _ in 0..100 {
            worker.request(true);
        }
        release.send(()).unwrap();
        drop(worker);
        join.join().unwrap();
        assert_eq!(observations.into_iter().collect::<Vec<_>>(), [true, true]);
    }
}

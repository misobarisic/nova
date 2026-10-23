//! Desktop idle inhibition while video plays (Linux-only).
//!
//! Holds either an `org.freedesktop.ScreenSaver` cookie or a logind `idle`
//! inhibitor fd while the player wants the screen kept awake, releasing on
//! pause/EOF/close. Best-effort throughout: with no session bus (or a
//! compositor implementing neither interface) playback simply doesn't inhibit
//! and the screen may dim.

use std::os::fd::OwnedFd;
use std::sync::Mutex;

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

static STATE: Mutex<InhibitState> = Mutex::new(InhibitState::empty());

/// Converge screen inhibition towards `want` (playing video). Transition-only:
/// steady states make no D-Bus calls. Failed acquisition is retried only after
/// pause/close; the initial attempt may contact both buses.
pub fn converge_idle_inhibit(want: bool) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
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
}

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

fn action_for(want: bool, held: bool) -> Action {
    match (want, held) {
        (true, false) => Action::Acquire,
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
}

impl InhibitState {
    const fn empty() -> Self {
        Self {
            conn: None,
            cookie: None,
            login_fd: None,
        }
    }

    fn held(&self) -> bool {
        self.cookie.is_some() || self.login_fd.is_some()
    }
}

static STATE: Mutex<InhibitState> = Mutex::new(InhibitState::empty());

/// Converge screen inhibition towards `want` (playing video). Transition-only:
/// steady states make no D-Bus calls. Safe from the UI thread; a call blocks
/// for one round trip at most, and only on flips.
pub fn converge_idle_inhibit(want: bool) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match action_for(want, state.held()) {
        Action::Keep => {}
        Action::Release => {
            if let (Some(conn), Some(cookie)) = (state.conn.clone(), state.cookie) {
                if let Ok(proxy) = ScreenSaverProxyBlocking::new(&conn)
                    && let Err(e) = proxy.uninhibit(cookie)
                {
                    eprintln!("nova player: release screensaver inhibit: {e}");
                }
            }
            state.cookie = None;
            state.login_fd = None;
        }
        Action::Acquire => {
            let conn = match state.conn.clone() {
                Some(conn) => conn,
                None => match zbus::blocking::Connection::session() {
                    Ok(conn) => {
                        state.conn = Some(conn.clone());
                        conn
                    }
                    Err(e) => {
                        eprintln!("nova player: no session bus for idle inhibit: {e}");
                        return;
                    }
                },
            };
            // Preferred path: the desktop's screen saver inhibitor.
            match ScreenSaverProxyBlocking::new(&conn)
                .map(|proxy| proxy.inhibit("nova", "Video playback"))
            {
                Ok(Ok(cookie)) => {
                    state.cookie = Some(cookie);
                    return;
                }
                Ok(Err(e)) | Err(e) => {
                    eprintln!("nova player: screensaver inhibit unavailable ({e}); trying logind");
                }
            }
            // Fallback: systemd-logind idle block (bare window managers).
            match LoginManagerProxyBlocking::new(&conn)
                .map(|proxy| proxy.inhibit("idle", "nova", "Video playback", "block"))
            {
                Ok(Ok(fd)) => {
                    state.login_fd = Some(fd.into());
                }
                Ok(Err(e)) | Err(e) => {
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
        assert_eq!(action_for(true, false), Action::Acquire);
        assert_eq!(action_for(false, true), Action::Release);
        assert_eq!(action_for(true, true), Action::Keep);
        assert_eq!(action_for(false, false), Action::Keep);
    }
}

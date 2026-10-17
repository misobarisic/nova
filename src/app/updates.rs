//! Device-local update state. UI ownership stays on the event loop; only the
//! bounded HTTP request and JSON decoding run on the worker.
use super::*;
use nova_media::net::updates::{self as transport, Response};
use std::cell::{Cell, RefCell};

const KEY: &str = "updates:v1";
const DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

impl Release {
    fn version(&self) -> Option<semver::Version> {
        if self.draft || self.prerelease || self.tag_name.len() > 128 {
            return None;
        }
        let version =
            semver::Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name))
                .ok()?;
        version.pre.is_empty().then_some(version)
    }

    fn newer_than(&self, base: &str) -> bool {
        self.version()
            .zip(semver::Version::parse(base).ok())
            .is_some_and(|(latest, current)| latest.cmp_precedence(&current).is_gt())
    }

    fn url(&self) -> Option<String> {
        // Only a parsed version can form the path; never open a server-supplied URL.
        self.version()?;
        Some(format!(
            "https://github.com/misobarisic/nova/releases/tag/{}",
            self.tag_name
        ))
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct LocalState {
    automatic: Option<bool>,
    next_check: u64,
    retry_at: u64,
    notice_after: u64,
    etag: Option<String>,
    release: Option<Release>,
}

impl LocalState {
    fn automatic(&self) -> bool {
        self.automatic.unwrap_or(true)
    }

    fn due(&self, now: u64, manual: bool, is_release: bool) -> bool {
        now >= self.retry_at
            && (manual || (is_release && self.automatic() && now >= self.next_check))
    }
}

type CheckResult = Result<Option<(Release, Option<String>)>, transport::Error>;

struct Controller {
    app: slint::Weak<AppWindow>,
    state: RefCell<LocalState>,
    checking: Cell<bool>,
    pending: RefCell<Option<(std::sync::mpsc::Receiver<CheckResult>, bool)>>,
}

impl Controller {
    fn save(&self) {
        if let Ok(json) = serde_json::to_string(&*self.state.borrow()) {
            storage::set_str(KEY, &json);
        }
    }

    fn publish(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let state = self.state.borrow();
        let available = state
            .release
            .as_ref()
            .is_some_and(|r| r.newer_than(crate::version::BASE));
        app.set_update_available(available);
        app.set_update_latest_version(
            state
                .release
                .as_ref()
                .map(|r| r.tag_name.as_str())
                .unwrap_or_default()
                .into(),
        );
        if !self.checking.get() {
            app.set_update_status(if available {
                3
            } else if state.release.is_some() {
                2
            } else {
                0
            });
        }
    }

    fn notice(&self, now: u64, manual: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let mut state = self.state.borrow_mut();
        if app.get_update_available()
            && (manual
                || (crate::version::is_release() && state.automatic() && now >= state.notice_after))
        {
            app.set_update_notice_visible(true);
            state.notice_after = now.saturating_add(DAY);
            drop(state);
            self.save();
        }
    }

    fn check(self: &Rc<Self>, manual: bool) {
        let now = now_secs();
        if self.checking.get() || !remote_playback::is_foreground() {
            return;
        }
        if !self
            .state
            .borrow()
            .due(now, manual, crate::version::is_release())
        {
            if manual && let Some(app) = self.app.upgrade() {
                app.set_update_status(5);
            }
            return;
        }
        self.checking.set(true);
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.set_update_status(1);
        let etag = {
            let mut state = self.state.borrow_mut();
            // Record attempts too, so offline restarts do not hammer GitHub.
            state.next_check = now.saturating_add(15 * 60);
            state.etag.clone().filter(|_| state.release.is_some())
        };
        self.save();
        // Workers send semantic state; the retained event-loop timer owns all
        // UI access, and dropping the window drops its receiver safely.
        let (tx, rx) = std::sync::mpsc::channel();
        *self.pending.borrow_mut() = Some((rx, manual));
        thread::spawn(move || {
            let result = transport::fetch_latest(etag.as_deref(), crate::version::BASE).and_then(
                |response| match response {
                    Response::Unchanged => Ok(None),
                    Response::Release { body, etag } => {
                        let release: Release =
                            serde_json::from_slice(&body).map_err(|e| transport::Error {
                                message: e.to_string(),
                                retry_at: None,
                            })?;
                        if release.version().is_none() {
                            return Err(transport::Error {
                                message: "invalid stable release version".into(),
                                retry_at: None,
                            });
                        }
                        Ok(Some((release, etag)))
                    }
                },
            );
            let _ = tx.send(result);
        });
    }

    fn complete(&self) {
        let result = {
            let pending = self.pending.borrow();
            let Some((rx, manual)) = pending.as_ref() else {
                return;
            };
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => Err(transport::Error {
                    message: "update worker stopped".into(),
                    retry_at: None,
                }),
            };
            (result, *manual)
        };
        self.pending.borrow_mut().take();
        self.checking.set(false);
        let (result, manual) = result;
        let now = now_secs();
        match result {
            Ok(release) => {
                let mut state = self.state.borrow_mut();
                if let Some((release, etag)) = release {
                    state.release = Some(release);
                    state.etag = etag;
                }
                state.next_check = now.saturating_add(DAY);
                state.retry_at = 0;
                drop(state);
                self.publish();
                self.notice(now, manual);
            }
            Err(error) => {
                eprintln!("nova: update check failed: {}", error.message);
                let mut state = self.state.borrow_mut();
                state.retry_at = error.retry_at.unwrap_or_default();
                state.next_check = state.next_check.max(state.retry_at);
                drop(state);
                if let Some(app) = self.app.upgrade() {
                    app.set_update_status(if error.retry_at.is_some() { 5 } else { 4 });
                }
            }
        }
        self.save();
    }
}

pub(super) fn install(app: &AppWindow) -> slint::Timer {
    let state = storage::get_str(KEY)
        .filter(|s| s.len() <= 16 * 1024)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let controller = Rc::new(Controller {
        app: app.as_weak(),
        state: RefCell::new(state),
        checking: Cell::new(false),
        pending: RefCell::new(None),
    });
    app.set_update_checks_enabled(true);
    app.set_update_is_development(!crate::version::is_release());
    app.set_update_automatic(controller.state.borrow().automatic());
    controller.publish();
    let c = controller.clone();
    app.on_update_check_requested(move || c.check(true));
    let c = controller.clone();
    app.on_update_automatic_changed(move |enabled| {
        c.state.borrow_mut().automatic = Some(enabled);
        if let Some(app) = c.app.upgrade() {
            app.set_update_automatic(enabled);
            if !enabled {
                app.set_update_notice_visible(false);
            }
        }
        c.save();
        if enabled {
            c.check(false);
        }
    });
    let c = controller.clone();
    app.on_update_release_requested(move || {
        let url = c
            .state
            .borrow()
            .release
            .as_ref()
            .filter(|r| r.newer_than(crate::version::BASE))
            .and_then(Release::url);
        if let Some(url) = url {
            if let Some(app) = c.app.upgrade() {
                app.set_update_notice_visible(false);
            }
            if let Err(error) = crate::player::open_browser(&url) {
                eprintln!("nova: could not open release: {error}");
            }
        }
    });
    let timer = slint::Timer::default();
    // This timer is owned by run(), never Android's headless sync service.
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            controller.complete();
            let Some(app) = controller.app.upgrade() else {
                return;
            };
            if !remote_playback::is_foreground()
                || app.get_player_open()
                || app.get_onboarding_visible()
            {
                return;
            }
            controller.notice(now_secs(), false);
            controller.check(false);
        },
    );
    timer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
        }
    }

    #[test]
    fn releases_compare_semantically_and_never_offer_downgrades() {
        assert!(release("v0.0.10").newer_than("0.0.9"));
        assert!(!release("v0.0.9").newer_than("0.0.10"));
        assert!(!release("v0.0.10+build.2").newer_than("0.0.10+build.1"));
        for tag in [
            "v0.0.11-beta.1",
            "latest",
            "../../evil",
            "https://example.com",
        ] {
            assert!(!release(tag).newer_than("0.0.10"));
            assert!(release(tag).url().is_none());
        }
        let mut draft = release("v9.0.0");
        draft.draft = true;
        assert!(!draft.newer_than("0.0.10"));
        let mut prerelease = release("v9.0.0");
        prerelease.prerelease = true;
        assert!(!prerelease.newer_than("0.0.10"));
    }

    #[test]
    fn automatic_checks_are_local_throttled_and_release_only() {
        let mut state = LocalState::default();
        assert!(state.due(100, false, true));
        assert!(!state.due(100, false, false));
        assert!(state.due(100, true, false));
        state.automatic = Some(false);
        assert!(!state.due(100, false, true));
        state.next_check = 500;
        assert!(state.due(100, true, false));
        state.retry_at = 200;
        assert!(!state.due(100, true, true));
        assert!(state.due(200, true, true));
    }
}

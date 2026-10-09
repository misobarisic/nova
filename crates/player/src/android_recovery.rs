//! Event-loop-owned recovery policy, independent of Android/GL so lifecycle
//! orderings can be tested on the desktop. Only a lost renderer requires a
//! reload; an activity resume with its original window intact does not.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Playback {
    pub session: u64,
    pub position: f64,
    pub paused: bool,
}

#[derive(Debug)]
pub(super) struct Recovery {
    next_session: u64,
    playback: Option<Playback>,
    submitted: bool,
    foreground: bool,
    resume_playback: bool,
    renderer_ready: bool,
    pending: bool,
    loading: bool,
    reload_queued: bool,
}

impl Recovery {
    pub fn new(foreground: bool) -> Self {
        Self {
            next_session: 0,
            playback: None,
            submitted: false,
            foreground,
            resume_playback: false,
            renderer_ready: false,
            pending: false,
            loading: false,
            reload_queued: false,
        }
    }

    pub fn begin(&mut self, position: f64) {
        self.next_session += 1;
        self.playback = Some(Playback {
            session: self.next_session,
            position: position.max(0.0),
            paused: !self.foreground,
        });
        self.resume_playback = !self.foreground;
        self.submitted = false;
        self.pending = false;
        self.loading = true;
        self.reload_queued = false;
    }

    pub fn close(&mut self) {
        self.playback = None;
        self.resume_playback = false;
        self.submitted = false;
        self.pending = false;
        self.loading = false;
        self.reload_queued = false;
    }

    pub fn submitted(&mut self) {
        self.submitted = self.playback.is_some();
        self.reload_queued = false;
    }

    pub fn seek(&mut self, position: f64) {
        if let Some(playback) = self.playback.as_mut() {
            playback.position = position.max(0.0);
        }
    }

    pub fn load_failed(&mut self) {
        self.reload_queued = false;
    }

    pub fn pause(&mut self) {
        if self.foreground {
            self.resume_playback = self.playback.is_some_and(|p| !p.paused);
        }
        self.foreground = false;
        if let Some(playback) = self.playback.as_mut() {
            playback.paused = true;
        }
    }

    pub fn resume(&mut self, auto_continue: bool) -> bool {
        let play =
            !self.foreground && auto_continue && self.resume_playback && self.playback.is_some();
        self.foreground = true;
        self.resume_playback = false;
        if play && let Some(playback) = self.playback.as_mut() {
            playback.paused = false;
        }
        play
    }

    pub fn teardown(&mut self) {
        self.renderer_ready = false;
        // A file already submitted needs a new VO, even if initial loading
        // has not produced time-pos/duration yet. A queued initial file will
        // instead load normally once the renderer becomes ready.
        self.pending |= self.submitted;
    }

    pub fn setup(&mut self) {
        self.renderer_ready = true;
    }

    pub fn observe(&mut self, position: Option<f64>, paused: Option<bool>) {
        if !self.submitted {
            return;
        }
        if let Some(playback) = self.playback.as_mut() {
            if let Some(position) = position.filter(|p| p.is_finite() && *p >= 0.0) {
                // Rebuilding the VO can briefly blank time-pos (including a
                // zero value). Keep the last position while that happens;
                // positive live audio-clock readings still advance it.
                if (!self.pending && !self.loading) || position > 0.0 || playback.position == 0.0 {
                    playback.position = position;
                    self.loading = false;
                }
            }
            // A pre-pause property observation must not undo the activity's
            // explicit pause while it is backgrounded.
            if let Some(paused) = paused.filter(|_| self.foreground) {
                playback.paused = paused;
            }
        }
    }

    pub fn take_reload(&mut self) -> Option<Playback> {
        if !self.pending || !self.foreground || !self.renderer_ready {
            return None;
        }
        let playback = self.playback?;
        self.pending = false;
        self.loading = true;
        self.reload_queued = true;
        Some(playback)
    }

    pub fn waiting(&self) -> bool {
        !self.foreground || !self.renderer_ready || self.pending || self.reload_queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(paused: bool) -> Recovery {
        let mut recovery = Recovery::new(true);
        recovery.setup();
        recovery.begin(12.0);
        recovery.submitted();
        recovery.observe(Some(42.5), Some(paused));
        recovery
    }

    #[test]
    fn resume_and_setup_in_either_order_produce_one_reload() {
        for resume_first in [true, false] {
            let mut recovery = playing(false);
            recovery.pause();
            recovery.teardown();
            if resume_first {
                recovery.resume(false);
            } else {
                recovery.setup();
            }
            assert!(recovery.waiting());
            // Includes a timer tick between the two lifecycle events.
            assert_eq!(recovery.take_reload(), None);
            if resume_first {
                recovery.setup();
            } else {
                recovery.resume(false);
            }
            assert_eq!(recovery.take_reload().unwrap().position, 42.5);
            recovery.setup();
            recovery.resume(false);
            assert_eq!(recovery.take_reload(), None);
        }
    }

    #[test]
    fn duplicates_and_resume_without_window_loss_do_not_reload() {
        let mut recovery = playing(false);
        recovery.pause();
        recovery.resume(false);
        assert_eq!(recovery.take_reload(), None);
        recovery.teardown();
        recovery.teardown();
        recovery.setup();
        recovery.setup();
        recovery.resume(false);
        assert!(recovery.take_reload().is_some());
        assert_eq!(recovery.take_reload(), None);
        // Rotation in the foreground does not need an activity resume.
        recovery.teardown();
        recovery.setup();
        assert!(recovery.take_reload().is_some());
    }

    #[test]
    fn paused_playback_and_missing_properties_retain_the_snapshot() {
        let mut recovery = playing(true);
        recovery.pause();
        recovery.teardown();
        recovery.observe(None, None);
        recovery.observe(Some(0.0), None);
        assert_eq!(recovery.take_reload(), None);
        recovery.resume(false);
        assert_eq!(recovery.take_reload(), None);
        recovery.setup();
        let playback = recovery.take_reload().unwrap();
        assert_eq!(playback.position, 42.5);
        assert!(playback.paused);
    }

    #[test]
    fn failed_reload_does_not_leave_the_controller_waiting_forever() {
        let mut recovery = playing(false);
        recovery.teardown();
        recovery.setup();
        assert!(recovery.take_reload().is_some());
        assert!(recovery.waiting());
        recovery.load_failed();
        assert!(!recovery.waiting());
        assert_eq!(recovery.take_reload(), None);
    }

    #[test]
    fn queued_reload_and_seek_to_zero_preserve_the_snapshot() {
        let mut recovery = playing(true);
        recovery.teardown();
        recovery.setup();
        assert_eq!(recovery.take_reload().unwrap().position, 42.5);
        assert!(recovery.waiting());
        recovery.observe(Some(0.0), None);
        // A second window loss before the queued load is drawn keeps its state.
        recovery.teardown();
        recovery.setup();
        assert_eq!(recovery.take_reload().unwrap().position, 42.5);
        recovery.submitted();
        recovery.seek(0.0);
        recovery.observe(None, None);
        recovery.teardown();
        recovery.setup();
        let playback = recovery.take_reload().unwrap();
        assert_eq!(playback.position, 0.0);
        assert!(playback.paused);
    }

    #[test]
    fn background_pause_survives_stale_properties_and_resume() {
        let mut recovery = playing(false);
        recovery.pause();
        recovery.teardown();
        recovery.observe(Some(58.0), Some(false));
        recovery.setup();
        recovery.resume(false);
        let reload = recovery.take_reload().unwrap();
        assert_eq!(reload.position, 58.0);
        assert!(reload.paused);
    }

    #[test]
    fn auto_continue_only_restores_previously_playing_sessions() {
        for paused in [true, false] {
            let mut recovery = playing(paused);
            recovery.pause();
            recovery.pause();
            recovery.teardown();
            recovery.observe(None, Some(false));
            recovery.setup();
            assert_eq!(recovery.resume(true), !paused);
            assert_eq!(recovery.take_reload().unwrap().paused, paused);
            assert!(!recovery.resume(true), "duplicate resume cannot play again");
        }
        let mut recovery = playing(false);
        recovery.pause();
        recovery.close();
        assert!(!recovery.resume(true), "closed playback cannot restart");
    }

    #[test]
    fn background_without_window_loss_stays_paused_without_reload() {
        let mut recovery = playing(false);
        recovery.pause();
        recovery.pause();
        recovery.observe(None, Some(false));
        recovery.resume(false);
        assert!(recovery.playback.unwrap().paused);
        assert_eq!(recovery.take_reload(), None);
        // An explicit Play in the foreground still resumes normally.
        recovery.observe(Some(43.0), Some(false));
        assert!(!recovery.playback.unwrap().paused);
    }

    #[test]
    fn close_or_replacement_cancels_old_session_even_for_the_same_url() {
        for replace in [true, false] {
            let mut recovery = playing(true);
            recovery.pause();
            recovery.teardown();
            if replace {
                recovery.begin(7.0);
            } else {
                recovery.close();
            }
            recovery.resume(false);
            recovery.setup();
            assert_eq!(recovery.take_reload(), None);
            if replace {
                recovery.submitted();
                recovery.teardown();
                recovery.setup();
                assert_eq!(
                    recovery.take_reload(),
                    Some(Playback {
                        session: 2,
                        position: 7.0,
                        paused: true
                    })
                );
            }
        }
    }

    #[test]
    fn background_during_initial_loading_preserves_the_requested_start() {
        for submitted in [true, false] {
            let mut recovery = Recovery::new(true);
            recovery.setup();
            recovery.begin(120.0);
            if submitted {
                recovery.submitted();
            }
            recovery.pause();
            recovery.teardown();
            recovery.observe(None, None);
            recovery.observe(Some(0.0), None);
            recovery.setup();
            recovery.resume(false);
            if submitted {
                let reload = recovery.take_reload().unwrap();
                assert_eq!(reload.position, 120.0);
                assert!(reload.paused);
            } else {
                assert_eq!(recovery.take_reload(), None);
                assert!(recovery.playback.unwrap().paused);
            }
        }
    }
}

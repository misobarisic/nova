//! Device/session-local sleep policy. No state is persisted or synced.
use super::*;
use nova_playback_api::{MediaIdentity, SleepTimerMode, SleepTimerStatus};

fn same_item(a: &MediaIdentity, b: &MediaIdentity) -> bool {
    a.media_type == b.media_type && a.title_id == b.title_id && a.episode_id == b.episode_id
}

#[derive(Default)]
pub(super) struct State {
    mode: Option<SleepTimerMode>,
    deadline: Option<u64>,
    remaining: u8,
    item: Option<MediaIdentity>,
    last_eof: Option<u64>,
    ignored_eof: Option<u64>,
    successor: Option<(MediaIdentity, u64)>,
    run: Option<u64>,
    stopped: bool,
}
impl State {
    fn arm(&mut self, mode: SleepTimerMode, item: MediaIdentity, now: u64) {
        *self = Self {
            deadline: match mode {
                SleepTimerMode::Minutes(n) => Some(now.saturating_add(n as u64 * 60_000)),
                _ => None,
            },
            remaining: match mode {
                SleepTimerMode::Episodes(n) => n,
                _ => 0,
            },
            mode: Some(mode),
            item: Some(item),
            ..Default::default()
        };
    }
    pub(super) fn snapshot(&self, now: u64) -> Option<SleepTimerStatus> {
        Some(SleepTimerStatus {
            mode: self.mode.clone()?,
            remaining_seconds: self.deadline.map_or(0, |at| {
                at.saturating_sub(now).div_ceil(1000).min(14_400) as u32
            }),
            remaining_episodes: self.remaining,
        })
    }
    fn expired(&self, now: u64) -> bool {
        self.stopped || self.deadline.is_some_and(|at| now >= at)
    }
    fn observe_start(&mut self, item: &MediaIdentity) {
        let successor = self
            .successor
            .take()
            .filter(|(next, run)| same_item(next, item) && self.run.is_none_or(|own| own == *run));
        if let Some((_, run)) = &successor {
            self.run = Some(*run);
        }
        let Some(armed) = &self.item else { return };
        match self.mode {
            Some(SleepTimerMode::EndOfItem) if !same_item(armed, item) => *self = Self::default(),
            Some(SleepTimerMode::Episodes(_)) => {
                let same_series = armed.media_type == item.media_type
                    && armed.title_id == item.title_id
                    && item.media_type == "series";
                if !same_item(armed, item) && !same_series && successor.is_none() {
                    *self = Self::default();
                } else {
                    self.item = Some(item.clone());
                }
            }
            _ => {}
        }
    }
    fn eof(&mut self, item: &MediaIdentity, native: u64) -> bool {
        if self.last_eof == Some(native) || self.ignored_eof == Some(native) {
            return self.stopped;
        }
        if !self
            .item
            .as_ref()
            .is_some_and(|armed| same_item(armed, item))
        {
            return false;
        }
        self.last_eof = Some(native);
        match self.mode {
            Some(SleepTimerMode::EndOfItem) => self.stopped = true,
            Some(SleepTimerMode::Episodes(_)) if item.media_type == "series" => {
                self.remaining = self.remaining.saturating_sub(1);
                self.stopped = self.remaining == 0;
            }
            _ => {}
        }
        self.stopped
    }
}

impl Bridge {
    pub(super) fn cast_sleep_tick(&self, finished: Option<(u64, MediaIdentity)>) -> bool {
        let mut state = self.shared.lock().unwrap();
        let timer = &mut state.sleep_timer;
        timer.mode.is_some()
            && (timer.expired(crate::player::elapsed_realtime_ms())
                || finished.is_some_and(|(id, item)| timer.eof(&item, id)))
    }

    /// The temporary close grant must never survive into an asynchronous picker.
    pub(super) fn sleep_timer_finish_successor_close(&self) {
        self.shared.lock().unwrap().sleep_timer.successor = None;
    }
    pub(super) fn sleep_timer_playback_closed(&self) {
        if self.shared.lock().unwrap().sleep_timer.successor.is_none() {
            self.cancel_sleep_timer();
        }
    }
    pub(super) fn cancel_sleep_timer(&self) {
        self.shared.lock().unwrap().sleep_timer = State::default();
        self.player.set_sleep_deadline(None);
        self.publish_sleep_timer();
    }
    pub(super) fn set_sleep_timer(&self, mode: SleepTimerMode) -> Result<(), String> {
        if !nova_playback_api::PlaybackAction::SetSleepTimer(mode.clone()).valid_payload() {
            return Err("Invalid sleep timer".into());
        }
        let item = self
            .current_playback_identity()
            .ok_or_else(|| text::tr("Sleep timer unavailable").to_owned())?;
        if !self.shared.lock().unwrap().casting.active
            && !self.app().is_some_and(|app| app.get_player_open())
        {
            return Err(text::tr("Sleep timer unavailable").into());
        }
        if (self.party_active() && !matches!(mode, SleepTimerMode::Minutes(_)))
            || (matches!(mode, SleepTimerMode::Episodes(_)) && item.media_type != "series")
        {
            return Err(text::tr("Episode timer requires series playback").into());
        }
        let deadline = {
            let mut state = self.shared.lock().unwrap();
            state
                .sleep_timer
                .arm(mode, item, crate::player::elapsed_realtime_ms());
            state.sleep_timer.deadline
        };
        self.player.set_sleep_deadline(deadline);
        if let Some(app) = self.app() {
            app.set_sleep_timer_notice("".into());
        }
        self.refresh_sleep_timer(); // An End-of-item timer armed on an EOF frame stops now.
        Ok(())
    }
    pub(super) fn extend_sleep_timer(&self) -> Result<(), String> {
        let deadline = {
            let mut state = self.shared.lock().unwrap();
            let timer = &mut state.sleep_timer;
            let Some(at) = timer.deadline else {
                return Err(text::tr("Choose a duration timer first").into());
            };
            let now = crate::player::elapsed_realtime_ms();
            if now >= at {
                drop(state);
                self.refresh_sleep_timer();
                return Err(text::tr("Sleep timer finished").into());
            }
            let at = at
                .saturating_add(15 * 60_000)
                .min(now.saturating_add(240 * 60_000));
            timer.deadline = Some(at);
            Some(at)
        };
        self.player.set_sleep_deadline(deadline);
        self.publish_sleep_timer();
        Ok(())
    }
    pub(super) fn sleep_timer_start(&self, item: &MediaIdentity) {
        self.shared.lock().unwrap().sleep_timer.observe_start(item);
        self.publish_sleep_timer();
    }
    pub(super) fn sleep_timer_note_successor(&self, item: MediaIdentity, run: u64) {
        self.shared.lock().unwrap().sleep_timer.successor = Some((item, run));
    }
    pub(super) fn sleep_timer_note_seek(&self, position: f64) {
        let near_end = self.app().is_some_and(|app| {
            app.get_duration() > 0.0 && position >= app.get_duration() as f64 - 1.0
        });
        self.shared.lock().unwrap().sleep_timer.ignored_eof =
            near_end.then(|| self.player.screenshot_session());
    }
    pub(super) fn sleep_timer_blocks_successor(&self, item: &MediaIdentity) -> bool {
        if self.refresh_sleep_timer() {
            return true;
        }
        let movie = item.media_type == "movie"
            && matches!(
                self.shared.lock().unwrap().sleep_timer.mode,
                Some(SleepTimerMode::Episodes(_))
            );
        if movie {
            self.stop_for_sleep_timer(true);
        }
        movie
    }
    pub(super) fn refresh_sleep_timer(&self) -> bool {
        if self.shared.lock().unwrap().casting.active {
            self.publish_sleep_timer();
            return false;
        }
        if self.party_active()
            && matches!(
                self.shared.lock().unwrap().sleep_timer.mode,
                Some(SleepTimerMode::EndOfItem | SleepTimerMode::Episodes(_))
            )
        {
            // Joining a party retires an earlier item/count policy; party
            // playback only exposes device-local duration ownership.
            self.cancel_sleep_timer();
            return false;
        }
        let expired = self.player.sleep_deadline_expired();
        // EOF belongs to the loaded target, never to a newer source lookup's
        // intent while the old native frame is still retained.
        let item = self.shared.lock().unwrap().playback.as_ref().and_then(|p| {
            Some(MediaIdentity {
                media_type: p.history.as_ref()?.type_.clone(),
                title_id: p.series_id.clone(),
                episode_id: p.episode_id.clone(),
                ..Default::default()
            })
        });
        let ended = self.player.playback_readiness().ended;
        let stop = {
            let mut state = self.shared.lock().unwrap();
            let timer = &mut state.sleep_timer;
            timer.mode.is_some()
                && (expired
                    || timer.expired(crate::player::elapsed_realtime_ms())
                    || (ended
                        && item
                            .as_ref()
                            .is_some_and(|item| timer.eof(item, self.player.screenshot_session()))))
        };
        if stop {
            self.stop_for_sleep_timer(false);
        } else {
            self.publish_sleep_timer();
        }
        stop
    }
    fn stop_for_sleep_timer(&self, movie_boundary: bool) {
        if let Some((position, duration)) = self.player.sleep_timer_progress()
            && position.is_finite()
            && duration.is_finite()
            && let Some(app) = self.app()
        {
            app.set_position(position.max(0.0) as f32);
            app.set_duration(duration.max(0.0) as f32);
        }
        self.note_player_progress_from_ui();
        self.persist_playback_checkpoint(false);
        // Leave the party before local Stop, so no group pause/stop is broadcast.
        if self.party_active() {
            self.party_leave();
        }
        self.cancel_sleep_timer();
        let _ = self.execute_playback_action(nova_playback_api::PlaybackAction::Stop);
        if let Some(app) = self.app() {
            app.set_sleep_timer_notice(
                text::tr(if movie_boundary {
                    "Episode timer finished the episode portion"
                } else {
                    "Sleep timer finished"
                })
                .into(),
            );
        }
    }
    fn publish_sleep_timer(&self) {
        let Some(app) = self.app() else { return };
        let item = self.current_playback_identity();
        let snapshot = self
            .shared
            .lock()
            .unwrap()
            .sleep_timer
            .snapshot(crate::player::elapsed_realtime_ms());
        let label = snapshot
            .as_ref()
            .map(|timer| {
                text::sleep_timer_status(
                    match timer.mode {
                        SleepTimerMode::Minutes(_) => 1,
                        SleepTimerMode::EndOfItem => 2,
                        SleepTimerMode::Episodes(_) => 3,
                    },
                    timer.remaining_seconds,
                    timer.remaining_episodes,
                )
            })
            .unwrap_or_else(|| text::tr("Off").into());
        app.set_sleep_timer_label(label.into());
        app.set_sleep_timer_active(snapshot.is_some());
        app.set_sleep_timer_duration(
            snapshot
                .as_ref()
                .is_some_and(|s| matches!(s.mode, SleepTimerMode::Minutes(_))),
        );
        app.set_sleep_timer_available(item.is_some() && app.get_player_open());
        app.set_sleep_timer_episodes_available(
            item.is_some_and(|item| item.media_type == "series") && !self.party_active(),
        );
        app.set_sleep_timer_end_available(!self.party_active());
    }
    pub(super) fn sleep_timer_ui(&self, mode: i32, value: i32) {
        use nova_playback_api::PlaybackAction as A;
        let action = match mode {
            0 => A::CancelSleepTimer,
            1 => A::SetSleepTimer(SleepTimerMode::Minutes(value.clamp(1, 240) as u16)),
            2 => A::SetSleepTimer(SleepTimerMode::EndOfItem),
            3 => A::SetSleepTimer(SleepTimerMode::Episodes(value.clamp(1, 10) as u8)),
            4 => A::ExtendSleepTimer,
            _ => return,
        };
        let _ = self.dispatch_playback_action(action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn episode(title: &str, episode: &str) -> MediaIdentity {
        MediaIdentity {
            media_type: "series".into(),
            title_id: title.into(),
            episode_id: episode.into(),
            ..Default::default()
        }
    }
    #[test]
    fn duration_counts_pause_buffer_and_suspend_without_wall_clock() {
        let mut timer = State::default();
        timer.arm(SleepTimerMode::Minutes(30), episode("show", "e1"), 1000);
        assert!(!timer.expired(1_800_999));
        // One resume observation can jump over suspended time. No pause,
        // buffering, playback-position or wall-clock input changes the deadline.
        assert!(timer.expired(1_801_000));
        timer.observe_start(&episode("other", "e8"));
        assert!(timer.expired(1_801_001));
    }
    #[test]
    fn end_item_survives_retry_but_different_typed_item_cancels() {
        let mut timer = State::default();
        let item = episode("show", "e1");
        timer.arm(SleepTimerMode::EndOfItem, item.clone(), 0);
        timer.observe_start(&item);
        assert!(!timer.eof(&episode("other", "e1"), 1));
        assert!(timer.eof(&item, 2));
        timer.arm(SleepTimerMode::EndOfItem, item, 0);
        timer.observe_start(&episode("show", "e2"));
        assert!(timer.mode.is_none());
    }
    #[test]
    fn episode_count_deduplicates_loads_preserves_successors_and_never_counts_movie() {
        let mut timer = State::default();
        let first = episode("show", "e1");
        timer.arm(SleepTimerMode::Episodes(3), first.clone(), 0);
        assert!(!timer.eof(&first, 10));
        assert!(!timer.eof(&first, 10));
        assert_eq!(timer.remaining, 2);
        let second = episode("other", "e2");
        timer.successor = Some((second.clone(), 22));
        timer.observe_start(&second);
        assert_eq!(timer.run, Some(22));
        assert!(!timer.eof(&second, 11));
        assert_eq!(timer.remaining, 1);
        let movie = MediaIdentity {
            media_type: "movie".into(),
            title_id: "film".into(),
            episode_id: "film".into(),
            ..Default::default()
        };
        assert!(!timer.eof(&movie, 12));
        assert_eq!(timer.remaining, 1);
        timer.observe_start(&episode("unrelated", "e1"));
        assert!(timer.mode.is_none());
    }
    #[test]
    fn manual_skip_does_not_count_and_seek_eof_is_ignored() {
        let mut timer = State::default();
        timer.arm(SleepTimerMode::Episodes(2), episode("show", "e1"), 0);
        timer.observe_start(&episode("show", "e2"));
        assert_eq!(timer.remaining, 2);
        timer.ignored_eof = Some(9);
        assert!(!timer.eof(&episode("show", "e2"), 9));
        assert_eq!(timer.remaining, 2);
        timer.ignored_eof = None; // A seek away re-arms genuine natural EOF.
        assert!(!timer.eof(&episode("show", "e2"), 9));
        assert_eq!(timer.remaining, 1);
        timer.observe_start(&episode("show", "e3"));
        assert!(timer.eof(&episode("show", "e3"), 10));
    }
    #[test]
    fn replacement_lookup_cannot_count_the_old_native_eof_as_the_new_item() {
        let mut timer = State::default();
        timer.arm(SleepTimerMode::Episodes(2), episode("show", "e1"), 0);
        timer.observe_start(&episode("show", "e2"));
        assert!(!timer.eof(&episode("show", "e1"), 1));
        assert_eq!(timer.remaining, 2);
    }
    #[test]
    fn cancellation_removes_deadline_and_restart_has_no_armed_state() {
        let mut timer = State::default();
        timer.arm(SleepTimerMode::Minutes(1), episode("show", "e1"), 50);
        assert!(timer.snapshot(51).is_some());
        timer = State::default();
        assert!(!timer.expired(u64::MAX));
        assert!(timer.snapshot(u64::MAX).is_none());
    }
}

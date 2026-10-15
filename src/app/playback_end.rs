//! Delayed, session-scoped choices over the retained native EOF frame.
use super::*;
use std::time::Instant;

const MENU_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Session {
    native: u64,
    playback: u64,
    type_: String,
    title: String,
    episode: String,
}

#[derive(Default)]
pub(super) struct State {
    session: Option<Session>,
    since: Option<Instant>,
    suppressed: bool,
    visible: bool,
}

impl State {
    fn observe(&mut self, session: Option<Session>, ended: bool, eligible: bool, now: Instant) {
        if self.session != session {
            *self = Self {
                session,
                ..Self::default()
            };
        }
        if self.session.is_none() || !ended {
            self.since = None;
            self.suppressed = false;
            self.visible = false;
        } else if !eligible {
            // Backgrounding must not accrue a timer that appears immediately
            // on return, nor revive a menu the user already dismissed.
            self.since = None;
            self.suppressed = true;
            self.visible = false;
        } else if !self.suppressed {
            self.visible =
                now.saturating_duration_since(*self.since.get_or_insert(now)) >= MENU_DELAY;
        }
    }

    fn stay(&mut self) {
        self.since = None;
        self.suppressed = true;
        self.visible = false;
    }

    fn accepts(&self, session: &Session) -> bool {
        self.visible && self.session.as_ref() == Some(session)
    }
}

struct Context {
    session: Session,
    history: PlaybackHistory,
}

impl Context {
    fn preview(&self) -> MetaPreview {
        MetaPreview {
            id: self.session.title.clone(),
            type_: self.session.type_.clone(),
            name: self.history.title.clone(),
            poster: (!self.history.poster_url.is_empty()).then(|| self.history.poster_url.clone()),
            ..Default::default()
        }
    }
}

impl Bridge {
    fn playback_end_context(&self) -> Option<Context> {
        let native = self.player.screenshot_session();
        let state = self.shared.lock().unwrap();
        let target = state.playback.as_ref()?;
        let history = target.history.as_ref()?;
        if target.session == 0 || target.series_id.is_empty() || history.type_.is_empty() {
            return None;
        }
        Some(Context {
            session: Session {
                native,
                playback: target.session,
                type_: history.type_.clone(),
                title: target.series_id.clone(),
                episode: target.episode_id.clone(),
            },
            history: history.clone(),
        })
    }

    fn playback_end_eligible(&self, app: &AppWindow) -> bool {
        remote_playback::is_foreground()
            && app.get_player_open()
            && app.get_playback_started()
            && !app.get_resume_prompt_visible()
            && !app.get_party_page_open()
            && !app.get_remote_controls_open()
            && !app.get_remote_devices_open()
            && !self.party_active()
            && self
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .can_play_locally()
    }

    fn playback_end_videos(&self, context: &Context) -> Vec<Video> {
        let current = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|modal| {
                modal.id == context.session.title && modal.type_ == context.session.type_
            })
            .map(|modal| modal.videos.clone());
        current
            .filter(|videos| !videos.is_empty())
            .or_else(|| read_episodes_cache_for(&context.session.type_, &context.session.title))
            .unwrap_or_else(|| {
                if self.remote_receiver_playback() {
                    self.shared
                        .lock()
                        .unwrap()
                        .remote_playback
                        .receiver_videos
                        .clone()
                } else {
                    Vec::new()
                }
            })
    }

    fn playback_end_next_video(&self, context: &Context) -> Option<Video> {
        if context.session.type_ == "movie" {
            return None;
        }
        next_episode::successor(
            &self.playback_end_videos(context),
            &context.session.episode,
            today_days(),
        )
        .cloned()
    }

    pub(super) fn refresh_playback_end_menu(&self) {
        let Some(app) = self.app() else { return };
        let context = app
            .get_player_open()
            .then(|| self.playback_end_context())
            .flatten();
        // Native readiness validates the loaded path and excludes a queued
        // replacement/loading source. A near-end position is not EOF.
        let ended = self.player.playback_readiness().ended;
        let eligible = self.playback_end_eligible(&app);
        let visible = {
            let mut state = self.shared.lock().unwrap();
            state.playback_end.observe(
                context.as_ref().map(|context| context.session.clone()),
                ended,
                eligible,
                Instant::now(),
            );
            state.playback_end.visible
        };
        let next = visible
            .then(|| {
                context
                    .as_ref()
                    .and_then(|context| self.playback_end_next_video(context))
            })
            .flatten();
        app.set_playback_end_menu_has_next(next.is_some());
        app.set_playback_end_menu_next_title(
            next.as_ref()
                .map(episode_row_label)
                .unwrap_or_default()
                .into(),
        );
        #[cfg(feature = "desktop")]
        let was_visible = app.get_playback_end_menu_visible();
        app.set_playback_end_menu_visible(visible);
        #[cfg(feature = "desktop")]
        if visible && !was_visible {
            // The normal paused OSD can fade at the same five-second mark.
            // The new menu still needs a pointer before its first frame.
            desktop_window::sync_cursor(&app);
        }
    }

    pub(super) fn clear_playback_end_menu(&self) {
        self.shared.lock().unwrap().playback_end = State::default();
        if let Some(app) = self.app() {
            app.set_playback_end_menu_visible(false);
            app.set_playback_end_menu_has_next(false);
            app.set_playback_end_menu_next_title(SharedString::default());
        }
    }

    fn current_playback_end_action(&self) -> Option<Context> {
        let app = self.app()?;
        if !app.get_playback_end_menu_visible()
            || !self.playback_end_eligible(&app)
            || !self.player.playback_readiness().ended
        {
            return None;
        }
        let context = self.playback_end_context()?;
        let state = self.shared.lock().unwrap();
        state
            .playback_end
            .accepts(&context.session)
            .then_some(context)
    }

    pub(super) fn playback_end_stay(&self) {
        if self.current_playback_end_action().is_none() {
            return;
        }
        self.shared.lock().unwrap().playback_end.stay();
        if let Some(app) = self.app() {
            app.set_playback_end_menu_visible(false);
            app.invoke_osd_mouse_moved();
        }
    }

    pub(super) fn playback_end_details(&self) {
        let Some(context) = self.current_playback_end_action() else {
            return;
        };
        if self
            .execute_playback_action(nova_playback_api::PlaybackAction::Stop)
            .is_err()
        {
            return;
        }
        self.open_preview(context.preview(), 0, false, None, false);
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(false);
            app.set_stream_selector_open(false);
            self.detail_tab_picked(0);
        }
    }

    pub(super) fn playback_end_next(&self) {
        let Some(context) = self.current_playback_end_action() else {
            return;
        };
        let videos = self.playback_end_videos(&context);
        let Some(next) =
            next_episode::successor(&videos, &context.session.episode, today_days()).cloned()
        else {
            return;
        };
        let matches = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .is_some_and(|modal| {
                modal.id == context.session.title && modal.type_ == context.session.type_
            });
        if !matches {
            self.open_preview(context.preview(), 0, false, None, false);
        }
        {
            let mut state = self.shared.lock().unwrap();
            // Metadata may be cold or the user may have browsed another title.
            // Preserve the exact cached successor list for the existing picker.
            let Some(modal) = state.modal_item.as_mut().filter(|modal| {
                modal.id == context.session.title && modal.type_ == context.session.type_
            }) else {
                return;
            };
            modal.seasons = ordered_seasons(&videos);
            modal.videos = videos;
            let Some(target) = state.playback.as_mut().filter(|target| {
                target.session == context.session.playback
                    && target.series_id == context.session.title
                    && target.episode_id == context.session.episode
            }) else {
                return;
            };
            // Dismissing the small near-end banner does not remove the explicit
            // Next choice on the terminal menu.
            target.next_episode_dismissed = false;
            target.next_episode_id.clone_from(&next.id);
        }
        let Some(app) = self.app() else { return };
        let token = format!("{}:{}", context.session.playback, next.id);
        app.set_next_episode_token(token.clone().into());
        app.set_next_episode_visible(true);
        self.clear_playback_end_menu();
        self.choose_next_episode_streams(&token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(native: u64) -> Session {
        Session {
            native,
            playback: 1,
            type_: "series".into(),
            title: "title".into(),
            episode: "episode".into(),
        }
    }

    #[test]
    fn menu_requires_five_full_seconds_of_real_eof() {
        let now = Instant::now();
        let mut state = State::default();
        state.observe(Some(session(1)), false, true, now);
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(30));
        state.observe(
            Some(session(1)),
            true,
            true,
            now + Duration::from_millis(34_999),
        );
        assert!(!state.visible);
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(35));
        assert!(state.visible);
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(60));
        assert!(state.visible);
    }

    #[test]
    fn stay_suppresses_current_eof_but_seek_and_reend_rearms() {
        let now = Instant::now();
        let mut state = State::default();
        state.observe(Some(session(1)), true, true, now);
        state.observe(Some(session(1)), true, true, now + MENU_DELAY);
        state.stay();
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(100));
        assert!(!state.visible);
        state.observe(
            Some(session(1)),
            false,
            true,
            now + Duration::from_secs(101),
        );
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(102));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(107));
        assert!(state.visible);
    }

    #[test]
    fn replacement_same_episode_and_close_cancel_the_old_timer() {
        let now = Instant::now();
        let mut state = State::default();
        state.observe(Some(session(1)), true, true, now);
        state.observe(Some(session(2)), true, true, now + Duration::from_secs(4));
        state.observe(Some(session(2)), true, true, now + Duration::from_secs(5));
        assert!(!state.visible);
        state.observe(Some(session(2)), true, true, now + Duration::from_secs(9));
        assert!(state.visible);
        state.observe(None, false, false, now + Duration::from_secs(10));
        assert!(!state.visible);
        assert!(state.session.is_none());
    }

    #[test]
    fn foreground_bounce_cancels_current_eof_without_reviving_stay() {
        let now = Instant::now();
        let mut state = State::default();
        state.observe(Some(session(1)), true, true, now);
        state.observe(Some(session(1)), true, false, now + Duration::from_secs(4));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(40));
        assert!(!state.visible);
        state.observe(Some(session(1)), false, true, now + Duration::from_secs(41));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(42));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(47));
        assert!(state.visible);
        state.stay();
        state.observe(Some(session(1)), true, false, now + Duration::from_secs(48));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(100));
        assert!(!state.visible);
    }

    #[test]
    fn loading_or_seeking_before_delay_restarts_the_full_wait() {
        let now = Instant::now();
        let mut state = State::default();
        state.observe(Some(session(1)), true, true, now);
        state.observe(Some(session(1)), false, true, now + Duration::from_secs(4));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(8));
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(12));
        assert!(!state.visible);
        state.observe(Some(session(1)), true, true, now + Duration::from_secs(13));
        assert!(state.visible);
    }

    #[test]
    fn menu_actions_reject_replacement_sessions_and_dismissed_menus() {
        let now = Instant::now();
        let mut state = State::default();
        let current = session(1);
        state.observe(Some(current.clone()), true, true, now);
        assert!(
            !state.accepts(&current),
            "countdown is not an actionable menu"
        );
        state.observe(Some(current.clone()), true, true, now + MENU_DELAY);
        assert!(state.accepts(&current));
        assert!(
            !state.accepts(&session(2)),
            "even the same episode in a replacement native load is stale"
        );
        assert!(!state.accepts(&Session {
            playback: 2,
            ..current.clone()
        }));
        assert!(!state.accepts(&Session {
            type_: "movie".into(),
            ..current.clone()
        }));
        assert!(!state.accepts(&Session {
            title: "browsed title".into(),
            ..current.clone()
        }));
        state.stay();
        assert!(!state.accepts(&current));
        state.observe(None, false, false, now + Duration::from_secs(6));
        assert!(!state.accepts(&current));
    }

    #[test]
    fn details_preview_uses_the_captured_typed_playback_identity() {
        let context = Context {
            session: session(1),
            history: PlaybackHistory {
                title: "Actually played".into(),
                poster_url: "played poster".into(),
                ..Default::default()
            },
        };
        let preview = context.preview();
        assert_eq!(preview.id, "title");
        assert_eq!(preview.type_, "series");
        assert_eq!(preview.name, "Actually played");
        assert_eq!(preview.poster.as_deref(), Some("played poster"));
    }
}

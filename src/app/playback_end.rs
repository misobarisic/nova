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
    automatic_attempted: bool,
    automatic: Option<Automatic>,
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
            self.automatic_attempted = false;
            self.automatic = None;
        } else if !eligible {
            // Backgrounding must not accrue a timer that appears immediately
            // on return, nor revive a menu the user already dismissed.
            self.since = None;
            self.suppressed = true;
            self.visible = false;
            self.automatic = None;
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

// All publications are scoped to native load, playback, addon/settings revision
// and a unique attempt, so late replies can never revive a cancelled preview.
struct Automatic {
    ticket: Arc<()>,
    next: Video,
    settings: u64,
    owners: Vec<(String, u64)>,
    rows: Vec<Vec<StreamUi>>,
    pending: usize,
    candidate: Option<StreamUi>,
    since: Option<Instant>,
}

fn automatic_successor(videos: &[Video], current: &str) -> Option<Video> {
    let next = next_episode::successor(videos, current, today_days())?;
    (episode_has_air_date(next) && episode_is_out(next, today_days())).then(|| next.clone())
}

fn automatic_choice(
    rows: &[StreamUi],
    preference: Option<&StreamPreference>,
    torrents: bool,
) -> Option<StreamUi> {
    let rows: Vec<_> = rows
        .iter()
        .filter(|row| {
            !matches!(row.source, StreamSource::Unsupported)
                && !matches!(&row.source, StreamSource::Downloaded { path, .. } if !path.is_file())
                && (torrents || !matches!(row.source, StreamSource::Torrent { .. }))
        })
        .cloned()
        .collect();
    let index = preference
        .and_then(|p| recommended_stream(&rows, None, p))
        .or_else(|| (!rows.is_empty()).then_some(0))?;
    Some(rows[index].clone())
}

struct Context {
    session: Session,
    history: PlaybackHistory,
}

impl Context {
    fn lookup(&self, videos: Vec<Video>) -> ModalItem {
        ModalItem {
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: self.session.title.clone(),
            type_: self.session.type_.clone(),
            request_id: self.session.episode.clone(),
            seasons: ordered_seasons(&videos),
            videos,
            season_backdrops: HashMap::new(),
            season_index: 0,
            episode_page: 0,
            name: self.history.title.clone(),
            year: String::new(),
            poster_url: self.history.poster_url.clone(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: Vec::new(),
        }
    }
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
        if !ended || !eligible || context.is_none() {
            app.set_auto_next_preview(false);
        }
        let automatic_allowed = ended
            && eligible
            && !app.get_completion_prompt_visible()
            && !app.get_onboarding_visible()
            && app.get_remote_replacement_token().is_empty();
        if !automatic_allowed {
            self.shared.lock().unwrap().playback_end.automatic = None;
            app.set_auto_next_preview(false);
        }
        let automatic = context
            .as_ref()
            .is_some_and(|context| automatic_allowed && self.refresh_automatic_next(context, &app));
        let visible = visible && !automatic;
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
        if (visible && !was_visible) || app.get_auto_next_preview() {
            // The normal paused OSD can fade at the same five-second mark.
            // The new menu still needs a pointer before its first frame.
            desktop_window::sync_cursor(&app);
        }
    }

    fn automatic_valid(&self, context: &Context, automatic: &Automatic) -> bool {
        if self.app().is_none_or(|app| {
            app.get_completion_prompt_visible()
                || app.get_onboarding_visible()
                || !app.get_remote_replacement_token().is_empty()
        }) {
            return false;
        }
        if self.remote_receiver_playback() || self.remote_output_selected() {
            return false;
        }
        if automatic.candidate.as_ref().is_some_and(|row| {
            automatic_choice(
                std::slice::from_ref(row),
                None,
                active_torrent_settings().enabled,
            )
            .is_none()
        }) {
            return false;
        }
        let state = self.shared.lock().unwrap();
        state.cache_settings.auto_next_episode
            && !state.cache_settings.player_external
            && nova_providers::plugin_settings::settings_generation() == automatic.settings
            && state
                .installed
                .iter()
                .filter(|a| a.enabled && a.available)
                .map(|a| (a.url.clone(), a.generation))
                .collect::<Vec<_>>()
                == automatic.owners
            && state.playback_end.session.as_ref() == Some(&context.session)
            && !state.playback_end.suppressed
            && self
                .playback_end_next_video_unlocked(context, &state)
                .is_some_and(|next| next.id == automatic.next.id)
    }

    fn playback_end_next_video_unlocked(&self, context: &Context, state: &Shared) -> Option<Video> {
        let videos = state
            .modal_item
            .as_ref()
            .filter(|m| m.id == context.session.title && m.type_ == context.session.type_)
            .map(|m| m.videos.clone())
            .or_else(|| read_episodes_cache_for(&context.session.type_, &context.session.title))?;
        // Automatic playback needs positive release evidence; the explicit
        // source picker remains available for dateless provider episodes.
        automatic_successor(&videos, &context.session.episode)
    }

    fn refresh_automatic_next(&self, context: &Context, app: &AppWindow) -> bool {
        let begin = {
            let mut state = self.shared.lock().unwrap();
            let begin = !state.playback_end.automatic_attempted && !state.playback_end.suppressed;
            state.playback_end.automatic_attempted = true;
            begin && state.cache_settings.auto_next_episode && !state.cache_settings.player_external
        };
        if begin {
            self.begin_automatic_next(context);
        }
        let automatic = self.shared.lock().unwrap().playback_end.automatic.take();
        let Some(mut automatic_value) = automatic else {
            app.set_auto_next_preview(false);
            return false;
        };
        if !self.automatic_valid(context, &automatic_value) {
            app.set_auto_next_preview(false);
            return false;
        }
        let now = Instant::now();
        if automatic_value.candidate.is_none() && self.shared.lock().unwrap().playback_end.visible {
            app.set_auto_next_preview(false);
            return false;
        }
        if automatic_value.candidate.is_none() && automatic_value.pending == 0 {
            let mut rows: Vec<_> = self
                .downloads
                .jobs_for_media(
                    &context.session.type_,
                    &context.session.title,
                    &automatic_value.next.id,
                )
                .into_iter()
                .filter(|job| job.phase == crate::download::DownloadPhase::Completed)
                .filter_map(|job| {
                    let path = job.artifact_path.clone().filter(|path| path.is_file())?;
                    Some(StreamUi {
                        id: format!("download:{}", job.id),
                        choice_id: None,
                        owner_url: String::new(),
                        display: job.display.clone(),
                        card: downloaded_stream_card(&job),
                        source: StreamSource::Downloaded {
                            job_id: job.id.clone(),
                            path,
                        },
                        addon: job.addon.clone(),
                        download: Some(job),
                    })
                })
                .collect();
            rows.extend(automatic_value.rows.iter().flatten().cloned());
            let preference = stream_series_scope(&context.session.type_, &context.session.title)
                .and_then(|id| {
                    self.last_stream
                        .series_preference(&id, nova_config::now_secs())
                });
            automatic_value.candidate = automatic_choice(
                &rows,
                preference.as_ref(),
                active_torrent_settings().enabled,
            );
            if automatic_value.candidate.is_some() {
                automatic_value.since = Some(now);
            }
        }
        let waiting = automatic_value.candidate.is_none();
        if waiting && self.shared.lock().unwrap().playback_end.visible {
            // The ordinary five-second EOF deadline also bounds discovery.
            app.set_auto_next_preview(false);
            return false;
        }
        if let Some(row) = &automatic_value.candidate {
            app.set_auto_next_preview_title(episode_row_label(&automatic_value.next).into());
            app.set_auto_next_preview_source(row.card.title.clone().into());
            app.set_auto_next_preview_metadata(row.card.metadata.clone().into());
            let elapsed = now.saturating_duration_since(automatic_value.since.unwrap());
            app.set_auto_next_seconds((8_u64.saturating_sub(elapsed.as_secs())) as i32);
        }
        let launch = automatic_value
            .since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(8));
        self.shared.lock().unwrap().playback_end.automatic = Some(automatic_value);
        app.set_auto_next_preview(!waiting);
        if launch {
            self.play_automatic_next();
        }
        true
    }

    fn begin_automatic_next(&self, context: &Context) {
        if context.session.type_ == "movie"
            || self.remote_receiver_playback()
            || self.remote_output_selected()
        {
            return;
        }
        let videos = self.playback_end_videos(context);
        let (next, urls, owners) = {
            let state = self.shared.lock().unwrap();
            let Some(next) = self.playback_end_next_video_unlocked(context, &state) else {
                return;
            };
            // Use the playing title's confirmed episode mapping. Browsing a
            // different title cannot retarget the lookup or its aliases.
            let lookup = context.lookup(videos);
            let modal = state
                .modal_item
                .as_ref()
                .filter(|m| m.id == context.session.title && m.type_ == context.session.type_)
                .unwrap_or(&lookup);
            let urls = detail::stream_search_urls_for_context(&state.installed, modal, &next.id);
            let owners = state
                .installed
                .iter()
                .filter(|a| a.enabled && a.available)
                .map(|a| (a.url.clone(), a.generation))
                .collect();
            (next, urls, owners)
        };
        let ticket = Arc::new(());
        let settings = nova_providers::plugin_settings::settings_generation();
        self.shared.lock().unwrap().playback_end.automatic = Some(Automatic {
            ticket: ticket.clone(),
            next: next.clone(),
            settings,
            owners,
            rows: vec![Vec::new(); urls.len()],
            pending: urls.len(),
            candidate: None,
            since: None,
        });
        for (index, (addon, url, owner, generation)) in urls.into_iter().enumerate() {
            let bridge = self.clone();
            let ticket = ticket.clone();
            let request = next.id.clone();
            let media_type = context.session.type_.clone();
            self.request_streams_with_prefetch_cache(
                url,
                owner.clone(),
                generation,
                settings,
                Arc::new(|_| {}),
                move |result| {
                    let rows = result
                        .ok()
                        .and_then(|response| Addon::parse_streams(&response.bytes).ok())
                        .unwrap_or_default()
                        .into_iter()
                        .take(80)
                        .map(|stream| StreamUi {
                            id: nova_providers::stable_stream_identity(&owner, &request, &stream),
                            choice_id: Some(stream_choice_identity(
                                &owner,
                                &media_type,
                                &request,
                                &stream,
                            )),
                            owner_url: owner.clone(),
                            display: stream_display(&stream),
                            card: stream_card_text(&stream),
                            source: stream_source_for_addon(&stream, &owner, generation, settings),
                            addon: addon.clone(),
                            download: None,
                        })
                        .collect();
                    let _ = crate::app::profiles::invoke_from_event_loop(move || {
                        let mut state = bridge.shared.lock().unwrap();
                        if let Some(automatic) = state
                            .playback_end
                            .automatic
                            .as_mut()
                            .filter(|a| Arc::ptr_eq(&a.ticket, &ticket))
                        {
                            automatic.rows[index] = rows;
                            automatic.pending = automatic.pending.saturating_sub(1);
                        }
                        drop(state);
                        bridge.refresh_playback_end_menu();
                    });
                },
            );
        }
    }

    fn cancel_automatic_next(&self) {
        let mut state = self.shared.lock().unwrap();
        state.playback_end.automatic = None;
        state.playback_end.visible = true;
        state.playback_end.since = Some(Instant::now() - MENU_DELAY);
        drop(state);
        if let Some(app) = self.app() {
            app.set_auto_next_preview(false);
        }
        self.refresh_playback_end_menu();
    }

    pub(super) fn play_automatic_next(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let Some(context) = self.playback_end_context() else {
            return;
        };
        if !app.get_auto_next_preview()
            || !self.playback_end_eligible(&app)
            || !self.player.playback_readiness().ended
        {
            return;
        }
        let automatic = self.shared.lock().unwrap().playback_end.automatic.take();
        let Some(automatic) =
            automatic.filter(|automatic| self.automatic_valid(&context, automatic))
        else {
            return;
        };
        let Some(row) = automatic.candidate else {
            return;
        };
        if automatic_choice(
            std::slice::from_ref(&row),
            None,
            active_torrent_settings().enabled,
        )
        .is_none()
        {
            self.cancel_automatic_next();
            return;
        }
        let videos = self.playback_end_videos(&context);
        let matches = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .is_some_and(|m| m.id == context.session.title && m.type_ == context.session.type_);
        if !matches {
            self.open_preview(context.preview(), 0, false, None, false);
        }
        if !self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .is_some_and(|m| m.id == context.session.title && m.type_ == context.session.type_)
        {
            return;
        }
        self.note_player_progress_from_ui();
        app.invoke_close_player();
        self.clear_streams();
        // Re-enter the ordinary episode/stream path with a verified candidate.
        // No resolver URL, headers or torrent work is activated by discovery.
        {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state
                .modal_item
                .as_mut()
                .filter(|m| m.id == context.session.title && m.type_ == context.session.type_)
            else {
                return;
            };
            modal.seasons = ordered_seasons(&videos);
            modal.videos = videos;
            modal.request_id = automatic.next.id.clone();
            state.playback = Some(PlaybackTarget {
                series_id: context.session.title,
                episode_id: automatic.next.id.clone(),
                force_start_over: true,
                ..Default::default()
            });
            state.stream_all = vec![row.clone()];
            state.streams = vec![row];
            state.stream_filter = 0;
        }
        apply_stream_episode_context(&app, Some(&automatic.next));
        self.stream_picked(0);
    }

    pub(super) fn clear_playback_end_menu(&self) {
        self.shared.lock().unwrap().playback_end = State::default();
        if let Some(app) = self.app() {
            app.set_playback_end_menu_visible(false);
            app.set_auto_next_preview(false);
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
        if self.app().is_some_and(|app| app.get_auto_next_preview()) {
            self.cancel_automatic_next();
            return;
        }
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

    fn row(id: &str, title: &str, source: StreamSource) -> StreamUi {
        StreamUi {
            id: id.into(),
            choice_id: Some(id.into()),
            owner_url: "owner".into(),
            display: title.into(),
            card: StreamCardText {
                title: title.into(),
                ..Default::default()
            },
            source,
            addon: "addon".into(),
            download: None,
        }
    }

    #[test]
    fn automatic_choice_uses_release_family_then_stable_playable_ranking() {
        let previous = row(
            "old",
            "[Family] Show 01",
            StreamSource::Url("https://old".into()),
        );
        let preference = StreamPreference::from_row(&previous);
        let candidates = vec![
            row("unsupported", "unsupported", StreamSource::Unsupported),
            row(
                "first",
                "Other release",
                StreamSource::Url("https://first".into()),
            ),
            row(
                "family",
                "[Family] Show 02",
                StreamSource::Url("https://family".into()),
            ),
            row(
                "family-lower",
                "[Family] Show 02",
                StreamSource::Url("https://lower".into()),
            ),
        ];
        assert_eq!(
            automatic_choice(&candidates, Some(&preference), true)
                .unwrap()
                .id,
            "family"
        );
        assert_eq!(
            automatic_choice(&candidates, None, true).unwrap().id,
            "first"
        );
        let mut disabled = vec![row(
            "torrent",
            "first",
            StreamSource::Torrent {
                info_hash: "hash".into(),
                file_idx: None,
            },
        )];
        disabled.extend(candidates);
        assert_eq!(
            automatic_choice(&disabled, None, false).unwrap().id,
            "first"
        );
        assert_eq!(
            automatic_choice(&disabled, None, true).unwrap().id,
            "torrent"
        );
        assert!(
            automatic_choice(
                &[row("unsupported", "unsupported", StreamSource::Unsupported)],
                None,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn automatic_successor_requires_known_release_and_never_skips_unknown_or_future() {
        let videos: Vec<_> = (1..=3)
            .map(|episode| Video {
                id: episode.to_string(),
                season: Some(1),
                episode: Some(episode),
                released: Some("2020-01-01".into()),
                ..Default::default()
            })
            .collect();
        assert_eq!(automatic_successor(&videos, "1").unwrap().id, "2");
        for date in [None, Some("unknown".into()), Some("2099-01-01".into())] {
            let mut videos = videos.clone();
            videos[1].released = date;
            assert!(automatic_successor(&videos, "1").is_none());
        }
        assert!(automatic_successor(&videos, "3").is_none());
    }

    #[test]
    fn seek_replacement_and_foreground_loss_revoke_automatic_attempts() {
        let now = Instant::now();
        for (next_session, ended, eligible) in [
            (Some(session(1)), false, true),
            (Some(session(2)), true, true),
            (Some(session(1)), true, false),
            (None, false, false),
        ] {
            let mut state = State::default();
            state.observe(Some(session(1)), true, true, now);
            state.automatic_attempted = true;
            state.automatic = Some(Automatic {
                ticket: Arc::new(()),
                next: Video::default(),
                settings: 0,
                owners: vec![],
                rows: vec![],
                pending: 0,
                candidate: Some(row(
                    "candidate",
                    "release",
                    StreamSource::Url("https://example.test".into()),
                )),
                since: Some(now),
            });
            state.observe(next_session, ended, eligible, now + Duration::from_secs(3));
            assert!(state.automatic.is_none());
            assert!(!state.visible);
            if !eligible && ended {
                state.observe(Some(session(1)), true, true, now + Duration::from_secs(60));
                assert!(
                    !state.visible,
                    "returning foreground cannot revive that EOF"
                );
            }
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

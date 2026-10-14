//! In-app playback flow and episode progress tracking.
use super::*;

#[derive(Clone, Default)]
struct PlaybackContext {
    title: String,
    episode_label: String,
    osd_episode_title: String,
    episode_number: String,
    use_modal_poster: bool,
    media_type: String,
    media_id: String,
    request_id: String,
    is_episode: bool,
    is_movie: bool,
}

/// Numbering is supplied separately by the selector; punctuation in titles is opaque.
fn player_episode_header(number: &str, title: &str) -> String {
    let number = number.replace(' ', "");
    match (number.is_empty(), title.trim().is_empty()) {
        (false, false) => format!("{number}: {title}"),
        (false, true) => number,
        (true, false) => title.into(),
        (true, true) => String::new(),
    }
}

/// A UI intent outlives metadata discovery and the resume prompt, but cannot
/// cross a different modal/episode selection. Active playback survives browsing.
#[derive(Clone)]
pub(super) struct PlaybackIntent {
    token: crate::torrent::PlaybackRequest,
    modal_token: Option<Arc<()>>,
    request_id: String,
    pending: bool,
    in_app: bool,
    // Credential-bearing resolver links stay inside the in-app player, including
    // a delayed resume prompt. Never pass them through OS process arguments.
    private_playback: bool,
    context: PlaybackContext,
}

impl PlaybackIntent {
    fn accepts(&self, token: &crate::torrent::PlaybackRequest, modal: Option<&ModalItem>) -> bool {
        self.token.same_request(token)
            && token.is_current()
            && (!self.pending
                || match (&self.modal_token, modal) {
                    (Some(original), Some(modal)) => {
                        Arc::ptr_eq(original, &modal.open_token)
                            && self.request_id == modal.request_id
                    }
                    (None, None) => true,
                    _ => false,
                })
    }
}

/// Called directly by the Android listener on the Slint event-loop thread,
/// before the UI timer may stop. Clone under the bridge registry lock so the
/// persistence/UI work runs after that lock is released.
#[cfg(target_os = "android")]
pub(crate) fn note_android_pause_checkpoint() {
    let bridge = super::bridge::with_global_bridge(Clone::clone);
    if let Some(bridge) = bridge {
        bridge.cancel_pending_playback();
        bridge.note_player_progress_from_ui();
        bridge.persist_outgoing_playback();
    }
    persistence::flush_report();
}

impl Bridge {
    pub(super) fn take_player_screenshot(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_player_open() || !app.get_playback_started() || app.get_screenshot_busy() {
            return;
        }
        let session = self.player.screenshot_session();
        // These captions belong to active playback, even while Detail changes.
        let context = nova_player::ScreenshotContext {
            show_name: app.get_player_show_title().to_string(),
            episode_number: self
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .map(|intent| intent.context.episode_number.clone())
                .unwrap_or_default(),
        };
        app.invoke_osd_mouse_moved();
        app.set_screenshot_busy(true);
        app.set_screenshot_notice(SharedString::default());
        let bridge = self.clone();
        std::thread::spawn(move || {
            let result = bridge
                .player
                .capture_screenshot_with_context(session, context);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                if !app.get_player_open() || bridge.player.screenshot_session() != session {
                    return;
                }
                app.set_screenshot_busy(false);
                let notice = match result {
                    Ok(saved) => text::screenshot_saved(&saved.display_path),
                    Err(error) => text::tr(&error.to_string()).to_owned(),
                };
                app.set_screenshot_notice(notice.clone().into());
                app.invoke_osd_mouse_moved();
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_secs(5), move || {
                    if let Some(app) = weak.upgrade()
                        && bridge.player.screenshot_session() == session
                        && !app.get_screenshot_busy()
                        && app.get_screenshot_notice().as_str() == notice
                    {
                        app.set_screenshot_notice(SharedString::default());
                    }
                });
            });
        });
    }

    pub(super) fn begin_playback_request(&self) -> crate::torrent::PlaybackRequest {
        self.persist_outgoing_playback();
        self.finish_playback_request();
        let (episode_label, osd_episode_title, episode_number) = self
            .app()
            .map(|app| {
                (
                    app.get_episode_context().to_string(),
                    player_episode_header(
                        app.get_stream_episode_number().as_str(),
                        app.get_stream_episode_title().as_str(),
                    ),
                    app.get_stream_episode_number().to_string(),
                )
            })
            .unwrap_or_default();
        let token = crate::torrent::PlaybackRequest::default();
        let mut state = self.shared.lock().unwrap();
        state.playback_intent = Some(PlaybackIntent {
            token: token.clone(),
            modal_token: state
                .modal_item
                .as_ref()
                .map(|modal| modal.open_token.clone()),
            request_id: state
                .modal_item
                .as_ref()
                .map(|modal| modal.request_id.clone())
                .unwrap_or_default(),
            pending: true,
            in_app: false,
            private_playback: false,
            context: state
                .modal_item
                .as_ref()
                .map(|modal| PlaybackContext {
                    title: modal.name.clone(),
                    episode_label,
                    osd_episode_title,
                    episode_number,
                    use_modal_poster: true,
                    media_type: modal.type_.clone(),
                    media_id: modal.id.clone(),
                    request_id: modal.request_id.clone(),
                    is_episode: !modal.seasons.is_empty(),
                    is_movie: modal.type_ == "movie",
                })
                .unwrap_or_default(),
        });
        token
    }

    pub(super) fn playback_request_is_current(
        &self,
        token: &crate::torrent::PlaybackRequest,
    ) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .playback_intent
            .as_ref()
            .is_some_and(|intent| intent.accepts(token, state.modal_item.as_ref()))
    }

    pub(super) fn finish_playback_request(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(intent) = state.playback_intent.take() {
                intent.token.cancel();
            }
            state.pending_resume_stream = None;
            state.resume_prompt_choice = None;
            state.active_torrent = None;
        }
        if let Some(app) = self.app() {
            app.set_resume_prompt_visible(false);
        }
        if let Some(engine) = crate::torrent::engine() {
            engine.on_playback_stopped(&active_torrent_settings());
        }
    }

    pub(super) fn cancel_pending_playback(&self) {
        let pending = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .is_some_and(|intent| intent.pending);
        if pending {
            self.finish_playback_request();
        }
    }

    fn mark_playback_opened(&self, in_app: bool) {
        if let Some(intent) = self.shared.lock().unwrap().playback_intent.as_mut() {
            intent.pending = false;
            intent.in_app = in_app;
        }
    }

    pub(super) fn playback_preference_identity(
        &self,
        include_pending: bool,
    ) -> Option<(String, String)> {
        let state = self.shared.lock().unwrap();
        let intent = state.playback_intent.as_ref()?;
        if !intent.token.is_current() || (!include_pending && (intent.pending || !intent.in_app)) {
            return None;
        }
        let context = &intent.context;
        (!context.media_id.is_empty()
            && (context.is_episode
                || context.is_movie
                || matches!(context.media_type.as_str(), "series" | "movie")))
        .then(|| (context.media_type.clone(), context.media_id.clone()))
    }

    // Settings edits the synced global default; OSD edits are title overrides.
    /// Apply a global rate only to sessions that have not overridden speed.
    pub(super) fn apply_default_playback_speed(&self, speed: f32) {
        let speed = nova_config::round_playback_speed(speed);
        self.store_playback_speed(speed);
        self.player
            .update_playback_defaults(&nova_config::active_cache_settings());
        self.refresh_effective_speed();
    }

    /// Main thread: transient rate preview (press-and-hold 2× on the video).
    /// Pushes straight into the running session without touching the stored
    /// default/title value, the UI mirror or the debounced save — the release
    /// restores the stored rate through this same path.
    pub(super) fn preview_playback_speed(&self, speed: f32) {
        self.player.set_speed(speed);
    }

    /// Main thread: Android brightness swipe notch(es). Steps the session
    /// brightness and mirrors the level for the Slint readout; desktop never
    /// fires this (the Slint side gates swipes on `is_android`).
    pub(super) fn android_brightness_step(&self, _steps: i32) {
        #[cfg(target_os = "android")]
        {
            let level = android_player::player_brightness_step(_steps);
            if let Some(app) = self.app() {
                app.set_player_brightness(level);
            }
        }
    }

    /// Main thread: Android volume swipe notch (`±1`). Steps the music
    /// stream; the system volume panel is the readout.
    pub(super) fn android_volume_step(&self, _dir: i32) {
        #[cfg(target_os = "android")]
        android_player::player_volume_step(_dir);
    }

    /// Main thread: ± one step of the playback rate (± buttons / arrow keys).
    /// The step is taken on top of the current value and re-quantized onto the
    /// 0.05 grid, so the rate always lands on an exact multiple — even when
    /// the slider left a free value like 1.234 behind.
    pub(super) fn nudge_playback_speed(&self, d: i32) {
        let current = self
            .app()
            .map(|a| a.get_effective_playback_speed())
            .unwrap_or(1.0);
        let next = nova_config::quantize_playback_speed(
            current + d as f32 * nova_config::PLAYBACK_SPEED_STEP,
        );
        self.apply_playback_speed(next);
    }

    /// Remember `speed` in the in-memory settings and the runtime mirror the
    /// media/player crates read. No disk write: the caller persists.
    fn store_playback_speed(&self, speed: f32) {
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.playback_speed = speed;
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings);
        if let Some(app) = self.app() {
            app.set_playback_speed(speed);
        }
    }

    /// Persist the progress map, then repaint episode rows, season cards
    /// (watched check) and library badges. Uses the light `apply_*` path
    /// (no thumbnail re-queue) like the playback tracker does.
    pub(super) fn persist_and_refresh_progress(&self) {
        let map = self.shared.lock().unwrap().progress.clone();
        write_progress_map(&map);
        self.apply_episode_rows();
        self.apply_season_cards();
        self.refresh_library_progress_ui();
    }

    /// Repaint progress-derived surfaces outside the detail page: library
    /// badges/checks plus Home's Continue Watching, New episodes and Upcoming rows. Called
    /// after progress writes and whenever fresh episode meta lands (which
    /// may reveal new released episodes without starting a viewing run).
    /// Lightweight: no thumbnail re-queue.
    pub(super) fn refresh_library_progress_ui(&self) {
        self.migrate_viewing_activity();
        self.update_library_badges();
        self.refresh_detail_library_watched();
        self.rebuild_continue_list();
        self.rebuild_upcoming_list();
        self.apply_home_to_ui();
        self.dispatch_continue_posters();
        self.dispatch_upcoming_posters();
    }

    /// Resolve a torrent stream row to a playable loopback URL and open it in
    /// the player. Progress is surfaced in the detail modal's stream hint.
    pub(super) fn play_torrent(&self, display: &str, info_hash: String, file_idx: Option<u32>) {
        let settings = active_torrent_settings();
        if !settings.enabled {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "P2P streaming is disabled in Settings → P2P.",
            )));
            return;
        }
        let Some(engine) = crate::torrent::engine() else {
            self.set_stream_hint(Some(StreamHint::Fixed("P2P engine unavailable.")));
            return;
        };
        if !engine.is_ready() {
            self.set_stream_hint(Some(StreamHint::Fixed("P2P engine could not start.")));
            return;
        }
        // A new torrent replaces any still-playing one: stop it first so a
        // no-cache torrent's data is deleted and caching mode stops seeding
        // the previous one.
        let request = self.begin_playback_request();
        // The whole row text carries the release name (Torrentio puts it on a
        // later line, not the first), which helps pick the right file inside
        // the torrent.
        let requested_name = display.trim().to_string();
        self.set_stream_hint(Some(StreamHint::Fixed("Connecting to peers…")));
        let bridge = self.clone();
        let active_hash = info_hash.clone();
        engine.resolve(
            request.clone(),
            info_hash,
            file_idx,
            requested_name,
            settings,
            move |result| {
                let bridge = bridge.clone();
                let active_hash = active_hash.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    bridge.complete_torrent_resolution(request, active_hash, result);
                });
            },
        );
    }

    fn complete_torrent_resolution(
        &self,
        request: crate::torrent::PlaybackRequest,
        active_hash: String,
        result: Result<crate::torrent::StreamReady, String>,
    ) {
        if !self.playback_request_is_current(&request) {
            request.cancel();
            if let Some(engine) = crate::torrent::engine() {
                engine.on_playback_stopped(&active_torrent_settings());
            }
            return;
        }
        match result {
            Ok(ready) => {
                self.set_stream_hint(Some(StreamHint::Playing(ready.display_name)));
                if nova_config::active_cache_settings().player_external {
                    // User choice (Settings → Player): external player,
                    // always. Not tracked: there is no close event.
                    self.mark_playback_opened(false);
                    let _ = crate::player::open_external(&ready.url);
                } else if self.open_player_for_request(
                    request.clone(),
                    ready.url.clone(),
                    Vec::new(),
                    Vec::new(),
                ) {
                    // Remember which torrent the player is presenting
                    // so the 250 ms tick can report download stats
                    // while mpv buffers, and the close path can
                    // pause/delete it. The external fallback below has
                    // no close event, so it is not tracked here.
                    {
                        self.shared.lock().unwrap().active_torrent = Some(active_hash);
                    }
                } else {
                    // Built-in failed: external app as a fallback.
                    // Not tracked: there is no close event.
                    self.mark_playback_opened(false);
                    let _ = crate::player::open_external(&ready.url);
                }
            }
            Err(e) => self.set_stream_hint(Some(StreamHint::P2pUnavailable(e.to_string()))),
        }
    }

    /// Open the in-app mpv player for `url` (replacing any previous player
    /// window). Returns false when the player can't be created (e.g. mpv or
    /// the window can't be initialized), so callers can fall back to an
    /// external player (ACTION_VIEW on Android, xdg-open elsewhere). Only ever
    /// runs on the main/UI thread.
    pub(super) fn open_player(&self, url: String) -> bool {
        self.open_player_with_options(url, Vec::new(), Vec::new())
    }

    /// A bundled resolver runs entirely on a worker. The existing playback
    /// token protects metadata changes, Back, suspend and replacement picks;
    /// addon/settings revisions additionally protect configuration changes.
    pub(super) fn play_provider_resolver(
        &self,
        provider_url: String,
        token: String,
        settings_generation: u64,
        addon_generation: u64,
    ) {
        let request = self.begin_playback_request();
        let bridge = self.clone();
        let check_bridge = self.clone();
        let check_request = request.clone();
        let check_url = provider_url.clone();
        let cancelled: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            !check_bridge.playback_request_is_current(&check_request)
                || nova_providers::plugin_settings::settings_generation() != settings_generation
                || !check_bridge
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .any(|addon| {
                        addon.url == check_url
                            && addon.enabled
                            && addon.available
                            && addon.generation == addon_generation
                    })
        });
        if cancelled() {
            self.finish_playback_request();
            self.set_stream_hint(Some(StreamHint::Fixed(
                "This stream selection is out of date. Reload streams.",
            )));
            return;
        }
        self.set_stream_hint(Some(StreamHint::Fixed("Resolving stream…")));
        std::thread::spawn(move || {
            let result = nova_providers::resolve_provider_stream(
                &provider_url,
                &token,
                Duration::from_secs(30),
                cancelled.clone(),
            );
            let _ = slint::invoke_from_event_loop(move || {
                if cancelled() {
                    // Only finish our own pending intent: a newer selection
                    // may already be resolving or playing on this UI thread.
                    if bridge.playback_request_is_current(&request) {
                        bridge.finish_playback_request();
                        bridge.set_stream_hint(Some(StreamHint::Fixed(
                            "This stream selection is out of date. Reload streams.",
                        )));
                    }
                    return;
                }
                let private_playback = result.as_ref().is_ok_and(|stream| {
                    stream
                        .extra
                        .get("novaSensitivePlayback")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                });
                if let Some(intent) = bridge.shared.lock().unwrap().playback_intent.as_mut() {
                    intent.private_playback = private_playback;
                }
                match result.map(|stream| stream_source(&stream)) {
                    Ok(StreamSource::Url(url)) => {
                        bridge.set_stream_hint(None);
                        if !private_playback && nova_config::active_cache_settings().player_external
                        {
                            bridge.mark_playback_opened(false);
                            let _ = crate::player::open_external(&url);
                        } else if !bridge.open_requested_stream(
                            request,
                            url.clone(),
                            Vec::new(),
                            Vec::new(),
                        ) {
                            if private_playback {
                                bridge.finish_playback_request();
                                bridge.set_stream_hint(Some(StreamHint::Fixed(
                                    "This stream requires the in-app player, which is unavailable.",
                                )));
                            } else {
                                bridge.mark_playback_opened(false);
                                let _ = crate::player::open_external(&url);
                            }
                        }
                    }
                    Ok(StreamSource::UrlWithOptions {
                        url,
                        headers,
                        subtitles,
                    }) => {
                        if !bridge.open_requested_stream(request, url, headers, subtitles) {
                            bridge.finish_playback_request();
                            bridge.set_stream_hint(Some(StreamHint::Fixed(
                                "This stream requires the in-app player, which is unavailable.",
                            )));
                        } else {
                            bridge.set_stream_hint(None);
                        }
                    }
                    Err(error) => {
                        bridge.finish_playback_request();
                        bridge.set_stream_hint(Some(StreamHint::ResolverFailure(error.message)));
                    }
                    _ => {
                        bridge.finish_playback_request();
                        bridge.set_stream_hint(Some(StreamHint::Fixed("Could not resolve this stream. Try another result or check the addon configuration.")));
                    }
                }
            });
        });
    }

    pub(super) fn open_player_with_options(
        &self,
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    ) -> bool {
        let request = self.begin_playback_request();
        self.open_requested_stream(request, url, headers, subtitles)
    }

    pub(super) fn play_download(&self, id: &str) {
        let Some(job) = self
            .downloads
            .job(id)
            .filter(|job| job.phase == crate::download::DownloadPhase::Completed)
        else {
            return;
        };
        let Some(path) = job.artifact_path.as_ref().filter(|path| path.is_file()) else {
            return;
        };
        let request = self.begin_playback_request();
        let episode = read_episodes_cache_for(&job.media_type, &job.media_id)
            .and_then(|videos| videos.into_iter().find(|v| v.id == job.request_id));
        if let Some(intent) = self.shared.lock().unwrap().playback_intent.as_mut() {
            intent.context = PlaybackContext {
                title: job.title.clone(),
                episode_label: episode
                    .as_ref()
                    .map(stream_episode_context_label)
                    .unwrap_or_default(),
                episode_number: episode.as_ref().map(episode_se_label).unwrap_or_default(),
                osd_episode_title: episode
                    .as_ref()
                    .map(|video| {
                        let number = episode_se_label(video);
                        let title = episode_context_label(video);
                        player_episode_header(&number, if title == number { "" } else { &title })
                    })
                    .unwrap_or_default(),
                use_modal_poster: false,
                media_type: job.media_type.clone(),
                media_id: job.media_id.clone(),
                request_id: job.request_id.clone(),
                is_episode: job.media_type == "series",
                is_movie: job.media_type == "movie",
            };
        }
        self.open_requested_stream(
            request,
            path.to_string_lossy().into_owned(),
            Vec::new(),
            Vec::new(),
        );
    }

    fn open_requested_stream(
        &self,
        request: crate::torrent::PlaybackRequest,
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    ) -> bool {
        let path = std::path::Path::new(&url);
        if path.is_file()
            && let Some(engine) = crate::torrent::engine()
        {
            // Acquire the bundle on a worker before opening it. A resolving
            // network torrent can hold the operation lock for seconds.
            let bridge = self.clone();
            let callback_request = request.clone();
            let cleanup_engine = engine.clone();
            engine.pin_local_playback(request, path.to_path_buf(), move || {
                let abandoned = callback_request.clone();
                let abandoned_engine = cleanup_engine.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if bridge.playback_request_is_current(&callback_request) {
                        if !bridge.open_player_for_request(
                            callback_request,
                            url.clone(),
                            headers,
                            subtitles,
                        ) {
                            bridge.mark_playback_opened(false);
                            let _ = crate::player::open_external(&url);
                        }
                    } else {
                        callback_request.cancel();
                        cleanup_engine.on_playback_stopped(&active_torrent_settings());
                    }
                })
                .map_err(|_| {
                    abandoned.cancel();
                    abandoned_engine.on_playback_stopped(&active_torrent_settings());
                });
            });
            true
        } else {
            self.open_player_for_request(request, url, headers, subtitles)
        }
    }

    /// Save the last observation before another episode replaces its target.
    pub(super) fn persist_outgoing_playback(&self) {
        persistence::flush_report();
        self.flush_playback_preferences(true);
        let map = {
            let mut state = self.shared.lock().unwrap();
            let Some(target) = state.playback.clone() else {
                return;
            };
            if target.last_pos <= 0.0 && target.last_dur <= 0.0 {
                return;
            }
            let key = progress_map_key(&target.series_id, &target.episode_id);
            let entry = state
                .progress
                .entry(key)
                .or_insert_with(|| EpisodeProgress {
                    series_id: target.series_id.clone(),
                    episode_id: target.episode_id.clone(),
                    ..Default::default()
                });
            if entry.position_secs != target.last_pos || entry.duration_secs != target.last_dur {
                entry.position_secs = target.last_pos;
                entry.duration_secs = target.last_dur;
                entry.updated_at_secs = now_secs();
                if target.last_pos > 0.0 {
                    entry.unwatched_at_secs = 0;
                }
                if is_watched_position(target.last_pos, target.last_dur) {
                    entry.watched = true;
                }
                Some(state.progress.clone())
            } else {
                None
            }
        };
        if let Some(map) = map {
            write_progress_map(&map);
        }
        nova_sync::flush_local_sync();
    }

    fn open_player_for_request(
        &self,
        request: crate::torrent::PlaybackRequest,
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    ) -> bool {
        if !self.playback_request_is_current(&request) {
            return false;
        }
        self.clear_next_episode_prompt();
        let resume_choice = self.shared.lock().unwrap().resume_prompt_choice.take();
        // The selected source owns its context through async resolution and
        // the resume prompt; a Downloads page can launch without a detail modal.
        let context = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .map(|intent| intent.context.clone())
            .unwrap_or_default();
        let (title, series_id, request_id, is_episode, is_movie) = (
            context.title,
            context.media_id,
            context.request_id,
            context.is_episode,
            context.is_movie,
        );
        // Series flow: make sure a playback target exists (episode_picked
        // arms it, but library re-entry can reach here directly) and count
        // this opening. Movies arm a target too, keyed to the movie's own id,
        // so Continue Watching covers them. The entry is only staged in memory
        // here, never persisted: writing a zero-position record now would
        // pollute the mesh with a "fresh open" newer than a real resume
        // elsewhere. The first tick save (once frames flow) and
        // finalize-on-close persist it instead.
        if is_episode && !request_id.is_empty() {
            let mut state = self.shared.lock().unwrap();
            let key = progress_map_key(&series_id, &request_id);
            let history_pos = state
                .progress
                .get(&key)
                .filter(|p| resumable_position(p.position_secs, p.duration_secs, p.watched))
                .map(|p| p.position_secs);
            let behavior = state.cache_settings.episode_start_behavior;
            let resume_pos = match behavior {
                EpisodeStartBehavior::StartOver => None,
                EpisodeStartBehavior::Resume | EpisodeStartBehavior::Ask => history_pos,
            };
            let ask = behavior == EpisodeStartBehavior::Ask;
            let entry = state
                .progress
                .entry(key.clone())
                .or_insert_with(|| EpisodeProgress {
                    series_id: series_id.clone(),
                    episode_id: request_id.clone(),
                    ..Default::default()
                });
            entry.series_id = series_id.clone();
            entry.episode_id = request_id.clone();
            if resume_choice.is_none() {
                entry.play_count += 1;
            }
            let keep_resume = state
                .playback
                .as_ref()
                .filter(|t| t.series_id == series_id && t.episode_id == request_id)
                .and_then(|t| t.resume_pos);
            state.playback = Some(PlaybackTarget {
                series_id,
                episode_id: request_id,
                session: next_episode::new_session(),
                resume_pos: match (behavior, resume_choice) {
                    (EpisodeStartBehavior::StartOver, _) | (_, Some(false)) => None,
                    (_, Some(true)) => history_pos,
                    (_, None) => keep_resume.or(resume_pos),
                },
                ..Default::default()
            });
            if ask && history_pos.is_some() && resume_choice.is_none() {
                state.pending_resume_stream = Some(PendingStream {
                    request,
                    url,
                    headers,
                    subtitles,
                });
                if let Some(app) = self.app() {
                    app.set_resume_prompt_visible(true);
                }
                return true;
            }
        } else if is_movie && !series_id.is_empty() {
            // Movies: one target keyed to the item's own id (`request_id` is
            // the same id for a movie; fall back to `series_id`).
            let episode_id = if request_id.is_empty() {
                series_id.clone()
            } else {
                request_id
            };
            let mut state = self.shared.lock().unwrap();
            let key = progress_map_key(&series_id, &episode_id);
            let resume_pos = state
                .progress
                .get(&key)
                .filter(|p| resumable_position(p.position_secs, p.duration_secs, p.watched))
                .map(|p| p.position_secs);
            let entry = state
                .progress
                .entry(key)
                .or_insert_with(|| EpisodeProgress {
                    series_id: series_id.clone(),
                    episode_id: episode_id.clone(),
                    ..Default::default()
                });
            entry.series_id = series_id.clone();
            entry.episode_id = episode_id.clone();
            entry.play_count += 1;
            state.playback = Some(PlaybackTarget {
                series_id,
                episode_id,
                resume_pos,
                ..Default::default()
            });
        } else {
            self.shared.lock().unwrap().playback = None;
        }
        // Arming playback re-surfaces the item in Continue Watching right away:
        // drop any "removed" stamp now instead of waiting for the first
        // progress tick to outdate it (the `hidden_at >= updated_at` rule in
        // `rebuild_continue_list` only un-hides once progress actually moves).
        let armed = {
            let state = self.shared.lock().unwrap();
            state.playback.as_ref().map(|t| t.series_id.clone())
        };
        if let Some(id) = armed {
            let map = {
                let mut state = self.shared.lock().unwrap();
                if state.continue_hidden.remove(&id).is_some() {
                    Some(state.continue_hidden.clone())
                } else {
                    None
                }
            };
            if let Some(map) = map {
                write_continue_hidden(&map);
                self.rebuild_continue_list();
                self.apply_home_to_ui();
            }
        }
        // The start position rides into play() below (single backend session
        // opening at the resume point instead of play-from-0 plus a later
        // seek — each restart is slow on debrid backends). `resume_done` is
        // deliberately left false: the tick safety net confirms the engine
        // actually landed at the resume point (some backends ignore the
        // load-time start) and disarms it once confirmed.
        let start_at = {
            let state = self.shared.lock().unwrap();
            state
                .playback
                .as_ref()
                .and_then(|t| t.resume_pos)
                .unwrap_or(0.0)
        };
        if let Some(app) = self.app() {
            app.set_player_episode_title(context.osd_episode_title.clone().into());
            app.set_player_show_title(title.clone().into());
            app.set_screenshot_busy(false);
            app.set_screenshot_notice(SharedString::default());
        }
        let ep = (!context.episode_label.is_empty()).then_some(context.episode_label);
        let display_title = match ep {
            Some(ep) => format!("{title} · {ep}"),
            None => title,
        };
        if let Some(app) = self.app() {
            app.set_player_title(slint::SharedString::from(&display_title));
            // Loading-screen background: prefer an episode thumbnail (set
            // when the episode was picked); fall back to the show poster
            // from the details modal. Movies (no episode pick) always land
            // here.
            if !context.use_modal_poster {
                app.set_player_poster(Image::default());
            } else if app.get_player_poster().size().width == 0 {
                app.set_player_poster(app.get_selected_poster());
            }
        }

        let preferences = self.prepare_playback_preferences();
        let allow_external = !self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .is_some_and(|intent| intent.private_playback);
        match self.player.play_with_preferences_and_policy(
            &url,
            start_at,
            &headers,
            &subtitles,
            preferences,
            allow_external,
        ) {
            Ok(()) => {
                self.mark_playback_opened(true);
                true
            }
            Err(e) => {
                eprintln!("nova: player unavailable: {e}");
                false
            }
        }
    }

    pub(super) fn resume_prompt_choice(&self, resume: bool) {
        let pending_stream = {
            let mut state = self.shared.lock().unwrap();
            state.pending_resume_stream.take()
        };
        if let Some(app) = self.app() {
            app.set_resume_prompt_visible(false);
        }
        if let Some(stream) = pending_stream
            && self.playback_request_is_current(&stream.request)
        {
            self.shared.lock().unwrap().resume_prompt_choice = Some(resume);
            let _ = self.open_player_for_request(
                stream.request,
                stream.url,
                stream.headers,
                stream.subtitles,
            );
        }
    }

    pub(super) fn cancel_resume_prompt(&self) {
        self.cancel_pending_playback();
        self.shared.lock().unwrap().pending_resume_stream = None;
        if let Some(app) = self.app() {
            app.set_resume_prompt_visible(false);
        }
    }

    /// While a torrent-backed stream is still buffering, mirror the engine's
    /// live download stats into the player status line. Called right after the
    /// 250 ms player tick; a no-op once playback starts, for direct streams,
    /// and on the web build.
    pub(super) fn note_torrent_progress_from_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        if !app.get_player_open() || app.get_playback_started() {
            return;
        }
        let hash = match self.shared.lock().unwrap().active_torrent.clone() {
            Some(h) => h,
            None => return,
        };
        let Some(engine) = crate::torrent::engine() else {
            return;
        };
        let stats = engine.stats(&hash);
        // Before any peer connects there is nothing useful to show; keep the
        // player's own "Loading stream…" line instead of "↓ 0 B/s · 0 peers".
        if stats.peers == 0 && stats.down_bps == 0 && stats.progress <= 0.0 {
            return;
        }
        app.set_player_status(SharedString::from(&format!(
            "{} ↓ {}/s · {} · {:.0}%",
            text::tr("Connecting…"),
            format_rate(stats.down_bps),
            text::peers(stats.peers),
            stats.progress * 100.0,
        )));
    }

    /// Poll the player overlay props (called right after the 250 ms player
    /// tick on the UI thread): issue a pending resume seek, throttle-save
    /// progress, and finalize history when the player just closed. Reading
    /// the mirrored Slint props keeps this identical on desktop (mpv) and
    /// web (HTML5 video) with no player-module changes.
    pub(super) fn note_player_progress_from_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        self.refresh_next_episode_prompt();
        if self.player.take_external_handoff() {
            self.mark_playback_opened(false);
        }
        let player_open = app.get_player_open();
        let closed = !player_open
            && self
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .is_some_and(|intent| intent.in_app);
        if closed {
            self.finish_playback_request();
        }
        let pos = app.get_position() as f64;
        let dur = app.get_duration() as f64;

        // Snapshot the target without holding the lock across player calls.
        let target = self.shared.lock().unwrap().playback.clone();
        let Some(mut target) = target else {
            return;
        };

        if player_open {
            // Safety net for the resume point: `play()` opens at the saved
            // position in a single backend session, but an engine that ignores
            // the load-time start would silently begin at 0. Once frames flow,
            // confirm we landed there and otherwise seek once. Disarm either
            // way so a later user scrub is never yanked back.
            if let Some(resume) = target.resume_pos {
                let step = resume_step(
                    resume,
                    pos,
                    dur,
                    app.get_playback_started(),
                    target.resume_done,
                );
                if step != ResumeStep::Idle {
                    target.resume_done = true;
                    {
                        let mut state = self.shared.lock().unwrap();
                        if let Some(t) = state.playback.as_mut()
                            && t.series_id == target.series_id
                            && t.episode_id == target.episode_id
                        {
                            t.resume_done = true;
                        }
                    }
                    if step == ResumeStep::Seek {
                        // Move the thumb with the seek: `Player::seek` arms the
                        // scrub guard, so the tick keeps the thumb at the resume
                        // point until mpv applies it.
                        app.set_position(resume as f32);
                        self.player.seek(resume as f32);
                    }
                }
            }
            if !app.get_playback_started() || (dur <= 0.0 && pos <= 0.0) {
                return; // frames not flowing yet: nothing to persist
            }
            target.last_pos = pos;
            target.last_dur = dur;
            let watched_now = is_watched_position(pos, dur);
            let already_watched = self
                .shared
                .lock()
                .unwrap()
                .progress
                .get(&progress_map_key(&target.series_id, &target.episode_id))
                .map(|p| p.watched)
                .unwrap_or(false);
            let now = now_secs();
            // While paused the position never advances: skip the time-based
            // save so a paused player doesn't rewrite the identical position
            // (and churn the synced record) every few seconds. Delta and
            // watched-transition saves still fire, and closing finalizes.
            let paused = app.get_is_paused();
            let due_by_time =
                !paused && now.saturating_sub(target.last_saved_at) >= PROGRESS_SAVE_INTERVAL_SECS;
            let due_by_delta = (pos - target.last_saved_pos).abs() >= PROGRESS_MIN_DELTA_SECS;
            let newly_watched = watched_now && !already_watched;
            let pause_save = paused && (pos - target.last_saved_pos).abs() > 0.001;
            if !(due_by_time || due_by_delta || newly_watched || pause_save) {
                if paused {
                    nova_sync::flush_local_sync();
                }
                // Still remember the latest observation for finalize.
                let mut state = self.shared.lock().unwrap();
                if let Some(t) = state.playback.as_mut()
                    && t.series_id == target.series_id
                    && t.episode_id == target.episode_id
                {
                    t.last_pos = pos;
                    t.last_dur = dur;
                }
                return;
            }
            target.last_saved_at = now;
            target.last_saved_pos = pos;
            let key = progress_map_key(&target.series_id, &target.episode_id);
            let ui_refresh = newly_watched;
            {
                let mut state = self.shared.lock().unwrap();
                if let Some(t) = state.playback.as_mut()
                    && t.series_id == target.series_id
                    && t.episode_id == target.episode_id
                {
                    *t = target.clone();
                }
                let entry = state
                    .progress
                    .entry(key.clone())
                    .or_insert_with(|| EpisodeProgress {
                        series_id: target.series_id.clone(),
                        episode_id: target.episode_id.clone(),
                        ..Default::default()
                    });
                entry.position_secs = pos;
                entry.duration_secs = dur;
                entry.updated_at_secs = now;
                if watched_now {
                    entry.watched = true;
                    // Watching to the end supersedes an earlier unwatch, so
                    // the mesh merge reads the fresh intent, not the stale one.
                    entry.unwatched_at_secs = 0;
                } else if pos > 0.0 {
                    // Any observed playback supersedes a prior unwatch: the
                    // unwatch signal means "no position", so a position with
                    // a stale intent would misread as one.
                    entry.unwatched_at_secs = 0;
                }
                let map = state.progress.clone();
                drop(state);
                if paused || newly_watched {
                    write_progress_map(&map);
                    nova_sync::flush_local_sync();
                } else {
                    nova_sync::playback_checkpoint(|| write_progress_map(&map));
                }
            }
            if newly_watched {
                self.auto_delete_watched_downloads(&target.series_id, &[target.episode_id.clone()]);
            }
            if ui_refresh {
                self.refresh_progress_ui();
            }
        } else {
            // Player just closed: finalize with the last observed values
            // (the close itself resets the UI props to 0). A close near the
            // end counts as watched the same as a natural EOF.
            let (saw_frames, watched_now) = (target.last_dur > 0.0 || target.last_pos > 0.0, {
                is_watched_position(target.last_pos, target.last_dur)
            });
            {
                let mut state = self.shared.lock().unwrap();
                let still_mine = state
                    .playback
                    .as_ref()
                    .map(|t| t.series_id == target.series_id && t.episode_id == target.episode_id)
                    .unwrap_or(false);
                if still_mine {
                    state.playback = None;
                } else {
                    return;
                }
                if saw_frames {
                    let key = progress_map_key(&target.series_id, &target.episode_id);
                    let entry = state
                        .progress
                        .entry(key)
                        .or_insert_with(|| EpisodeProgress {
                            series_id: target.series_id.clone(),
                            episode_id: target.episode_id.clone(),
                            ..Default::default()
                        });
                    entry.position_secs = target.last_pos;
                    entry.duration_secs = target.last_dur;
                    entry.updated_at_secs = now_secs();
                    if watched_now {
                        entry.watched = true;
                        entry.unwatched_at_secs = 0;
                    } else if target.last_pos > 0.0 {
                        entry.unwatched_at_secs = 0;
                    }
                    let map = state.progress.clone();
                    drop(state);
                    write_progress_map(&map);
                }
            }
            nova_sync::flush_local_sync();
            if saw_frames && watched_now {
                self.auto_delete_watched_downloads(&target.series_id, &[target.episode_id.clone()]);
            }
            // Closing reveals Home again without a navigation callback. Rebuild
            // its derived episode cards after finalizing the saved position.
            self.apply_episode_rows();
            self.apply_season_cards();
            self.refresh_library_progress_ui();
        }
    }

    /// Re-render episode rows (watched/progress) and library badges after
    /// tracking updates. Episode rows use the light path (no thumbnail
    /// re-queue); library badges update in place so posters keep loading.
    pub(super) fn refresh_progress_ui(&self) {
        self.apply_episode_rows();
        self.update_library_badges();
    }
}

/// Decision for one live playback tick about the saved resume point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResumeStep {
    /// Nothing to do: no frames yet, unknown duration, or already handled.
    Idle,
    /// The engine reached the resume point: disarm the safety seek so a later
    /// user scrub is never pulled back to it.
    Landed,
    /// The engine opened elsewhere (ignored the load-time start): issue the
    /// safety seek now.
    Seek,
}

/// Pure resume decision used by [`Bridge::note_player_progress_from_ui`]:
/// `resume` is the saved position, `pos`/`dur` the live engine values,
/// `started` whether frames are flowing, and `done` whether the resume point
/// was already reached/handled. Within 5 s counts as landed (the same slack
/// the tick uses elsewhere).
pub(crate) fn resume_step(
    resume: f64,
    pos: f64,
    dur: f64,
    started: bool,
    done: bool,
) -> ResumeStep {
    if done || dur <= 0.0 || !started {
        return ResumeStep::Idle;
    }
    if (resume - pos).abs() <= 5.0 {
        ResumeStep::Landed
    } else {
        ResumeStep::Seek
    }
}

/// Read the whole progress map (empty when absent/corrupt).
pub(crate) fn read_progress_map() -> HashMap<String, EpisodeProgress> {
    persistence::pending_progress().unwrap_or_else(|| {
        read_json::<HashMap<String, EpisodeProgress>>(EPISODE_PROGRESS_KEY).unwrap_or_default()
    })
}
/// Queue a progress snapshot; storage writes only changed episode records.
pub(crate) fn write_progress_map(map: &HashMap<String, EpisodeProgress>) {
    persistence::write_progress(map);
}
/// App-wide torrent settings (mirrored in the settings page). Persisted in
/// the same KV store as the image-cache settings, so it works on Android too.
/// The web build has no torrent support, so it always reads the disabled
/// default.
pub(crate) fn read_torrent_settings() -> TorrentSettings {
    read_json::<TorrentSettings>(TORRENT_SETTINGS_KEY).unwrap_or_default()
}
pub(crate) fn write_torrent_settings(settings: &TorrentSettings) {
    write_json(TORRENT_SETTINGS_KEY, settings);
    *CURRENT_TORRENT_SETTINGS.lock().unwrap() = settings.clone();
}
pub(crate) fn active_torrent_settings() -> TorrentSettings {
    CURRENT_TORRENT_SETTINGS.lock().unwrap().clone()
}
#[cfg(feature = "desktop")]
impl Bridge {
    /// Create the session on a worker after Home's first completed render.
    pub(super) fn start_torrent_after_first_frame(&self) {
        let settings = active_torrent_settings();
        if !settings.enabled || crate::torrent::engine().is_some() {
            return;
        }
        let dir = torrent_cache_dir();
        let b = self.clone();
        thread::spawn(move || {
            let engine = crate::torrent::TorrentEngine::setup(dir.clone(), &settings);
            let _ = slint::invoke_from_event_loop(move || {
                let current = active_torrent_settings();
                if b.app().is_some()
                    && current.enabled
                    && torrent_cache_dir() == dir
                    && crate::torrent::engine().is_none()
                {
                    // Setup already used the startup settings. Live edits
                    // start/update their own engine, which the guard above keeps.
                    // Reapplying here could purge a no-cache session on the UI thread.
                    engine.restore_downloads(b.downloads.torrent_owners());
                    crate::torrent::install(engine);
                    b.downloads.start();
                    b.refresh_torrent_disk_usage();
                } else {
                    thread::spawn(move || drop(engine));
                }
            });
        });
    }
}

/// Default folder the torrent session downloads into.
pub(crate) fn torrent_cache_dir() -> PathBuf {
    let s = active_torrent_settings();
    if s.dir.trim().is_empty() {
        app_cache_dir().join("torrents")
    } else {
        PathBuf::from(s.dir.trim())
    }
}
/// Bring the process-wide torrent engine in line with `settings`: start it on
/// first use when torrent playback is enabled, push live settings into an
/// already-running engine, and tear it down when disabled. Keeping this the
/// only start path means a disabled setting never spawns the session, its
/// sockets or its tokio runtime.
pub(crate) fn sync_torrent_engine(settings: &TorrentSettings) {
    if !settings.enabled {
        crate::torrent::uninstall();
        return;
    }
    match crate::torrent::engine() {
        Some(engine) => engine.apply_settings(settings),
        None => {
            let engine = crate::torrent::TorrentEngine::setup(torrent_cache_dir(), settings);
            engine.restore_downloads(downloads::persisted_torrent_owners());
            crate::torrent::install(engine);
        }
    }
}

#[cfg(all(test, feature = "desktop"))]
mod home_return_tests {
    use super::*;

    #[test]
    fn closing_playback_refreshes_home_without_navigation() {
        const ROOT: &str = "NOVA_HOME_PLAYBACK_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-playback-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::playback::home_return_tests::closing_playback_refreshes_home_without_navigation", "--nocapture"])
                .output().unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        app.set_show_home(true);
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        // A resolver callback is injected here so no network or media backend
        // is needed to prove stale success/error leave the UI and target alone.
        let old = bridge.begin_playback_request();
        let current = bridge.begin_playback_request();
        assert!(!old.is_current());
        bridge.complete_torrent_resolution(
            old.clone(),
            "old".into(),
            Ok(crate::torrent::StreamReady {
                url: "http://127.0.0.1/stale".into(),
                display_name: "stale".into(),
            }),
        );
        bridge.complete_torrent_resolution(old, "old".into(), Err("stale failure".into()));
        assert!(!app.get_player_open());
        assert!(bridge.shared.lock().unwrap().active_torrent.is_none());
        assert!(bridge.playback_request_is_current(&current));
        // A pending resolver must not be mistaken for a closed player.
        bridge.note_player_progress_from_ui();
        assert!(current.is_current());
        bridge.stream_selector_back();
        assert!(!current.is_current(), "movie Back cancels pending work");
        let pending = bridge.begin_playback_request();
        bridge.cancel_resume_prompt();
        assert!(!pending.is_current());
        let pending = bridge.begin_playback_request();
        bridge.modal_closed();
        assert!(!pending.is_current());
        let playing = bridge.begin_playback_request();
        bridge.mark_playback_opened(true);
        bridge.clear_streams();
        assert!(playing.is_current(), "browsing keeps active playback");
        bridge.note_player_progress_from_ui();
        assert!(
            !playing.is_current(),
            "natural close works without a progress target"
        );

        let external = bridge.begin_playback_request();
        bridge.mark_playback_opened(false);
        bridge.note_player_progress_from_ui();
        assert!(
            external.is_current(),
            "external handoff keeps source ownership"
        );
        bridge.finish_playback_request();

        let videos = (1..=2)
            .map(|number| Video {
                id: format!("episode-{number}"),
                name: format!("Episode {number}"),
                season: Some(1),
                episode: Some(number),
                released: Some("2000-01-01T00:00:00.000Z".into()),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        super::super::detail::write_episodes_cache_for("series", "series", &videos);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![
                serde_json::from_value(
                    serde_json::json!({"id":"series", "type_":"series", "name":"Series"}),
                )
                .unwrap(),
            ];
            state.progress.insert(
                progress_map_key("series", "episode-1"),
                EpisodeProgress {
                    series_id: "series".into(),
                    episode_id: "episode-1".into(),
                    position_secs: 300.0,
                    duration_secs: 1000.0,
                    updated_at_secs: 1,
                    ..Default::default()
                },
            );
        }
        bridge.persist_library();
        bridge.rebuild_continue_list();
        bridge.apply_home_to_ui();
        let old = app.get_home_continue().row_data(0).unwrap();
        assert_eq!(
            bridge.shared.lock().unwrap().continue_list[0].episode_id,
            "episode-1"
        );
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-1".into(),
            last_pos: 950.0,
            last_dur: 1000.0,
            ..Default::default()
        });
        app.set_player_open(false);
        bridge.note_player_progress_from_ui();
        assert!(app.get_show_home());
        assert_eq!(
            bridge.shared.lock().unwrap().continue_list[0].episode_id,
            "episode-2"
        );
        assert_ne!(
            app.get_home_continue().row_data(0).unwrap().episode_title,
            old.episode_title
        );
        // An interrupted next episode must also repaint its resume progress.
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-2".into(),
            last_pos: 500.0,
            last_dur: 1000.0,
            ..Default::default()
        });
        bridge.note_player_progress_from_ui();
        assert!((app.get_home_continue().row_data(0).unwrap().progress - 0.5).abs() < 0.001);

        // Complete the known show, announce a future episode, then release it.
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-2".into(),
            last_pos: 1000.0,
            last_dur: 1000.0,
            ..Default::default()
        });
        bridge.note_player_progress_from_ui();
        assert_eq!(
            read_viewing_activity()["series"].phase,
            ViewingPhase::AwaitingReturn
        );
        assert_eq!(app.get_home_continue().row_count(), 0);
        let mut refreshed = videos.clone();
        refreshed.push(Video {
            id: "episode-3".into(),
            name: "New episode".into(),
            season: Some(1),
            episode: Some(3),
            released: Some("2999-01-01".into()),
            ..Default::default()
        });
        write_episodes_cache_for("series", "series", &refreshed);
        bridge.refresh_library_progress_ui();
        assert_eq!(app.get_library().row_data(0).unwrap().status, "Completed");
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        assert_eq!(app.get_home_upcoming().row_count(), 1);
        refreshed[2].released = Some("2000-01-03".into());
        write_episodes_cache_for("series", "series", &refreshed);
        bridge.refresh_library_progress_ui();
        assert_eq!(
            app.get_library().row_data(0).unwrap().status,
            "New episodes"
        );
        assert_eq!(app.get_home_continue().row_count(), 0);
        assert_eq!(app.get_home_new_episodes().row_count(), 1);
        assert_eq!(
            app.get_home_new_episodes().row_data(0).unwrap().remaining,
            "1 new episode"
        );

        // Removing an available batch stays separate from Continue hides.
        bridge.new_episode_remove(0);
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        refreshed.push(Video {
            id: "episode-4".into(),
            season: Some(1),
            episode: Some(4),
            released: Some("2000-01-04".into()),
            ..Default::default()
        });
        write_episodes_cache_for("series", "series", &refreshed);
        bridge.refresh_library_progress_ui();
        assert_eq!(app.get_home_new_episodes().row_count(), 1);
        assert_eq!(
            bridge.shared.lock().unwrap().new_episodes_list[0].episode_id,
            "episode-3"
        );

        // A short attempt cannot reactivate viewing; meaningful playback can.
        for (position, expected) in [(3.0, "New episodes"), (100.0, "Watching")] {
            bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
                series_id: "series".into(),
                episode_id: "episode-3".into(),
                last_pos: position,
                last_dur: 1000.0,
                ..Default::default()
            });
            bridge.note_player_progress_from_ui();
            assert_eq!(app.get_library().row_data(0).unwrap().status, expected);
        }
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        assert_eq!(app.get_home_continue().row_count(), 1);
        let owner = nova_sync::local_store().unwrap();
        let basis = owner
            .lock()
            .unwrap()
            .digest()
            .get(DOMAIN_VIEWING_ACTIVITY)
            .cloned();
        bridge.sync_apply(vec![DOMAIN_PROGRESS.into(), DOMAIN_VIEWING_ACTIVITY.into()]);
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .digest()
                .get(DOMAIN_VIEWING_ACTIVITY)
                .cloned(),
            basis,
            "applying remote activity must not echo it"
        );
        assert_eq!(
            read_viewing_activity()["series"].phase,
            ViewingPhase::Active
        );
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-3".into(),
            last_pos: 1000.0,
            last_dur: 1000.0,
            ..Default::default()
        });
        bridge.note_player_progress_from_ui();
        assert_eq!(app.get_library().row_data(0).unwrap().activity_action, 3);
        let before = read_progress_map();
        bridge.library_status_action(0, 3);
        assert_eq!(
            app.get_library().row_data(0).unwrap().status,
            "New episodes"
        );
        assert_eq!(app.get_library().row_data(0).unwrap().activity_action, 4);
        bridge.library_status_action(0, 4);
        assert_eq!(app.get_library().row_data(0).unwrap().status, "Watching");
        assert_eq!(
            serde_json::to_value(read_progress_map()).unwrap(),
            serde_json::to_value(before).unwrap(),
            "manual viewing intent must not modify watched history"
        );

        // Pausing between checkpoints saves the latest position once. Timer
        // ticks at the stationary pause point leave both snapshot and HLC alone.
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_is_paused(true);
        app.set_position(123.0);
        app.set_duration(1000.0);
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-3".into(),
            last_saved_pos: 100.0,
            last_saved_at: now_secs(),
            ..Default::default()
        });
        bridge.note_player_progress_from_ui();
        assert_eq!(
            read_progress_map()[&progress_map_key("series", "episode-3")].position_secs,
            123.0
        );
        let paused_snapshot = storage::get_str(EPISODE_PROGRESS_KEY).unwrap();
        let paused_digest = owner.lock().unwrap().digest();
        bridge.note_player_progress_from_ui();
        assert_eq!(
            storage::get_str(EPISODE_PROGRESS_KEY).unwrap(),
            paused_snapshot
        );
        assert_eq!(owner.lock().unwrap().digest(), paused_digest);

        // Replacement saves the observation made since the last checkpoint.
        bridge
            .shared
            .lock()
            .unwrap()
            .playback
            .as_mut()
            .unwrap()
            .last_pos = 145.0;
        bridge.persist_outgoing_playback();
        assert_eq!(
            read_progress_map()[&progress_map_key("series", "episode-3")].position_secs,
            145.0
        );
        let outgoing_digest = owner.lock().unwrap().digest();
        bridge.persist_outgoing_playback();
        assert_eq!(owner.lock().unwrap().digest(), outgoing_digest);
        app.set_player_open(false);

        let snapshot = storage::get_str(VIEWING_ACTIVITY_KEY).unwrap();
        {
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN_VIEWING_ACTIVITY,
                "series",
                Some("{invalid".into()),
                0,
                nova_sync::local_device().unwrap(),
            );
            store.save().unwrap();
        }
        bridge.sync_apply(vec![DOMAIN_VIEWING_ACTIVITY.into()]);
        assert_eq!(
            storage::get_str(VIEWING_ACTIVITY_KEY).unwrap(),
            snapshot,
            "malformed remote activity must retain the local snapshot"
        );
        assert!(!writable_key(VIEWING_ACTIVITY_KEY));
    }
}

#[cfg(all(test, feature = "desktop"))]
mod preference_tests {
    use super::*;
    use nova_config::playback_preferences::{PreferenceValue, TrackChoice, preference_key};

    #[test]
    fn title_speed_persists_without_editing_defaults_and_remote_changes_wait() {
        const ROOT: &str = "NOVA_PLAYBACK_PREFERENCES_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-preferences-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::playback::preference_tests::title_speed_persists_without_editing_defaults_and_remote_changes_wait", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.playback_speed = 1.15;
            state.cache_settings.default_audio_language = "ja".into();
            state.cache_settings.default_subtitle_language = "en".into();
            state.playback_intent = Some(PlaybackIntent {
                token: Default::default(),
                modal_token: None,
                request_id: "episode-one".into(),
                pending: false,
                in_app: true,
                private_playback: false,
                context: PlaybackContext {
                    media_type: "series".into(),
                    media_id: "title".into(),
                    ..Default::default()
                },
            });
        }
        bridge.settings_to_ui();
        bridge.apply_playback_speed(1.4);
        bridge.flush_playback_preferences(true);
        assert_eq!(
            app.get_playback_speed(),
            1.15,
            "OSD must not edit the default"
        );
        assert_eq!(app.get_effective_playback_speed(), 1.4);
        assert_eq!(
            bridge.shared.lock().unwrap().cache_settings.playback_speed,
            1.15
        );
        let prefs = bridge.prepare_playback_preferences();
        assert_eq!(prefs.title.speed, Some(1.4));
        assert!(prefs.title.audio.is_none());
        assert_eq!(prefs.audio_language, "ja");
        bridge.preview_playback_speed(2.0);
        assert_eq!(bridge.prepare_playback_preferences().title.speed, Some(1.4));
        assert_eq!(app.get_effective_playback_speed(), 1.4);
        let owner = nova_sync::local_store().unwrap();
        let speed_key = preference_key("series", "title", 2);
        let sub_key = preference_key("series", "title", 1);
        {
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN_PLAYBACK_PREFERENCES,
                &speed_key,
                Some(serde_json::to_string(&PreferenceValue::Speed(1.8)).unwrap()),
                0,
                999,
            );
            store.set(
                DOMAIN_PLAYBACK_PREFERENCES,
                &sub_key,
                Some(serde_json::to_string(&PreferenceValue::Subtitles(TrackChoice::Off)).unwrap()),
                0,
                999,
            );
            store.save().unwrap();
        }
        bridge.sync_apply(vec![DOMAIN_PLAYBACK_PREFERENCES.into()]);
        assert_eq!(
            app.get_effective_playback_speed(),
            1.4,
            "remote edits must not interrupt playback"
        );
        let next = bridge.prepare_playback_preferences();
        assert_eq!(next.title.speed, Some(1.8));
        assert_eq!(next.title.subtitles, Some(TrackChoice::Off));
        bridge.restore_playback_default(2);
        assert_eq!(app.get_effective_playback_speed(), 1.15);
        let next = bridge.prepare_playback_preferences();
        assert!(next.title.speed.is_none());
        assert_eq!(next.title.subtitles, Some(TrackChoice::Off));
        assert!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_PLAYBACK_PREFERENCES, &speed_key)
                .unwrap()
                .value
                .is_none()
        );
        assert!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_PLAYBACK_PREFERENCES, &sub_key)
                .unwrap()
                .value
                .is_some()
        );
        bridge.apply_playback_speed(1.6);
        // Replacing/closing flushes the pending slider change even before its debounce.
        bridge.persist_outgoing_playback();
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context
            .request_id = "episode-two".into();
        assert_eq!(bridge.prepare_playback_preferences().title.speed, Some(1.6));
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context
            .media_type = "movie".into();
        assert!(bridge.prepare_playback_preferences().title.speed.is_none());
        assert_eq!(bridge.prepare_playback_preferences().speed(), 1.15);
        assert_eq!(
            read_json::<HashMap<String, PreferenceValue>>(playback_preferences::PREFERENCES_KEY)
                .unwrap()
                .get(&speed_key),
            Some(&PreferenceValue::Speed(1.6))
        );
        // Upgrade a legacy settings snapshot whose baseline excluded speed.
        {
            let mut store = owner.lock().unwrap();
            store.queue_extra("settings", Some(r#"{"playback_speed":1.15}"#.into()));
            store.queue_extra("sync:baseline:settings", Some("{}".into()));
            store.save().unwrap();
        }
        bridge.seed_playback_default_settings();
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .record(DOMAIN_SETTINGS, "playback_speed")
                .unwrap()
                .value
                .as_deref(),
            Some("1.15")
        );
        {
            let mut store = owner.lock().unwrap();
            store.set(
                DOMAIN_SETTINGS,
                "playback_speed",
                Some("1.9".into()),
                0,
                999,
            );
            store.save().unwrap();
        }
        bridge.seed_playback_default_settings();
        assert_eq!(
            bridge.shared.lock().unwrap().cache_settings.playback_speed,
            1.9,
            "migration must adopt an existing shared speed"
        );
        // An older app can have acknowledged an unknown domain without its KV
        // snapshot. Upgrading must import those records even without a replay.
        {
            let mut store = owner.lock().unwrap();
            store.queue_extra(playback_preferences::PREFERENCES_KEY, None);
            store.queue_extra("sync:baseline:playback_preferences", None);
            store.save().unwrap();
        }
        bridge.restore_playback_preferences_schema();
        assert_eq!(
            read_json::<HashMap<String, PreferenceValue>>(playback_preferences::PREFERENCES_KEY)
                .unwrap()
                .get(&speed_key),
            Some(&PreferenceValue::Speed(1.6))
        );
    }
}

#[cfg(test)]
mod header_tests {
    use super::player_episode_header;

    #[test]
    fn separate_number_and_title_preserve_title_punctuation() {
        assert_eq!(
            player_episode_header("S1 E1", "A title: with · punctuation"),
            "S1E1: A title: with · punctuation"
        );
        assert_eq!(player_episode_header("E3", ""), "E3");
        assert_eq!(player_episode_header("", "Special"), "Special");
        assert_eq!(player_episode_header("", ""), "");
    }
}

//! In-app playback flow and episode progress tracking.
use super::*;

impl Bridge {
    // ---- Playback rate (Settings → Player + the player's settings panel) ---
    //
    // One per-device value (`CacheSettings::playback_speed`) edits the same
    // number from both UIs. It is applied to the running mpv session live and
    // written to the KV store on a short debounce, so a slider drag persists
    // once instead of per pointer move.

    /// Main thread: a new playback rate came from a slider. Clamped and
    /// rounded to hundredths, stored per device, applied to the running
    /// session, and mirrored back so both UIs show the same number.
    pub(super) fn apply_playback_speed(&self, speed: f32) {
        let speed = nova_config::round_playback_speed(speed);
        self.store_playback_speed(speed);
        self.player.set_speed(speed);
    }

    /// Main thread: transient rate preview (press-and-hold 2× on the video).
    /// Pushes straight into the running session without touching the stored
    /// per-device value, the UI mirror or the debounced save — the release
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
        let current = self.app().map(|a| a.get_playback_speed()).unwrap_or(1.0);
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
    /// badges/checks plus Home's Continue Watching + Upcoming rows. Called
    /// after progress writes and whenever fresh episode meta lands (which
    /// may add unaired episodes, dropping stale ✓/Seen states and
    /// Completed buckets). Lightweight: no thumbnail re-queue.
    pub(super) fn refresh_library_progress_ui(&self) {
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
        engine.on_playback_stopped(&settings);
        // The whole row text carries the release name (Torrentio puts it on a
        // later line, not the first), which helps pick the right file inside
        // the torrent.
        let requested_name = display.trim().to_string();
        self.set_stream_hint(Some(StreamHint::Fixed("Connecting to peers…")));
        let bridge = self.clone();
        let active_hash = info_hash.clone();
        engine.resolve(
            info_hash,
            file_idx,
            requested_name,
            settings,
            move |result| {
                let bridge = bridge.clone();
                let active_hash = active_hash.clone();
                let _ = slint::invoke_from_event_loop(move || match result {
                    Ok(ready) => {
                        bridge.set_stream_hint(Some(StreamHint::Playing(ready.display_name)));
                        if nova_config::active_cache_settings().player_external {
                            // User choice (Settings → Player): external player,
                            // always. Not tracked: there is no close event.
                            let _ = crate::player::open_external(&ready.url);
                        } else if bridge.open_player(ready.url.clone()) {
                            // Remember which torrent the player is presenting
                            // so the 250 ms tick can report download stats
                            // while mpv buffers, and the close path can
                            // pause/delete it. The external fallback below has
                            // no close event, so it is not tracked here.
                            {
                                bridge.shared.lock().unwrap().active_torrent = Some(active_hash);
                            }
                        } else {
                            // Built-in failed: external app as a fallback.
                            // Not tracked: there is no close event.
                            let _ = crate::player::open_external(&ready.url);
                        }
                    }
                    Err(e) => {
                        bridge.set_stream_hint(Some(StreamHint::P2pUnavailable(e.to_string())))
                    }
                });
            },
        );
    }

    /// Open the in-app mpv player for `url` (replacing any previous player
    /// window). Returns false when the player can't be created (e.g. mpv or
    /// the window can't be initialized), so callers can fall back to an
    /// external player (ACTION_VIEW on Android, xdg-open elsewhere). Only ever
    /// runs on the main/UI thread.
    pub(super) fn open_player(&self, url: String) -> bool {
        self.open_player_with_options(url, Vec::new(), Vec::new())
    }

    pub(super) fn open_player_with_options(
        &self,
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    ) -> bool {
        self.clear_next_episode_prompt();
        let resume_choice = self.shared.lock().unwrap().resume_prompt_choice.take();
        // Set the player title from the modal item before opening.
        let (title, series_id, request_id, is_episode, is_movie) = {
            let state = self.shared.lock().unwrap();
            let m = state.modal_item.as_ref();
            (
                m.map(|m| m.name.clone()).unwrap_or_default(),
                m.map(|m| m.id.clone()).unwrap_or_default(),
                m.map(|m| m.request_id.clone()).unwrap_or_default(),
                m.map(|m| !m.seasons.is_empty()).unwrap_or(false),
                m.map(|m| m.type_ == "movie").unwrap_or(false),
            )
        };
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
        let ep = self.app().and_then(|a| {
            let ctx = a.get_episode_context();
            if ctx.is_empty() {
                None
            } else {
                Some(ctx.to_string())
            }
        });
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
            if app.get_player_poster().size().width == 0 {
                app.set_player_poster(app.get_selected_poster());
            }
        }

        match self
            .player
            .play_with_options(&url, start_at, &headers, &subtitles)
        {
            Ok(()) => true,
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
        if let Some(stream) = pending_stream {
            self.shared.lock().unwrap().resume_prompt_choice = Some(resume);
            let _ = self.open_player_with_options(stream.url, stream.headers, stream.subtitles);
        }
    }

    pub(super) fn cancel_resume_prompt(&self) {
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
        let player_open = app.get_player_open();
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
            if !(due_by_time || due_by_delta || newly_watched) {
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
                write_progress_map(&map);
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

    /// Recompute library badges in place (no poster/model rebuild).
    pub(super) fn update_library_badges(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let (view, map) = (
            self.current_library_view(),
            self.shared.lock().unwrap().progress.clone(),
        );
        let model = app.get_library();
        if model.row_count() != view.len()
            || view.iter().enumerate().any(|(i, entry)| {
                model
                    .row_data(i)
                    .is_none_or(|card| card.id.as_str() != entry.id)
            })
        {
            // Progress can move a title between automatic filters. Refresh
            // the model before indices can drift away from the displayed cards.
            self.apply_library_to_ui();
            return;
        }
        for (i, entry) in view.iter().enumerate() {
            let episodes = read_episodes_cache_for(&entry.type_, &entry.id).unwrap_or_default();
            let badge = library_badge_for(&entry.id, &episodes, &map);
            let status = text::tr(
                entry
                    .watch_status
                    .badge_label()
                    .unwrap_or_else(|| auto_bucket(&entry.id, &episodes, &map)),
            );
            let media_type = text::tr(if entry.type_ == "movie" {
                "Movie"
            } else {
                "TV"
            });
            let watched = series_fully_watched(&entry.id, &episodes, &map);
            let (watched_count, episode_count) = if entry.type_ == "movie" {
                (0, 0)
            } else {
                library_episode_counts(&entry.id, &episodes, &map)
            };
            let Some(mut card) = model.row_data(i) else {
                continue;
            };
            if card.badge.as_str() != badge.as_str()
                || card.watched != watched
                || card.status.as_str() != status
                || card.media_type.as_str() != media_type
                || card.watched_count != watched_count
                || card.episode_count != episode_count
            {
                card.badge = badge.into();
                card.status = status.into();
                card.media_type = media_type.into();
                card.watched = watched;
                card.watched_count = watched_count;
                card.episode_count = episode_count;
                model.set_row_data(i, card);
            }
        }
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
    read_json::<HashMap<String, EpisodeProgress>>(EPISODE_PROGRESS_KEY).unwrap_or_default()
}
/// Persist the whole progress map.
pub(crate) fn write_progress_map(map: &HashMap<String, EpisodeProgress>) {
    write_json(EPISODE_PROGRESS_KEY, map);
    if !applying() {
        notify_progress(map);
    }
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
        None => crate::torrent::install(crate::torrent::TorrentEngine::setup(
            torrent_cache_dir(),
            settings,
        )),
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
            Arc::new(Mutex::new(PosterStore::new(1))),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
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
    }
}

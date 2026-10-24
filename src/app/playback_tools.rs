//! Profile-local scene notes and a guarded handoff to the next offered source.
use super::*;
use nova_config::playback_preferences::TrackChoice;
use std::time::{Duration, Instant};

use super::bookmarks::{
    Bookmark, clean_note, identity_key, load_bookmarks, new_bookmark, save_bookmarks, scene_id,
};

const SECONDARY_KEY: &str = "secondary_subtitles:v1";
const SECONDARY_POSITION_KEY: &str = "secondary_subtitle_positions:v1";

#[derive(Default)]
pub(super) struct PlaybackToolsState {
    identity: Option<String>,
    bookmarks: Vec<Bookmark>,
    pub(super) failed_retry: Option<playback::RetryContext>,
    pub(super) pending_retry: Option<playback::RetryContext>,
    pub(super) bookmark_resume: Option<playback::BookmarkResume>,
    pub(super) pending_stream_selection: Option<playback::SelectedStream>,
    pub(super) recovery_prompt: RecoveryPrompt,
    pub(super) success_progress: source_recovery::ProgressEvidence,
    pub(super) guarded_resume: Option<f64>,
}

/// Only a continuous foreground wait may suggest switching sources. Normal
/// playback, user pauses, seeks and long background gaps start a fresh wait.
#[derive(Default)]
pub(super) struct RecoveryPrompt {
    last_tick: Option<Instant>,
    last_position: Option<f64>,
    waiting: Duration,
}
struct RecoveryObservation {
    open: bool,
    started: bool,
    paused: bool,
    buffering: bool,
    ended: bool,
    position: f64,
}
impl RecoveryPrompt {
    fn observe(&mut self, now: Instant, observation: RecoveryObservation) -> bool {
        let RecoveryObservation {
            open,
            started,
            paused,
            buffering,
            ended,
            position,
        } = observation;
        if !open || paused || ended || !position.is_finite() {
            *self = Self::default();
            return false;
        }
        let elapsed = self
            .last_tick
            .replace(now)
            .map(|previous| now.saturating_duration_since(previous));
        let previous = self.last_position.replace(position);
        let moved = previous.is_some_and(|previous| (position - previous).abs() > 0.01);
        let waiting = !started || buffering || previous.is_some_and(|_| !moved);
        if (moved && started) || !waiting {
            self.waiting = Duration::ZERO;
        } else if let Some(elapsed) = elapsed {
            // The UI tick normally runs every 250ms. Do not count time asleep,
            // suspended or behind a blocked event loop toward a recovery prompt.
            if elapsed <= Duration::from_secs(2) {
                self.waiting += elapsed;
            } else {
                self.waiting = Duration::ZERO;
            }
        }
        self.waiting >= Duration::from_secs(30)
    }
}

fn title_key(media_type: &str, title_id: &str) -> String {
    serde_json::to_string(&(media_type, title_id)).expect("string tuple is serializable")
}

impl Bridge {
    pub(super) fn refresh_playback_tools(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let identity = app
            .get_player_open()
            .then(|| self.current_playback_identity())
            .flatten();
        let key = identity.as_ref().map(identity_key);
        let changed = self.shared.lock().unwrap().playback_tools.identity != key;
        if changed {
            let mut bookmarks = key
                .as_ref()
                .and_then(|key| load_bookmarks().ok()?.remove(key))
                .unwrap_or_default()
                .into_iter()
                .filter(|bookmark| bookmark.position.is_finite() && bookmark.position >= 0.0)
                .collect::<Vec<_>>();
            bookmarks.sort_by(|a, b| a.position.total_cmp(&b.position));
            let mut state = self.shared.lock().unwrap();
            state.playback_tools.identity = key;
            state.playback_tools.bookmarks = bookmarks;
            drop(state);
            self.publish_scene_bookmarks();
        }
        app.set_scene_bookmarks_available(identity.is_some() && app.get_playback_started());
        let available = self.alternate_stream_index().is_some();
        app.set_try_another_stream_available(available);
        let readiness = self.player.playback_readiness();
        let observation = RecoveryObservation {
            open: app.get_player_open(),
            started: app.get_playback_started(),
            // Loading deliberately publishes is_paused=true until its first
            // frame. The native pause intent distinguishes that placeholder
            // from an actual pause or Android background suspension.
            paused: if app.get_playback_started() {
                app.get_is_paused()
            } else {
                self.player.playback_pause_requested()
            },
            buffering: readiness.buffering,
            ended: readiness.ended,
            position: app.get_position() as f64,
        };
        let paused = observation.paused;
        let now = Instant::now();
        let local_foreground = remote_playback::is_foreground()
            && !self.party_active()
            && !self.remote_receiver_playback();
        let confirmation_hold = self.player.awaiting_start_confirmation();
        let automatic_allowed =
            local_foreground && !app.get_resume_prompt_visible() && !confirmation_hold;
        let playing = app.get_player_open()
            && app.get_playback_started()
            && !paused
            && !readiness.buffering
            && !readiness.ended
            && remote_playback::is_foreground();
        let success = {
            let mut state = self.shared.lock().unwrap();
            let identity = state
                .playback_intent
                .as_ref()
                .filter(|intent| intent.active_local())
                .and_then(|intent| intent.context.selected_stream.as_ref())
                .and_then(|selected| selected.choice.clone());
            state
                .playback_tools
                .success_progress
                .observe(now, app.get_position() as f64, playing)
                .then_some(identity)
                .flatten()
        };
        if let Some(identity) = success {
            source_recovery::remember(&identity);
        }
        let visible = self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .recovery_prompt
            .observe(now, observation);
        app.set_automatic_stream_recovery_visible(available && visible);
        let automatic = {
            let mut state = self.shared.lock().unwrap();
            let enabled = state.cache_settings.auto_retry_streams
                && !state.cache_settings.uses_external_player()
                && !state.casting.active
                && !state.casting.loading;
            let context = if let Some(intent) = state.playback_intent.as_mut() {
                Some(&mut intent.context)
            } else {
                state
                    .playback_tools
                    .failed_retry
                    .as_mut()
                    .map(|retry| &mut retry.context)
            };
            if let Some(context) = context {
                let gap = context
                    .retry_budget
                    .last_tick
                    .replace(now)
                    .is_some_and(|previous| {
                        now.saturating_duration_since(previous) > Duration::from_secs(2)
                    });
                // A normal resume question blocks action without cancelling
                // the not-yet-started run. Explicit pause callbacks revoke it
                // immediately, including pauses shorter than one UI tick.
                if (paused && !app.get_resume_prompt_visible() && !confirmation_hold)
                    || !local_foreground
                    || gap
                    || (!enabled && context.automatic_source_selection)
                {
                    context.retry_budget.cancelled = true;
                }
                enabled
                    && automatic_allowed
                    && !context.retry_budget.cancelled
                    && available
                    && visible
            } else {
                false
            }
        };
        if automatic {
            self.retry_another_stream(true);
        }
    }

    pub(super) fn reload_scene_bookmarks(&self) {
        self.shared.lock().unwrap().playback_tools.identity = None;
        self.refresh_playback_tools();
    }

    fn publish_scene_bookmarks(&self) {
        let state = self.shared.lock().unwrap();
        let key = state.playback_tools.identity.as_deref().unwrap_or("");
        let rows = state
            .playback_tools
            .bookmarks
            .iter()
            .map(|bookmark| nova_ui::SceneBookmarkRow {
                id: scene_id(key, bookmark).into(),
                position: bookmark.position as f32,
                note: bookmark.note.clone().into(),
            })
            .collect::<Vec<_>>();
        drop(state);
        if let Some(app) = self.app() {
            app.set_scene_bookmarks(Rc::new(VecModel::from(rows)).into());
        }
    }

    fn scene_bookmark_error(&self, message: &str) -> bool {
        if let Some(app) = self.app() {
            app.set_scene_bookmark_notice(text::tr(message).into());
        }
        false
    }

    /// Resolve only a chapter currently advertised by the active native source.
    pub(super) fn chapter_position(&self, id: &str) -> Option<f64> {
        let app = self.app()?;
        app.get_player_chapters().iter().find_map(|row| {
            let position = f64::from(row.position);
            (position.is_finite()
                && position >= 0.0
                && format!("c{:016x}", position.to_bits()) == id)
                .then_some(position)
        })
    }

    /// The ID is meaningful only inside this title's current bookmark list.
    pub(super) fn scene_bookmark_position(&self, id: &str) -> Option<f64> {
        let key = identity_key(&self.current_playback_identity()?);
        let values = load_bookmarks().ok()?;
        values.get(&key)?.iter().find_map(|bookmark| {
            (bookmark.position.is_finite()
                && bookmark.position >= 0.0
                && scene_id(&key, bookmark) == id)
                .then_some(bookmark.position)
        })
    }

    pub(super) fn add_scene_bookmark(&self, note: &str) -> bool {
        let Some(app) = self
            .app()
            .filter(|app| app.get_player_open() && app.get_playback_started())
        else {
            return false;
        };
        let Some(identity) = self.current_playback_identity() else {
            return false;
        };
        let position = app.get_position() as f64;
        if !position.is_finite() || position < 0.0 {
            return false;
        }
        let key = identity_key(&identity);
        let Ok(mut values) = load_bookmarks() else {
            return self.scene_bookmark_error(
                "Could not read your scene bookmarks. Your saved data has been kept.",
            );
        };
        let poster = detail::read_meta_header_for(&identity.media_type, &identity.title_id)
            .map(|header| header.poster_url)
            .unwrap_or_default();
        let added = new_bookmark(position, note, &identity, &poster);
        let rows = values.entry(key).or_default();
        if let Some(existing) = rows
            .iter_mut()
            .find(|bookmark| (bookmark.position - position).abs() < 0.5)
        {
            let note = clean_note(note);
            if !note.is_empty() {
                existing.note = note;
            }
            if existing.metadata.is_none() {
                existing.metadata = added.metadata;
            }
        } else {
            rows.push(added);
        }
        rows.sort_by(|a, b| a.position.total_cmp(&b.position));
        if save_bookmarks(&values).is_err() {
            return self.scene_bookmark_error(
                "Could not save your scene bookmark. Your note has been kept.",
            );
        }
        self.reload_scene_bookmarks();
        self.refresh_bookmarks();
        if let Some(app) = self.app() {
            app.set_scene_bookmark_notice("".into());
        }
        true
    }

    pub(super) fn edit_scene_bookmark(&self, id: &str, note: &str) -> bool {
        let Some(identity) = self.current_playback_identity() else {
            return false;
        };
        let key = identity_key(&identity);
        let Ok(mut values) = load_bookmarks() else {
            return self.scene_bookmark_error(
                "Could not read your scene bookmarks. Your saved data has been kept.",
            );
        };
        let Some(bookmark) = values.get_mut(&key).and_then(|rows| {
            rows.iter_mut()
                .find(|bookmark| scene_id(&key, bookmark) == id)
        }) else {
            return false;
        };
        bookmark.note = clean_note(note);
        if save_bookmarks(&values).is_err() {
            return self.scene_bookmark_error(
                "Could not save your scene bookmark. Your note has been kept.",
            );
        }
        self.reload_scene_bookmarks();
        self.refresh_bookmarks();
        if let Some(app) = self.app() {
            app.set_scene_bookmark_notice("".into());
        }
        true
    }

    pub(super) fn delete_scene_bookmark(&self, id: &str) -> bool {
        let Some(identity) = self.current_playback_identity() else {
            return false;
        };
        let key = identity_key(&identity);
        let Ok(mut values) = load_bookmarks() else {
            return self.scene_bookmark_error(
                "Could not read your scene bookmarks. Your saved data has been kept.",
            );
        };
        let Some(rows) = values.get_mut(&key) else {
            return false;
        };
        let before = rows.len();
        rows.retain(|bookmark| scene_id(&key, bookmark) != id);
        if before == rows.len() {
            return false;
        }
        if rows.is_empty() {
            values.remove(&key);
        }
        if save_bookmarks(&values).is_err() {
            return self.scene_bookmark_error(
                "Could not save your scene bookmark. Your note has been kept.",
            );
        }
        self.reload_scene_bookmarks();
        self.refresh_bookmarks();
        if let Some(app) = self.app() {
            app.set_scene_bookmark_notice("".into());
        }
        true
    }

    pub(super) fn prepare_secondary_subtitles(&self) {
        let choice = self
            .playback_preference_identity(true)
            .and_then(|(media_type, id)| {
                read_json::<HashMap<String, TrackChoice>>(SECONDARY_KEY)?
                    .remove(&title_key(&media_type, &id))
            })
            .unwrap_or(TrackChoice::Off);
        let position = self
            .playback_preference_identity(true)
            .and_then(|(media_type, id)| {
                read_json::<HashMap<String, i32>>(SECONDARY_POSITION_KEY)?
                    .remove(&title_key(&media_type, &id))
            })
            .filter(|position| (0..=2).contains(position))
            .unwrap_or(0);
        self.player.set_secondary_subtitle_position(position);
        if let Some(app) = self.app() {
            app.set_secondary_subtitle_position_index(position);
        }
        self.player.set_secondary_subtitle_preference(choice);
    }

    pub(super) fn change_secondary_subtitle_position(&self, index: i32) {
        if !(0..=2).contains(&index)
            || !self
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .can_play_locally()
        {
            return;
        }
        let Some((media_type, title)) = self.playback_preference_identity(false) else {
            return;
        };
        let mut values =
            read_json::<HashMap<String, i32>>(SECONDARY_POSITION_KEY).unwrap_or_default();
        if !writable_key(SECONDARY_POSITION_KEY) {
            return;
        }
        values.insert(title_key(&media_type, &title), index);
        let failures = write_failures();
        write_json(SECONDARY_POSITION_KEY, &values);
        if failures != write_failures() {
            return;
        }
        self.player.set_secondary_subtitle_position(index);
        if let Some(app) = self.app() {
            app.set_secondary_subtitle_position_index(index);
        }
    }

    pub(super) fn pick_secondary_playback_track(&self, id: i32) {
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return;
        }
        let Some((media_type, title)) = self.playback_preference_identity(false) else {
            return;
        };
        self.cancel_secondary_subtitle_download();
        if id < -1 {
            self.load_secondary_addon_subtitle(id, true);
            return;
        }
        if let Some(choice) = self.player.pick_secondary_subtitle(id) {
            let mut values =
                read_json::<HashMap<String, TrackChoice>>(SECONDARY_KEY).unwrap_or_default();
            values.insert(title_key(&media_type, &title), choice);
            write_json(SECONDARY_KEY, &values);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bookmark_identity_keeps_title_episode_and_media_type_distinct() {
        let identity = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "a:b".into(),
            episode_id: "c".into(),
            title: String::new(),
            episode: String::new(),
        };
        let mut other = identity.clone();
        other.title_id = "a".into();
        other.episode_id = "b:c".into();
        assert_ne!(identity_key(&identity), identity_key(&other));
        other = identity.clone();
        other.media_type = "movie".into();
        assert_ne!(identity_key(&identity), identity_key(&other));
        other = identity.clone();
        other.title = "translated caption".into();
        assert_eq!(identity_key(&identity), identity_key(&other));
    }

    fn observation(started: bool, position: f64) -> RecoveryObservation {
        RecoveryObservation {
            open: true,
            started,
            paused: false,
            buffering: false,
            ended: false,
            position,
        }
    }

    #[test]
    fn recovery_prompt_waits_thirty_continuous_seconds_and_healthy_progress_hides_it() {
        let base = Instant::now();
        let mut prompt = RecoveryPrompt::default();
        for second in 0..30 {
            assert!(!prompt.observe(base + Duration::from_secs(second), observation(false, 0.0)));
        }
        assert!(prompt.observe(base + Duration::from_secs(30), observation(false, 0.0)));
        // Startup and early-failure text use the same delayed state. First real
        // progress immediately removes it and never accrues healthy play time.
        for second in 31..100 {
            assert!(!prompt.observe(
                base + Duration::from_secs(second),
                observation(true, second as f64)
            ));
        }
        // A position stall also qualifies, even without a cache-starved flag.
        for second in 100..129 {
            assert!(!prompt.observe(base + Duration::from_secs(second), observation(true, 99.0)));
        }
        assert!(prompt.observe(base + Duration::from_secs(129), observation(true, 99.0)));
        assert!(!prompt.observe(base + Duration::from_secs(130), observation(true, 100.0)));
    }

    #[test]
    fn recovery_prompt_resets_for_pause_eof_close_background_gap_and_new_source() {
        let base = Instant::now();
        for reset in 0..5 {
            let mut prompt = RecoveryPrompt::default();
            for second in 0..26 {
                let mut sample = observation(true, 10.0);
                sample.buffering = true;
                assert!(!prompt.observe(base + Duration::from_secs(second), sample));
            }
            let restart = if reset == 3 { 90 } else { 26 };
            let mut sample = observation(true, 10.0);
            match reset {
                0 => sample.paused = true,
                1 => sample.ended = true,
                2 => sample.open = false,
                3 => {}
                _ => prompt = RecoveryPrompt::default(),
            }
            assert!(!prompt.observe(base + Duration::from_secs(restart), sample));
            for second in restart + 1..restart + 30 {
                assert!(
                    !prompt.observe(base + Duration::from_secs(second), observation(true, 10.0)),
                    "reset {reset} leaked prior waiting time"
                );
            }
        }
    }
}

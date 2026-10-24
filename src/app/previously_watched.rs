//! Explicit, spoiler-bounded reminders from recorded playback, plus isolated ending replays.
use super::*;
use nova_ui_shared::LastTimeBridge;

const RETURN_AFTER: u64 = 30 * 86400;
const ENDING_SECONDS: f64 = 240.0;

fn activation() -> profiles::Context {
    let mut context = profiles::capture();
    // Shared belongs to the session. Retaining that session here would create
    // a reference cycle across profile switches; identity needs only id/epoch.
    context.session = None;
    context
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ReplayEvidence {
    pub(super) source: Option<String>,
    pub(super) duration: f64,
}
impl ReplayEvidence {
    pub(super) fn stored_valid(&self) -> bool {
        self.duration.is_finite()
            && self.duration > 0.0
            && self.source.as_ref().is_none_or(|source| {
                source.strip_prefix("stream-").is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())
                })
            })
    }
    fn valid(&self) -> bool {
        self.stored_valid()
            && self.duration.is_finite()
            && self.duration > ENDING_SECONDS
            && self
                .source
                .as_ref()
                .is_none_or(|s| !s.is_empty() && s.len() <= 256)
    }
}
#[derive(Default)]
pub(super) struct Facts {
    pub(super) last_at: Option<u64>,
    pub(super) completed: Option<Completed>,
}
pub(super) struct Completed {
    pub(super) episode_id: String,
    pub(super) replay: Option<ReplayEvidence>,
}
#[derive(Clone)]
struct Selection {
    owner: profiles::Context,
    type_: String,
    title_id: String,
    episode_id: String,
    evidence: Option<ReplayEvidence>,
    modal: Option<Arc<()>>,
}
impl Selection {
    fn matches(&self, modal: Option<&ModalItem>) -> bool {
        self.owner == profiles::capture()
            && modal.is_some_and(|m| {
                self.type_ == m.type_
                    && self.title_id == m.id
                    && self.episode_id == m.request_id
                    && self
                        .modal
                        .as_ref()
                        .is_some_and(|token| Arc::ptr_eq(token, &m.open_token))
            })
    }
}
#[derive(Default)]
pub(super) struct State {
    selection: Option<Selection>,
    generation: u64,
}
/// Lives in the selected source's immutable request context, surviving resolvers
/// and retries. It never becomes a normal PlaybackTarget or a durable watch.
#[derive(Clone)]
pub(super) struct Replay {
    owner: profiles::Context,
    evidence: Option<ReplayEvidence>,
    pub(super) confirmed: bool,
    pub(super) duration: f64,
}

impl Replay {
    pub(super) fn prepare_source(&mut self) {
        // Approval belongs to one loaded version. A replacement source must
        // establish its own duration and matching cut before any ending seek.
        self.confirmed = false;
        self.duration = 0.0;
    }
}

pub(super) fn take_replay(state: &Shared, context: &playback::PlaybackContext) -> Option<Replay> {
    state
        .previously_watched
        .selection
        .as_ref()
        .filter(|s| s.matches(state.modal_item.as_ref()))
        .filter(|s| {
            s.type_ == context.media_type
                && s.title_id == context.media_id
                && s.episode_id == context.request_id
        })
        .map(|s| Replay {
            owner: s.owner.clone(),
            evidence: s.evidence.clone(),
            confirmed: false,
            duration: 0.0,
        })
}

/// A stale downloaded stream row cannot discard an explicit replay context by
/// falling through the download player's ordinary mismatched-context default.
pub(super) fn download_allowed(
    state: &Shared,
    type_: &str,
    title_id: &str,
    episode_id: &str,
) -> bool {
    state
        .previously_watched
        .selection
        .as_ref()
        .filter(|s| s.matches(state.modal_item.as_ref()))
        .is_none_or(|s| s.type_ == type_ && s.title_id == title_id && s.episode_id == episode_id)
}

pub(super) fn due(
    progress: &HashMap<String, EpisodeProgress>,
    type_: &str,
    id: &str,
    now: u64,
) -> bool {
    let last = progress
        .values()
        .filter(|p| p.series_id == id)
        .filter_map(|p| p.history.as_ref())
        .filter(|h| h.type_ == type_)
        .map(|h| h.played_at_secs)
        .filter(|at| *at > 0 && *at <= now)
        .max();
    last.is_some_and(|at| now.saturating_sub(at) >= RETURN_AFTER)
}

impl Bridge {
    pub(super) fn setup_last_time(&self) {
        let Some(app) = self.app() else { return };
        let bridge = self.clone();
        app.global::<LastTimeBridge>()
            .on_replay(move || bridge.replay_last_ending());
        let bridge = self.clone();
        app.global::<LastTimeBridge>()
            .on_confirm(move || bridge.confirm_last_ending());
        let bridge = self.clone();
        app.global::<LastTimeBridge>().on_closed(move || {
            let Some(app) = bridge.app() else { return };
            let sheet = app.global::<LastTimeBridge>();
            let cancel = sheet.get_confirmation();
            {
                let mut state = bridge.shared.lock().unwrap();
                state.previously_watched.generation =
                    state.previously_watched.generation.wrapping_add(1);
            }
            sheet.set_open(false);
            sheet.set_confirmation(false);
            if cancel && bridge.last_time_replay_active() {
                bridge.player.close();
            }
        });
        let bridge = self.clone();
        app.global::<LastTimeBridge>().on_visual_closed(move || {
            let Some(app) = bridge.app() else { return };
            let sheet = app.global::<LastTimeBridge>();
            if !sheet.get_open() && !sheet.get_visual_retained() {
                app.invoke_preserve_back_focus();
                app.invoke_focus_mounted_page();
            }
        });
    }

    /// Profile changes must not keep the outgoing profile's synopsis painted.
    pub(super) fn invalidate_last_time(&self) {
        let Some(app) = self.app() else { return };
        let sheet = app.global::<LastTimeBridge>();
        // Release the old owner's focus guard synchronously. The generation
        // invalidates its painted snapshot in the same UI turn, before a frame.
        sheet.set_visual_retained(false);
        sheet.set_dismiss_generation(sheet.get_dismiss_generation().wrapping_add(1));
        sheet.set_open(false);
        sheet.set_confirmation(false);
        sheet.set_title("".into());
        sheet.set_date("".into());
        sheet.set_episode("".into());
        sheet.set_synopsis("".into());
        sheet.set_partial("".into());
        sheet.set_can_replay(false);
    }

    pub(super) fn open_last_time(&self, type_: &str, id: &str, resume_id: &str) {
        if type_ == "movie" {
            return;
        }
        let Some(app) = self.app() else { return };
        let mut state = self.shared.lock().unwrap();
        let Some(entry) = state
            .entries
            .iter()
            .find(|e| e.type_ == type_ && e.id == id)
        else {
            return;
        };
        let title = entry.name.clone();
        state.previously_watched.generation = state.previously_watched.generation.wrapping_add(1);
        let generation = state.previously_watched.generation;
        state.previously_watched.selection = None;
        drop(state);
        let sheet = app.global::<LastTimeBridge>();
        sheet.set_title(title.into());
        sheet.set_confirmation(false);
        sheet.set_date("".into());
        sheet.set_episode("".into());
        sheet.set_synopsis(text::tr("Loading playback history…").into());
        sheet.set_partial("".into());
        sheet.set_can_replay(false);
        sheet.set_open(true);
        let (type_, id, resume_id) = (type_.to_string(), id.to_string(), resume_id.to_string());
        let bridge = self.clone();
        thread::spawn(move || {
            let facts = viewing_journal::last_time(&profiles::ProfileStore::capture(), &type_, &id);
            let videos = read_episodes_cache_for(&type_, &id).unwrap_or_default();
            let _ = profiles::invoke_from_event_loop(move || {
                bridge.apply_last_time(&type_, &id, &resume_id, generation, facts, videos)
            });
        });
    }

    fn apply_last_time(
        &self,
        type_: &str,
        id: &str,
        resume_id: &str,
        generation: u64,
        facts: Facts,
        videos: Vec<Video>,
    ) {
        let Some(app) = self.app() else { return };
        let sheet = app.global::<LastTimeBridge>();
        let mut state = self.shared.lock().unwrap();
        if state.previously_watched.generation != generation
            || !sheet.get_open()
            || sheet.get_confirmation()
        {
            return;
        }
        sheet.set_date(
            facts
                .last_at
                .map(|at| text::journal_date(at, false))
                .unwrap_or_else(|| text::tr("No recorded playback date").into())
                .into(),
        );
        let completed_video = facts
            .completed
            .as_ref()
            .and_then(|c| videos.iter().find(|v| v.id == c.episode_id));
        sheet.set_episode(
            completed_video
                .map(|v| format!("{} · {}", episode_badge(v), episode_row_label(v)))
                .unwrap_or_default()
                .into(),
        );
        sheet.set_synopsis(
            completed_video
                .and_then(|v| v.overview.as_ref())
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    text::tr(if facts.completed.is_some() {
                        "No synopsis is available for the last completed episode."
                    } else {
                        "A completed episode could not be confirmed from playback history."
                    })
                    .into()
                })
                .into(),
        );
        // Resume episodes expose numbering/progress only: no title, art or synopsis.
        let partial = state
            .progress
            .get(&progress_map_key(id, resume_id))
            .filter(|p| resumable_position(p.position_secs, p.duration_secs, false))
            .map(|p| {
                text::last_time_partial(
                    &videos
                        .iter()
                        .find(|v| v.id == resume_id)
                        .map(episode_se_label)
                        .unwrap_or_else(|| text::tr("Episode").into()),
                    progress_fraction(p.position_secs, p.duration_secs),
                )
            })
            .unwrap_or_default();
        sheet.set_partial(partial.into());
        sheet.set_can_replay(
            completed_video.is_some()
                && !self.party_active_without_lock(&state)
                && !state.casting.selected,
        );
        state.previously_watched.selection = facts.completed.map(|c| Selection {
            owner: activation(),
            type_: type_.into(),
            title_id: id.into(),
            episode_id: c.episode_id,
            evidence: c.replay.filter(ReplayEvidence::valid),
            modal: None,
        });
        sheet.set_open(true);
    }

    // Avoid calling party_active while Shared is held.
    fn party_active_without_lock(&self, state: &Shared) -> bool {
        state.party.active()
    }

    fn replay_last_ending(&self) {
        if self.party_active() {
            return;
        }
        let selection = self
            .shared
            .lock()
            .unwrap()
            .previously_watched
            .selection
            .clone();
        let Some(selection) = selection.filter(|s| s.owner == profiles::capture()) else {
            return;
        };
        let bridge = self.clone();
        let generation = self.shared.lock().unwrap().previously_watched.generation;
        thread::spawn(move || {
            let facts = viewing_journal::last_time(
                &profiles::ProfileStore::capture(),
                &selection.type_,
                &selection.title_id,
            );
            if !facts
                .completed
                .is_some_and(|c| c.episode_id == selection.episode_id)
            {
                return;
            }
            let _ = profiles::invoke_from_event_loop(move || {
                if bridge.shared.lock().unwrap().previously_watched.generation == generation {
                    bridge.begin_last_ending(selection);
                }
            });
        });
    }

    fn begin_last_ending(&self, selection: Selection) {
        let Some(entry) = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.type_ == selection.type_ && e.id == selection.title_id)
            .cloned()
        else {
            return;
        };
        if let Some(app) = self.app() {
            app.global::<LastTimeBridge>().set_open(false);
        }
        self.open_library_entry(entry);
        // A new selector token scopes the explicit replay through every async callback.
        {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state
                .modal_item
                .as_ref()
                .filter(|m| m.type_ == selection.type_ && m.id == selection.title_id)
            else {
                return;
            };
            let mut armed = selection.clone();
            armed.modal = Some(modal.open_token.clone());
            state.previously_watched.selection = Some(armed);
        }
        self.episode_streams_by_id(&selection.title_id, &selection.episode_id);
        // Normal selection arms tracking before a source exists. It must never
        // checkpoint the old ending or promote it into Continue Watching.
        self.shared.lock().unwrap().playback = None;
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(true);
        }
    }

    pub(super) fn last_time_replay_selected(&self) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .previously_watched
            .selection
            .as_ref()
            .is_some_and(|s| s.matches(state.modal_item.as_ref()))
    }

    pub(super) fn last_time_action_allowed(
        &self,
        action: &nova_playback_api::PlaybackAction,
    ) -> bool {
        let pending = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .and_then(|i| i.context.ending_replay.as_ref())
            .is_some_and(|r| !r.confirmed);
        !pending
            || matches!(
                action,
                nova_playback_api::PlaybackAction::Stop
                    | nova_playback_api::PlaybackAction::SetPaused(true)
                    | nova_playback_api::PlaybackAction::SetVolume(_)
                    | nova_playback_api::PlaybackAction::VolumeStep(_)
                    | nova_playback_api::PlaybackAction::SystemVolumeStep(_)
                    | nova_playback_api::PlaybackAction::SetMuted(_)
                    | nova_playback_api::PlaybackAction::ToggleMute
                    | nova_playback_api::PlaybackAction::SetFullscreen(_)
                    | nova_playback_api::PlaybackAction::ToggleFullscreen
                    | nova_playback_api::PlaybackAction::EndSpeedPreview
            )
    }

    pub(super) fn last_time_replay_active(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .is_some_and(|i| i.context.ending_replay.is_some())
    }

    pub(super) fn last_time_playback_evidence(&self, duration: f64) -> Option<ReplayEvidence> {
        let state = self.shared.lock().unwrap();
        let context = &state.playback_intent.as_ref()?.context;
        let evidence = ReplayEvidence {
            source: playback::replay_source(context),
            duration,
        };
        evidence.valid().then_some(evidence)
    }

    /// Return true while a replay owns playback. Normal progress, journal,
    /// tracking, next-episode prompts and completion callbacks are bypassed.
    pub(super) fn note_last_time_replay(&self) -> bool {
        let Some(app) = self.app() else { return false };
        let replay = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .and_then(|i| i.context.ending_replay.clone());
        let Some(replay) = replay else { return false };
        if replay.owner != profiles::capture() || self.party_active() {
            self.player.close();
            self.finish_playback_request();
            return true;
        }
        if !app.get_player_open() {
            self.finish_playback_request();
            return true;
        }
        if replay.confirmed || !app.get_playback_started() {
            return true;
        }
        self.player.set_paused(true);
        let duration = f64::from(app.get_duration());
        if !duration.is_finite() || duration <= 0.0 {
            return true;
        }
        if duration <= ENDING_SECONDS {
            self.player.close();
            self.finish_playback_request();
            self.set_stream_hint(Some(StreamHint::Fixed(
                "This version is too short to replay an ending safely.",
            )));
            return true;
        }
        let exact = {
            let mut state = self.shared.lock().unwrap();
            let Some(intent) = state.playback_intent.as_mut() else {
                return true;
            };
            let source = playback::replay_source(&intent.context);
            let exact = replay.evidence.as_ref().is_some_and(|e| {
                source_recovery::Timeline {
                    position: duration - ENDING_SECONDS,
                    duration: e.duration,
                    source: e.source.clone(),
                }
                .can_resume_without_question(source.as_deref(), duration)
            });
            intent.context.ending_replay.as_mut().unwrap().duration = duration;
            exact
        };
        if exact {
            self.confirm_last_ending();
        } else {
            let sheet = app.global::<LastTimeBridge>();
            sheet.set_title(app.get_player_show_title());
            sheet.set_date(text::journal_duration(duration).into());
            sheet.set_confirmation(true);
            sheet.set_open(true);
        }
        true
    }

    fn confirm_last_ending(&self) {
        let Some(app) = self.app() else { return };
        let position = {
            let mut state = self.shared.lock().unwrap();
            let Some(replay) = state
                .playback_intent
                .as_mut()
                .and_then(|i| i.context.ending_replay.as_mut())
            else {
                return;
            };
            if replay.owner != profiles::capture()
                || replay.confirmed
                || replay.duration <= ENDING_SECONDS
            {
                return;
            }
            replay.confirmed = true;
            replay.duration - ENDING_SECONDS
        };
        app.global::<LastTimeBridge>().set_open(false);
        app.global::<LastTimeBridge>().set_confirmation(false);
        self.player.release_start_pause();
        self.player.seek(position as f32);
        self.player.set_paused(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_replacements_must_confirm_the_new_loaded_version() {
        let mut replay = Replay {
            owner: activation(),
            evidence: None,
            confirmed: true,
            duration: 2400.0,
        };
        replay.prepare_source();
        assert!(!replay.confirmed);
        assert_eq!(replay.duration, 0.0);
    }

    #[test]
    fn reminders_follow_actual_playback_clock_not_manual_progress_edits() {
        let now = 100 * 86400;
        let mut progress = HashMap::new();
        let mut row = EpisodeProgress {
            series_id: "show".into(),
            episode_id: "show:1:2".into(),
            updated_at_secs: now,
            watched: true,
            history: Some(PlaybackHistory {
                type_: "series".into(),
                played_at_secs: now - RETURN_AFTER,
                ..Default::default()
            }),
            ..Default::default()
        };
        progress.insert("show|episode".into(), row.clone());
        assert!(due(&progress, "series", "show", now));
        assert!(!due(&progress, "movie", "show", now));
        row.history.as_mut().unwrap().played_at_secs += 1;
        progress.insert("show|episode".into(), row.clone());
        assert!(!due(&progress, "series", "show", now));
        row.history = None;
        progress.insert("show|episode".into(), row);
        assert!(
            !due(&progress, "series", "show", now),
            "a manual watched flag cannot invent a playback date"
        );
    }

    #[test]
    fn replay_intent_cannot_cross_selector_reopens_or_episode_changes() {
        let token = Arc::new(());
        let mut state = Shared {
            modal_item: Some(ModalItem {
                details: Default::default(),
                open_token: token.clone(),
                pending_watch_now: None,
                episodes_loading: false,
                id: "show".into(),
                type_: "series".into(),
                request_id: "show:1:2".into(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: String::new(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            }),
            ..Default::default()
        };
        state.previously_watched.selection = Some(Selection {
            owner: activation(),
            type_: "series".into(),
            title_id: "show".into(),
            episode_id: "show:1:2".into(),
            evidence: None,
            modal: Some(token),
        });
        let mut context = playback::PlaybackContext::default();
        context.media_type = "series".into();
        context.media_id = "show".into();
        context.request_id = "show:1:2".into();
        assert!(take_replay(&state, &context).is_some());
        assert!(download_allowed(&state, "series", "show", "show:1:2"));
        assert!(!download_allowed(&state, "movie", "show", "show:1:2"));
        assert!(!download_allowed(
            &state,
            "series",
            "other-show",
            "show:1:2"
        ));
        assert!(!download_allowed(&state, "series", "show", "show:1:3"));
        state.modal_item.as_mut().unwrap().request_id = "show:1:3".into();
        assert!(take_replay(&state, &context).is_none());
        state.modal_item.as_mut().unwrap().request_id = "show:1:2".into();
        state.modal_item.as_mut().unwrap().open_token = Arc::new(());
        assert!(take_replay(&state, &context).is_none());
    }
}

#[cfg(all(test, feature = "desktop"))]
mod replay_regression {
    use super::*;

    #[test]
    fn replay_ticks_checkpoints_and_close_preserve_continue_and_completed_history() {
        const CHILD: &str = "NOVA_LAST_TIME_REPLAY_TEST";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-last-time-replay-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(CHILD, &root)
                .args(["--exact", "app::previously_watched::replay_regression::replay_ticks_checkpoints_and_close_preserve_continue_and_completed_history", "--nocapture"])
                .output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_system_time();
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
            state.progress.insert(
                progress_map_key("show", "show:1:2"),
                EpisodeProgress {
                    series_id: "show".into(),
                    episode_id: "show:1:2".into(),
                    watched: true,
                    position_secs: 2200.0,
                    duration_secs: 2400.0,
                    play_count: 1,
                    ..Default::default()
                },
            );
            state.progress.insert(
                progress_map_key("show", "show:1:3"),
                EpisodeProgress {
                    series_id: "show".into(),
                    episode_id: "show:1:3".into(),
                    position_secs: 400.0,
                    duration_secs: 2400.0,
                    ..Default::default()
                },
            );
            state.continue_list = vec![ContinueEntry {
                type_: "series".into(),
                series_id: "show".into(),
                episode_id: "show:1:3".into(),
                ..Default::default()
            }];
        }
        let before = serde_json::to_value(&bridge.shared.lock().unwrap().progress).unwrap();
        let request = bridge.begin_playback_request();
        {
            let mut state = bridge.shared.lock().unwrap();
            let context = &mut state.playback_intent.as_mut().unwrap().context;
            context.media_type = "series".into();
            context.media_id = "show".into();
            context.request_id = "show:1:2".into();
            context.ending_replay = Some(Replay {
                owner: activation(),
                evidence: None,
                confirmed: true,
                duration: 2400.0,
            });
        }
        bridge.mark_playback_opened(true);
        app.set_player_open(true);
        app.set_playback_started(true);
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context
            .ending_replay
            .as_mut()
            .unwrap()
            .confirmed = false;
        for action in [
            nova_playback_api::PlaybackAction::SetPaused(false),
            nova_playback_api::PlaybackAction::Resume(true),
            nova_playback_api::PlaybackAction::SeekAbsolute(1500.0),
        ] {
            assert!(
                bridge.execute_playback_action(action).is_err(),
                "regular controls cannot bypass replay version approval"
            );
        }
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context
            .ending_replay
            .as_mut()
            .unwrap()
            .confirmed = true;
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_position(2390.0);
        app.set_duration(2400.0);
        bridge.note_player_progress_from_ui();
        bridge.note_journal_observation(
            nova_playback_api::MediaIdentity {
                media_type: "series".into(),
                title_id: "show".into(),
                episode_id: "show:1:2".into(),
                ..Default::default()
            },
            2390.0,
            2400.0,
            true,
            1.0,
        );
        bridge.persist_playback_checkpoint(true);
        assert_eq!(
            serde_json::to_value(&bridge.shared.lock().unwrap().progress).unwrap(),
            before
        );
        assert!(
            profiles::ProfileStore::capture()
                .scan(viewing_journal::PREFIX)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            bridge.shared.lock().unwrap().continue_list[0].episode_id,
            "show:1:3"
        );
        app.set_player_open(false);
        bridge.note_player_progress_from_ui();
        assert!(!request.is_current());
        assert_eq!(
            serde_json::to_value(&bridge.shared.lock().unwrap().progress).unwrap(),
            before
        );
        assert_eq!(
            bridge.shared.lock().unwrap().continue_list[0].episode_id,
            "show:1:3"
        );
    }
}

//! In-app playback flow and episode progress tracking.
use super::*;

#[derive(Clone, Default)]
pub(super) struct PlaybackContext {
    policy_revision: String,
    recovery_timeline: Option<source_recovery::Timeline>,
    pub(super) automatic_source_selection: bool,
    pub(super) retry_budget: source_recovery::RetryBudget,
    intro_download: Option<crate::download::DownloadJob>,
    retry_preferences: Option<nova_config::playback_preferences::SessionPreferences>,
    bookmark_resume: bool,
    pub(super) selected_stream: Option<SelectedStream>,
    subtitle_hints: Vec<(String, String)>,
    title: String,
    episode_label: String,
    osd_episode_title: String,
    media_summary: String,
    episode_number: String,
    use_modal_poster: bool,
    logo_url: String,
    media_type: String,
    media_id: String,
    request_id: String,
    is_episode: bool,
    is_movie: bool,
    history: Option<PlaybackHistory>,
    /// Explicit History action: includes zero for Play again.
    history_resume: Option<f64>,
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
    pub(super) pending: bool,
    pub(super) in_app: bool,
    // Credential-bearing resolver links stay inside the in-app player, including
    // a delayed resume prompt. Never pass them through OS process arguments.
    private_playback: bool,
    pub(super) context: PlaybackContext,
    detached: bool,
}

#[derive(Clone)]
pub(super) struct SelectedStream {
    pub(super) choice: Option<String>,
    row: String,
    owner: String,
    continuity: Option<String>,
}
impl SelectedStream {
    fn from_row(row: &StreamUi) -> Self {
        Self {
            choice: row.choice_id.clone(),
            row: row.id.clone(),
            owner: row.owner_url.clone(),
            // An explicitly selected torrent file is byte-identical across
            // addon mirrors. A pack without fileIdx does not prove the cut.
            continuity: match &row.source {
                StreamSource::Torrent {
                    info_hash,
                    file_idx: Some(index),
                } if info_hash.len() == 40
                    && info_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
                {
                    Some(preference_hash(
                        "torrent-file",
                        &format!("{}:{index}", info_hash.to_ascii_lowercase()),
                    ))
                }
                StreamSource::Url(url) => Some(preference_hash(
                    "exact-url-headers",
                    &serde_json::to_string(&(url, Vec::<(String, String)>::new()))
                        .expect("source headers serialize"),
                )),
                StreamSource::UrlWithOptions { url, headers, .. } => Some(preference_hash(
                    "exact-url-headers",
                    &serde_json::to_string(&(url, headers)).expect("source headers serialize"),
                )),
                _ => row.choice_id.clone(),
            },
        }
    }
    fn retry_identity(&self) -> &str {
        self.continuity
            .as_deref()
            .or(self.choice.as_deref())
            .unwrap_or(&self.row)
    }
    fn matches(&self, row: &StreamUi) -> bool {
        self.choice.as_ref().map_or_else(
            || self.row == row.id && self.owner == row.owner_url,
            |choice| row.choice_id.as_ref() == Some(choice),
        )
    }
}
fn alternate_stream_index(rows: &[StreamUi], selected: &SelectedStream) -> Option<usize> {
    let start = rows
        .iter()
        .position(|row| selected.matches(row))
        .map_or(0, |index| (index + 1) % rows.len());
    (0..rows.len())
        .map(|offset| (start + offset) % rows.len())
        .find(|index| {
            let row = &rows[*index];
            !selected.matches(row)
                && matches!(
                    row.source,
                    StreamSource::Url(_)
                        | StreamSource::UrlWithOptions { .. }
                        | StreamSource::Resolver { .. }
                        | StreamSource::Torrent { .. }
                )
        })
}

fn ranked_recovery_index(
    rows: &[StreamUi],
    selected: &SelectedStream,
    rules: &nova_config::StreamFilterRules,
    budget: Option<&source_recovery::RetryBudget>,
    start: usize,
) -> Option<usize> {
    let torrents_enabled = active_torrent_settings().enabled;
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            !selected.matches(row)
                && (!matches!(row.source, StreamSource::Torrent { .. }) || torrents_enabled)
                && rules.accepts(&row.card.filter_facts)
                && budget.is_none_or(|budget| {
                    budget.accepts(SelectedStream::from_row(row).retry_identity())
                })
                && matches!(
                    row.source,
                    StreamSource::Url(_)
                        | StreamSource::UrlWithOptions { .. }
                        | StreamSource::Resolver { .. }
                        | StreamSource::Torrent { .. }
                )
        })
        .min_by(|(a_index, a), (b_index, b)| {
            (if rules.enabled {
                rules.compare(&a.card.filter_facts, &b.card.filter_facts)
            } else {
                std::cmp::Ordering::Equal
            })
            .then_with(|| {
                source_recovery::verified(b.choice_id.as_deref())
                    .cmp(&source_recovery::verified(a.choice_id.as_deref()))
            })
            .then_with(|| {
                ((a_index + rows.len() - start) % rows.len())
                    .cmp(&((b_index + rows.len() - start) % rows.len()))
            })
        })
        .map(|(index, _)| index)
}

// A failed resolver has no worker authority. Keep only a display/source-selection
// snapshot, scoped to the exact detail opening that supplied its stream list.
#[derive(Clone)]
pub(super) struct RetryContext {
    pub(super) context: PlaybackContext,
    modal_token: Arc<()>,
}
/// Receiver-owned replacement context; never serialized or sent to a controller.
pub(super) struct RemoteRetry {
    context: PlaybackContext,
}
#[cfg(test)]
impl RemoteRetry {
    pub(super) fn retry_state(
        &self,
    ) -> (
        Option<f64>,
        bool,
        Option<nova_config::playback_preferences::SessionPreferences>,
    ) {
        (
            self.context.history_resume,
            self.context.bookmark_resume,
            self.context.retry_preferences.clone(),
        )
    }
}

pub(super) struct BookmarkResume {
    identity: nova_playback_api::MediaIdentity,
    position: f64,
    modal_token: Arc<()>,
}
impl BookmarkResume {
    fn matches(&self, modal: &ModalItem) -> bool {
        Arc::ptr_eq(&self.modal_token, &modal.open_token)
            && self.identity.media_type == modal.type_
            && self.identity.title_id == modal.id
            && self.identity.episode_id == modal.request_id
    }
}

impl RetryContext {
    fn matches(&self, modal: &ModalItem) -> bool {
        Arc::ptr_eq(&self.modal_token, &modal.open_token)
            && self.context.media_id == modal.id
            && self.context.media_type == modal.type_
            && self.context.request_id == modal.request_id
    }
}

impl PlaybackIntent {
    pub(super) fn active_local(&self) -> bool {
        self.in_app && self.token.is_current()
    }
    fn accepts(&self, token: &crate::torrent::PlaybackRequest, modal: Option<&ModalItem>) -> bool {
        self.token.same_request(token)
            && token.is_current()
            && (self.detached
                || !self.pending
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
        bridge.refresh_sleep_timer();
        bridge.party_suspend();
        bridge.remote_suspend();
        bridge.refresh_playback_end_menu();
        bridge.cancel_automatic_source_retry();
        bridge.cancel_pending_playback();
        bridge.note_player_progress_from_ui();
        bridge.persist_playback_checkpoint(false);
    }
    persistence::flush_report();
}

impl Bridge {
    /// Snapshot the actual playback intent, not whichever detail page is open.
    pub(super) fn playback_policy_source_allowed(&self) -> bool {
        let state = self.shared.lock().unwrap();
        let owner = profiles::ProfileStore::capture();
        state.playback_intent.as_ref().is_none_or(|i| {
            i.context.intro_download.as_ref().map_or_else(
                || {
                    i.context.selected_stream.as_ref().map_or_else(
                        || !profile_policy::restricted(),
                        |s| profile_policy::allows_addon(&owner, &s.owner),
                    )
                },
                |j| smart_offline::allows_download(&owner, j),
            )
        })
    }
    pub(super) fn current_playback_identity(&self) -> Option<nova_playback_api::MediaIdentity> {
        let state = self.shared.lock().unwrap();
        let context = &state.playback_intent.as_ref()?.context;
        if context.media_id.is_empty() || context.request_id.is_empty() {
            return None;
        }
        Some(nova_playback_api::MediaIdentity {
            media_type: context.media_type.clone(),
            title_id: context.media_id.clone(),
            episode_id: context.request_id.clone(),
            title: context.title.clone(),
            episode: context.episode_label.clone(),
        })
    }

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
        if app.get_osd_visible() {
            app.invoke_osd_mouse_moved();
        }
        app.set_screenshot_busy(true);
        {
            let mut state = self.shared.lock().unwrap();
            state.remote_playback.screenshot_saved = false;
            state.remote_playback.screenshot_failed = false;
        }
        app.set_screenshot_notice(SharedString::default());
        let bridge = self.clone();
        crate::app::profiles::thread::spawn(move || {
            let result = bridge
                .player
                .capture_screenshot_with_context(session, context);
            let succeeded = result.is_ok();
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                if !app.get_player_open() || bridge.player.screenshot_session() != session {
                    return;
                }
                app.set_screenshot_busy(false);
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .screenshot_saved = succeeded;
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .remote_playback
                    .screenshot_failed = !succeeded;
                let notice = match result {
                    Ok(saved) => text::screenshot_saved(&saved.display_path),
                    Err(error) => text::tr(&error.to_string()).to_owned(),
                };
                app.set_screenshot_notice(notice.clone().into());
                if app.get_osd_visible() {
                    app.invoke_osd_mouse_moved();
                }
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_secs(5), move || {
                    if let Some(app) = weak.upgrade()
                        && bridge.player.screenshot_session() == session
                        && !app.get_screenshot_busy()
                        && app.get_screenshot_notice().as_str() == notice
                    {
                        app.set_screenshot_notice(SharedString::default());
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .remote_playback
                            .screenshot_saved = false;
                        bridge
                            .shared
                            .lock()
                            .unwrap()
                            .remote_playback
                            .screenshot_failed = false;
                    }
                });
            });
        });
    }

    pub(super) fn capture_remote_retry(&self) -> Option<RemoteRetry> {
        let app = self.app()?;
        let preferences = self.player.retry_preferences();
        let mut context = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()?
            .context
            .clone();
        let position = if app.get_playback_started() {
            app.get_position() as f64
        } else {
            context.history_resume.unwrap_or(0.0)
        };
        if position.is_finite() {
            context.history_resume = Some(position.max(0.0));
        }
        if app.get_playback_started() || context.retry_preferences.is_none() {
            context.retry_preferences = Some(preferences);
        }
        context.bookmark_resume = true; // Explicit recovery seeks never ask the ordinary resume question.
        Some(RemoteRetry { context })
    }

    pub(super) fn begin_playback_request(&self) -> crate::torrent::PlaybackRequest {
        if profile_policy::entry_blocked() {
            let rejected = crate::torrent::PlaybackRequest::default();
            rejected.cancel();
            return rejected;
        }
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            let rejected = crate::torrent::PlaybackRequest::default();
            rejected.cancel();
            return rejected;
        }
        let (retry, bookmark, selected_stream) = {
            let mut state = self.shared.lock().unwrap();
            if state.remote_playback.start_context.is_some() {
                // Receiver-owned starts cannot inherit an unrelated Detail
                // opening's retry, scene seek, or last clicked source row.
                (
                    state
                        .remote_playback
                        .start_retry
                        .take()
                        .map(|retry| retry.context),
                    None,
                    None,
                )
            } else {
                let explicit_selection = state.playback_tools.pending_retry.is_none()
                    && state.playback_tools.pending_stream_selection.is_some();
                let retry = state
                    .playback_tools
                    .pending_retry
                    .take()
                    .or_else(|| state.playback_tools.failed_retry.clone())
                    .or_else(|| {
                        state
                            .playback_intent
                            .as_ref()
                            .filter(|intent| intent.context.bookmark_resume)
                            .and_then(|intent| {
                                Some(RetryContext {
                                    context: intent.context.clone(),
                                    modal_token: intent.modal_token.clone()?,
                                })
                            })
                    });
                let retry = retry
                    .filter(|retry| {
                        state
                            .modal_item
                            .as_ref()
                            .is_some_and(|modal| retry.matches(modal))
                    })
                    .map(|retry| {
                        let mut context = retry.context;
                        if explicit_selection {
                            // A new deliberate source choice starts a new run;
                            // a failed automatic attempt cannot spend its budget.
                            context.retry_budget = Default::default();
                            context.automatic_source_selection = false;
                        }
                        context
                    });
                let bookmark = state
                    .playback_tools
                    .bookmark_resume
                    .take()
                    .filter(|bookmark| {
                        state
                            .modal_item
                            .as_ref()
                            .is_some_and(|modal| bookmark.matches(modal))
                    });
                let selected_stream = state.playback_tools.pending_stream_selection.take();
                (retry, bookmark, selected_stream)
            }
        };
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
        state.casting.capture_destination(&token);
        let remote_context = state.remote_playback.start_context.take();
        state.remote_playback.begin();
        if let Some(media) = &remote_context {
            state.remote_playback.receiver_videos =
                read_episodes_cache_for(&media.media_type, &media.title_id).unwrap_or_default();
        }
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
            detached: remote_context.is_some(),
            context: remote_context
                .map(|media| PlaybackContext {
                    title: media.title,
                    episode_label: media.episode.clone(),
                    osd_episode_title: media.episode,
                    episode_number: state
                        .remote_playback
                        .receiver_videos
                        .iter()
                        .find(|video| video.id == media.episode_id)
                        .map(episode_se_label)
                        .unwrap_or_default(),
                    media_type: media.media_type.clone(),
                    media_id: media.title_id,
                    request_id: media.episode_id,
                    is_episode: media.media_type != "movie",
                    is_movie: media.media_type == "movie",
                    ..Default::default()
                })
                .unwrap_or_else(|| {
                    state
                        .modal_item
                        .as_ref()
                        .map(|modal| PlaybackContext {
                            title: modal.name.clone(),
                            media_summary: std::iter::once(modal.year.as_str())
                                .chain(modal.genres.iter().take(2).map(String::as_str))
                                .filter(|value| !value.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · "),
                            episode_label,
                            osd_episode_title,
                            episode_number,
                            use_modal_poster: true,
                            logo_url: modal.logo_url.clone(),
                            media_type: modal.type_.clone(),
                            media_id: modal.id.clone(),
                            request_id: modal.request_id.clone(),
                            // A History deep link can supply its saved exact
                            // episode while the full season metadata reloads.
                            is_episode: modal.type_ != "movie"
                                && (!modal.seasons.is_empty()
                                    || modal.videos.iter().any(|v| v.id == modal.request_id)),
                            is_movie: modal.type_ == "movie",
                            ..Default::default()
                        })
                        .unwrap_or_default()
                }),
        });
        let mut context = state.playback_intent.as_ref().unwrap().context.clone();
        context.subtitle_hints = std::mem::take(&mut state.pending_subtitle_hints);
        let modal = state
            .modal_item
            .as_ref()
            .filter(|modal| modal.id == context.media_id && modal.type_ == context.media_type);
        let detached = state.playback_intent.as_ref().unwrap().detached;
        let video = if detached {
            state
                .remote_playback
                .receiver_videos
                .iter()
                .find(|v| v.id == context.request_id)
        } else {
            modal.and_then(|modal| modal.videos.iter().find(|v| v.id == context.request_id))
        };
        let poster_url = if detached {
            None
        } else {
            modal.map(|m| m.poster_url.clone())
        }
        .filter(|url| !url.is_empty())
        .or_else(|| {
            state
                .entries
                .iter()
                .find(|entry| entry.id == context.media_id && entry.type_ == context.media_type)
                .map(|entry| entry.poster_url.clone())
        })
        .or_else(|| {
            detail::read_meta_header_for(&context.media_type, &context.media_id)
                .map(|header| header.poster_url)
        })
        .unwrap_or_default();
        let history_hidden: HashMap<String, u64> =
            read_json(watch_history::HIDDEN_KEY).unwrap_or_default();
        let hidden_at = history_hidden.get(&watch_history::title_key(
            &context.media_type,
            &context.media_id,
        ));
        context.history = Some(PlaybackHistory {
            type_: context.media_type.clone(),
            title: context.title.clone(),
            poster_url,
            episode_label: video
                .map(episode_context_label)
                .unwrap_or_else(|| context.episode_label.clone()),
            season: video.and_then(|video| video.season),
            episode: video.and_then(Video::episode_number),
            played_at_secs: state
                .progress
                .get(&progress_map_key(&context.media_id, &context.request_id))
                .and_then(|record| record.history.as_ref())
                .map_or(0, |history| history.played_at_secs),
            engaged_secs: Some(
                state
                    .progress
                    .get(&progress_map_key(&context.media_id, &context.request_id))
                    .filter(|record| {
                        playback_can_resume(record)
                            && hidden_at.is_none_or(|hidden_at| {
                                record
                                    .history
                                    .as_ref()
                                    .is_some_and(|history| history.played_at_secs > *hidden_at)
                            })
                    })
                    .map_or(0.0, |record| {
                        record
                            .history
                            .as_ref()
                            .and_then(|history| history.engaged_secs)
                            .unwrap_or(watch_history::MEANINGFUL_PLAYBACK_SECS)
                    }),
            ),
            rewatch: state
                .progress
                .get(&progress_map_key(&context.media_id, &context.request_id))
                .is_some_and(|p| p.watched),
        });
        context.history_resume = (!detached)
            .then(|| {
                state
                    .history_selection
                    .as_ref()
                    .zip(state.modal_item.as_ref())
                    .filter(|(selection, modal)| selection.matches(modal))
                    .map(|(selection, _)| selection.resume_position())
            })
            .flatten();
        if state.playback.as_ref().is_some_and(|target| {
            target.force_start_over
                && target.series_id == context.media_id
                && target.episode_id == context.request_id
        }) {
            context.history_resume = Some(0.0);
        }
        if let Some(retry) = retry {
            context = retry;
        }
        if let Some(bookmark) = bookmark {
            context.history_resume = Some(bookmark.position);
            context.bookmark_resume = true;
        }
        context.selected_stream = selected_stream;
        context.policy_revision = profile_policy::revision(&profiles::ProfileStore::capture());
        if context.recovery_timeline.is_some() {
            state.playback_intent.as_mut().unwrap().private_playback = true;
        }

        state.playback_intent.as_mut().unwrap().context = context.clone();
        drop(state);
        self.queue_note_playback_start();
        self.sleep_timer_start(&nova_playback_api::MediaIdentity {
            media_type: context.media_type.clone(),
            title_id: context.media_id.clone(),
            episode_id: context.request_id.clone(),
            ..Default::default()
        });
        self.publish_player_context(&context);
        token
    }

    pub(super) fn arm_bookmark_resume(
        &self,
        identity: nova_playback_api::MediaIdentity,
        position: f64,
    ) {
        if !position.is_finite() || position < 0.0 {
            return;
        }
        let mut state = self.shared.lock().unwrap();
        let Some(modal) = state
            .modal_item
            .as_ref()
            .filter(|modal| modal.id == identity.title_id && modal.type_ == identity.media_type)
        else {
            return;
        };
        let modal_token = modal.open_token.clone();
        state.playback_tools.bookmark_resume = Some(BookmarkResume {
            identity,
            position,
            modal_token,
        });
    }

    /// Consume only a saved scene belonging to the exact selector opening and
    /// media whose receiver-side offers are being picked. Never leave it armed
    /// for a later local selection after handing ownership to the controller.
    pub(super) fn take_remote_bookmark_resume(
        &self,
        media: &nova_playback_api::MediaIdentity,
    ) -> Option<f64> {
        let mut state = self.shared.lock().unwrap();
        let bookmark = state.playback_tools.bookmark_resume.take()?;
        (state
            .modal_item
            .as_ref()
            .is_some_and(|modal| bookmark.matches(modal))
            && bookmark.identity.media_type == media.media_type
            && bookmark.identity.title_id == media.title_id
            && bookmark.identity.episode_id == media.episode_id)
            .then_some(bookmark.position)
    }

    pub(super) fn note_selected_playback_stream(&self, stream: &StreamUi) {
        let automatic = self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .pending_retry
            .as_ref()
            .is_some_and(|retry| retry.context.automatic_source_selection);
        if !automatic {
            self.queue_note_selected_stream(stream);
        }
        self.party_remember_source(stream);
        self.shared
            .lock()
            .unwrap()
            .playback_tools
            .pending_stream_selection = Some(SelectedStream::from_row(stream));
    }

    pub(super) fn alternate_stream_index(&self) -> Option<usize> {
        let app = self.app()?;
        if !app.get_player_open() || self.remote_receiver_playback() {
            return None;
        }
        let rules = self.effective_stream_filter_rules();
        let state = self.shared.lock().unwrap();
        if !state.remote_playback.can_play_locally() {
            return None;
        }
        let modal = state.modal_item.as_ref()?;
        let active = state.playback_intent.as_ref().is_some_and(|intent| {
            intent.in_app
                && intent.token.is_current()
                && modal.id == intent.context.media_id
                && modal.type_ == intent.context.media_type
                && modal.request_id == intent.context.request_id
                && intent
                    .modal_token
                    .as_ref()
                    .is_some_and(|token| Arc::ptr_eq(token, &modal.open_token))
        });
        let failed = state
            .playback_tools
            .failed_retry
            .as_ref()
            .is_some_and(|retry| retry.matches(modal));
        if !active && !failed {
            return None;
        }
        let selected = state
            .playback_intent
            .as_ref()
            .and_then(|intent| intent.context.selected_stream.as_ref())
            .or_else(|| {
                state
                    .playback_tools
                    .failed_retry
                    .as_ref()
                    .and_then(|retry| retry.context.selected_stream.as_ref())
            })?;
        let fallback = alternate_stream_index(&state.streams, selected)?;
        ranked_recovery_index(&state.streams, selected, &rules, None, fallback)
    }

    pub(super) fn try_another_stream(&self) {
        self.retry_another_stream(false);
    }

    pub(super) fn retry_another_stream(&self, automatic: bool) {
        let Some(mut index) = self.alternate_stream_index() else {
            return;
        };
        let Some(app) = self.app() else {
            return;
        };
        let preferences = if app.get_playback_started() {
            self.player.retry_preferences()
        } else {
            self.prepare_playback_preferences()
        };
        let party_active = self.party_active();
        let rules = self.effective_stream_filter_rules();
        let mut state = self.shared.lock().unwrap();
        let context = state
            .playback_intent
            .as_ref()
            .map(|intent| intent.context.clone())
            .or_else(|| {
                state
                    .playback_tools
                    .failed_retry
                    .as_ref()
                    .map(|retry| retry.context.clone())
            });
        let Some(mut context) = context else {
            return;
        };
        context.automatic_source_selection = automatic;
        if automatic {
            if let Some(selected) = &context.selected_stream {
                context
                    .retry_budget
                    .exclude_original(selected.retry_identity());
            }
            index = match context.selected_stream.as_ref().and_then(|selected| {
                ranked_recovery_index(
                    &state.streams,
                    selected,
                    &rules,
                    Some(&context.retry_budget),
                    index,
                )
            }) {
                Some(index) => index,
                None => return,
            };
            let row = &state.streams[index];
            if !context
                .retry_budget
                .attempt(SelectedStream::from_row(row).retry_identity())
            {
                return;
            }
        } else {
            // Manual recovery is always available, including after the bounded
            // automatic run was cancelled or exhausted.
            context.retry_budget.cancelled = true;
        }
        let Some(modal) = state.modal_item.as_ref() else {
            return;
        };
        let modal_token = modal.open_token.clone();
        let position = if app.get_playback_started() {
            app.get_position() as f64
        } else {
            context
                .history_resume
                .or_else(|| state.playback.as_ref().and_then(|target| target.resume_pos))
                .unwrap_or(0.0)
        };
        if (app.get_playback_started() || context.recovery_timeline.is_none())
            && position.is_finite()
            && position > 0.0
            && !party_active
        {
            context.recovery_timeline = Some(source_recovery::Timeline {
                position,
                duration: app.get_duration() as f64,
                source: context
                    .selected_stream
                    .as_ref()
                    .and_then(|selected| selected.continuity.clone()),
            });
        }
        context.history_resume = Some(if position.is_finite() {
            position.max(0.0)
        } else {
            0.0
        });
        if app.get_playback_started() || context.retry_preferences.is_none() {
            context.retry_preferences = Some(preferences);
        }
        state.playback_tools.pending_retry = Some(RetryContext {
            context,
            modal_token,
        });
        drop(state);
        self.clear_playback_end_menu();
        self.stream_picked(index);
    }

    pub(super) fn playback_request_is_current(
        &self,
        token: &crate::torrent::PlaybackRequest,
    ) -> bool {
        if profile_policy::entry_blocked() {
            return false;
        }
        if self.player.sleep_deadline_expired() {
            return false;
        }
        let state = self.shared.lock().unwrap();
        if state.playback_intent.as_ref().is_some_and(|intent| {
            intent.pending
                && intent.context.automatic_source_selection
                && (!remote_playback::is_foreground()
                    || intent.context.retry_budget.cancelled
                    || !state.cache_settings.auto_retry_streams
                    || state.cache_settings.uses_external_player()
                    || state.casting.active
                    || state.casting.loading
                    || state.party.active()
                    || !state.remote_playback.can_play_locally())
        }) {
            token.cancel();
            return false;
        }
        let owner = profiles::ProfileStore::capture();
        if state.playback_intent.as_ref().is_some_and(|i| {
            i.context.policy_revision != profile_policy::revision(&owner)
                || !profile_policy::allows_title(&owner, &i.context.media_type, &i.context.media_id)
                || i.context
                    .intro_download
                    .as_ref()
                    .is_some_and(|j| !smart_offline::allows_download(&owner, j))
                || (i.context.intro_download.is_none()
                    && i.context
                        .selected_stream
                        .as_ref()
                        .map_or_else(profile_policy::restricted, |s| {
                            !profile_policy::allows_addon(&owner, &s.owner)
                        }))
        }) {
            return false;
        }
        state
            .playback_intent
            .as_ref()
            .is_some_and(|intent| intent.accepts(token, state.modal_item.as_ref()))
    }

    pub(super) fn finish_playback_request(&self) {
        self.finish_viewing_journal();
        self.clear_playback_end_menu();
        self.playback_closed();
        {
            let mut state = self.shared.lock().unwrap();
            state.playback_tools.recovery_prompt = Default::default();
            state.playback_tools.success_progress = Default::default();
            state.playback_tools.guarded_resume = None;
            state.playback_tools.failed_retry = None;
            state.playback_tools.pending_retry = None;
            state.playback_tools.bookmark_resume = None;
            state.playback_tools.pending_stream_selection = None;
            if let Some(intent) = state.playback_intent.take() {
                intent.token.cancel();
            }
            state.pending_resume_stream = None;
            state.resume_prompt_choice = None;
            state.active_torrent = None;
            state.casting.clear_source();
        }
        if let Some(app) = self.app() {
            app.set_resume_prompt_visible(false);
            app.set_resume_source_changed(false);
            app.set_automatic_stream_recovery_visible(false);
        }
        if let Some(engine) = crate::torrent::engine() {
            engine.on_playback_stopped(&active_torrent_settings());
        }
    }

    /// Host browsing must not cancel a receiver-owned loading request.
    pub(super) fn cancel_pending_for_navigation(&self) {
        self.cancel_automatic_source_retry();
        if !self.remote_receiver_playback() {
            self.cancel_pending_playback();
        }
    }

    fn cancel_automatic_source_retry(&self) {
        let mut state = self.shared.lock().unwrap();
        if let Some(intent) = state.playback_intent.as_mut() {
            intent.context.retry_budget.cancelled = true;
        }
        if let Some(retry) = state.playback_tools.failed_retry.as_mut() {
            retry.context.retry_budget.cancelled = true;
        }
    }

    pub(super) fn cancel_pending_playback(&self) {
        if self.shared.lock().unwrap().casting.loading {
            return;
        }
        let (pending, visible) = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .map(|intent| (intent.pending, intent.pending && intent.in_app))
            .unwrap_or_default();
        if pending {
            self.finish_playback_request();
            if visible {
                // Retiring a candidate request may leave older playback live.
                // Cancel its timer only when this path actually closes it.
                self.cancel_sleep_timer();
                self.player.close();
            }
        }
    }

    pub(super) fn mark_playback_opened(&self, in_app: bool) {
        if !in_app {
            self.preserve_queue_on_close(false);
            self.cancel_queue_run();
            self.cancel_sleep_timer();
            self.playback_closed();
        }
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

    /// Android volume gesture changes the receiver music-stream level.
    #[cfg(target_os = "android")]
    pub(super) fn android_volume_step(&self, _dir: i32) {
        #[cfg(target_os = "android")]
        android_player::player_volume_step(_dir);
    }

    /// Update the default mirror; the caller persists the setting.
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

    /// Persist active progress and refresh the dependent library rows.
    pub(super) fn persist_and_refresh_progress(&self) {
        // The gesture's visible feedback precedes map snapshots, persistence
        // enqueueing and the offscreen Library/Home metadata joins.
        self.refresh_detail_progress_ui();
        self.refresh_watch_queue_progress();
        let map = self.shared.lock().unwrap().progress.clone();
        write_progress_map(&map);
        self.request_progress_projection();
    }

    /// Repaint progress-derived surfaces outside the detail page: library
    /// badges/checks plus Home's Continue Watching, New episodes and Upcoming rows. Called
    /// after progress writes and whenever fresh episode meta lands (which
    /// may reveal new released episodes without starting a viewing run).
    /// Lightweight: no thumbnail re-queue.
    pub(super) fn refresh_library_progress_ui(&self) {
        // Legacy synchronous callers may rebuild after a metadata/settings
        // change while an earlier watched-action worker is still preparing.
        super::progress_projection::invalidate();
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
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return;
        }
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
        if !request.is_current() {
            return;
        }
        if self.remote_receiver_playback() {
            self.open_provider_loading(&request);
        }
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
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
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
                if self.external_playback_allowed() {
                    // User choice (Settings → Player): external player,
                    // always. Not tracked: there is no close event.
                    self.mark_playback_opened(false);
                    if !self.remote_receiver_playback() && !self.party_active() {
                        let _ = crate::player::open_external(&ready.url);
                    }
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
                    // Android can use an external app as a fallback.
                    // Not tracked: there is no close event.
                    self.mark_playback_opened(false);
                    if !self.remote_receiver_playback()
                        && !self.party_active()
                        && crate::player::open_external(&ready.url).is_err()
                    {
                        self.set_stream_hint(Some(StreamHint::Fixed(
                            "This stream requires the in-app player, which is unavailable.",
                        )));
                    }
                    self.finish_playback_request();
                }
            }
            Err(e) => {
                self.finish_playback_request();
                self.set_stream_hint(Some(StreamHint::P2pUnavailable(e.to_string())));
            }
        }
    }

    /// Open the in-app mpv player for `url` (replacing any previous player
    /// window). Returns false when the player can't be created (e.g. mpv or
    /// the window can't be initialized), so callers can fall back to an
    /// external player on Android (ACTION_VIEW). Only ever
    /// runs on the main/UI thread.
    pub(super) fn remote_receiver_playback(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .is_some_and(|i| i.detached)
    }

    pub(super) fn recovery_source_selected(&self) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .playback_tools
            .pending_retry
            .as_ref()
            .is_some_and(|retry| retry.context.recovery_timeline.is_some())
            || state
                .playback_intent
                .as_ref()
                .is_some_and(|intent| intent.context.recovery_timeline.is_some())
    }

    fn external_playback_allowed(&self) -> bool {
        nova_config::active_cache_settings().uses_external_player()
            && !self.remote_receiver_playback()
            && !self.party_active()
            && !self.recovery_source_selected()
            && !self.shared.lock().unwrap().casting.selected
    }

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
        if !profile_policy::allows_addon(&profiles::ProfileStore::capture(), &provider_url) {
            return;
        }
        let request = self.begin_playback_request();
        if !request.is_current() {
            return;
        }
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
        self.set_stream_hint(None);
        if self.external_playback_allowed() {
            self.set_stream_hint(Some(StreamHint::Fixed("Resolving stream…")));
        } else {
            self.open_provider_loading(&request);
        }
        crate::app::profiles::thread::spawn(move || {
            let result = nova_providers::resolve_provider_stream(
                &provider_url,
                &token,
                Duration::from_secs(30),
                cancelled.clone(),
            );
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge.complete_provider_resolution(request, cancelled(), result);
            });
        });
    }

    fn complete_provider_resolution(
        &self,
        request: crate::torrent::PlaybackRequest,
        selection_is_stale: bool,
        result: Result<addons::Stream, nova_providers::ProviderResolveError>,
    ) {
        if !self.playback_request_is_current(&request) {
            return;
        }

        if selection_is_stale {
            self.provider_resolution_failed(
                &request,
                "This stream selection is out of date. Reload streams.".into(),
            );
            return;
        }
        if result.is_ok()
            && let Some(app) = self.app()
            && app.get_player_open()
            && !app.get_playback_started()
        {
            app.set_player_status(text::tr("Loading stream…").into());
        }
        let private_playback = self.remote_receiver_playback()
            || self.recovery_source_selected()
            || self.party_active()
            || result.as_ref().is_ok_and(|stream| {
                stream
                    .extra
                    .get("novaSensitivePlayback")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            });
        if let Some(intent) = self.shared.lock().unwrap().playback_intent.as_mut() {
            intent.private_playback = private_playback;
        }
        match result.map(|stream| stream_source(&stream)) {
            Ok(StreamSource::Url(url)) => {
                self.set_stream_hint(None);
                if !private_playback && self.external_playback_allowed() {
                    self.player.close();
                    self.mark_playback_opened(false);
                    let _ = crate::player::open_external(&url);
                } else if !self.open_requested_stream(
                    request.clone(),
                    url.clone(),
                    Vec::new(),
                    Vec::new(),
                ) {
                    if private_playback
                        || self.remote_receiver_playback()
                        || !cfg!(target_os = "android")
                    {
                        self.provider_resolution_failed(
                            &request,
                            "This stream requires the in-app player, which is unavailable.".into(),
                        );
                    } else {
                        self.player.close();
                        self.mark_playback_opened(false);
                        if !self.remote_receiver_playback() {
                            let _ = crate::player::open_external(&url);
                        }
                    }
                }
            }
            Ok(StreamSource::UrlWithOptions {
                url,
                headers,
                subtitles,
            }) => {
                if !self.open_requested_stream(request.clone(), url, headers, subtitles) {
                    self.provider_resolution_failed(
                        &request,
                        "This stream requires the in-app player, which is unavailable.".into(),
                    );
                } else {
                    self.set_stream_hint(None);
                }
            }
            Err(error) => {
                self.provider_resolution_failed(&request, error.message);
            }
            _ => {
                self.provider_resolution_failed(&request, "Could not resolve this stream. Try another result or check the addon configuration.".into());
            }
        }
    }

    pub(super) fn open_player_with_options(
        &self,
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    ) -> bool {
        let request = self.begin_playback_request();
        if !request.is_current() {
            return false;
        }
        self.open_requested_stream(request, url, headers, subtitles)
    }

    pub(super) fn play_download(&self, id: &str) {
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return;
        }
        let Some(job) = self
            .downloads
            .job(id)
            .filter(|job| job.phase == crate::download::DownloadPhase::Completed)
            .filter(|job| smart_offline::allows_download(&profiles::ProfileStore::capture(), job))
        else {
            return;
        };
        let Some(path) = job.artifact_path.as_ref().filter(|path| path.is_file()) else {
            return;
        };
        let request = self.begin_playback_request();
        if !request.is_current() {
            return;
        }
        let episode = read_episodes_cache_for(&job.media_type, &job.media_id)
            .and_then(|videos| videos.into_iter().find(|v| v.id == job.request_id));
        if let Some(scope) = stream_series_scope(&job.media_type, &job.media_id) {
            self.last_stream.remember_series(
                &scope,
                StreamPreference::from_download(&job),
                nova_config::now_secs(),
            );
        }
        let saved_coordinates = job
            .intro_request
            .as_ref()
            .map(|request| request.coordinates());
        let mut state = self.shared.lock().unwrap();
        let rewatch = state
            .progress
            .get(&progress_map_key(&job.media_id, &job.request_id))
            .is_some_and(|p| p.watched);
        let force_start_over = state.playback.as_ref().is_some_and(|target| {
            target.force_start_over
                && target.series_id == job.media_id
                && target.episode_id == job.request_id
        });
        let explicit_context = state
            .playback_intent
            .as_ref()
            .map(|intent| intent.context.clone())
            .filter(|context| {
                context.media_type == job.media_type
                    && context.media_id == job.media_id
                    && context.request_id == job.request_id
            })
            .unwrap_or_default();
        if let Some(intent) = state.playback_intent.as_mut() {
            intent.context = PlaybackContext {
                policy_revision: profile_policy::revision(&profiles::ProfileStore::capture()),
                intro_download: Some(job.clone()),
                history_resume: explicit_context
                    .history_resume
                    .or_else(|| force_start_over.then_some(0.0)),
                bookmark_resume: explicit_context.bookmark_resume,
                retry_preferences: explicit_context.retry_preferences,
                selected_stream: explicit_context.selected_stream,
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
                is_episode: job.media_type == "series" || saved_coordinates.is_some(),
                is_movie: job.media_type == "movie",
                history: Some(PlaybackHistory {
                    type_: job.media_type.clone(),
                    title: job.title.clone(),
                    poster_url: detail::read_meta_header_for(&job.media_type, &job.media_id)
                        .map(|header| header.poster_url)
                        .unwrap_or_default(),
                    episode_label: episode
                        .as_ref()
                        .map(episode_context_label)
                        .unwrap_or_default(),
                    season: episode
                        .as_ref()
                        .and_then(|video| video.season)
                        .or(saved_coordinates.map(|(season, _)| season)),
                    episode: episode
                        .as_ref()
                        .and_then(Video::episode_number)
                        .or(saved_coordinates.map(|(_, episode)| episode)),
                    rewatch,
                    ..Default::default()
                }),
                ..Default::default()
            };
        }
        drop(state);
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
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    if bridge.playback_request_is_current(&callback_request) {
                        if !bridge.open_player_for_request(
                            callback_request,
                            url.clone(),
                            headers,
                            subtitles,
                        ) {
                            bridge.mark_playback_opened(false);
                            if !bridge.remote_receiver_playback()
                                && !bridge.party_active()
                                && !bridge.recovery_source_selected()
                                && crate::player::open_external(&url).is_err()
                            {
                                bridge.set_stream_hint(Some(StreamHint::Fixed(
                                    "This stream requires the in-app player, which is unavailable.",
                                )));
                            }
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
            let opened = self.open_player_for_request(request, url, headers, subtitles);
            if !opened && self.remote_receiver_playback() {
                self.finish_playback_request();
            }
            opened
        }
    }

    /// Save the last observation before another episode replaces its target.
    pub(super) fn persist_outgoing_playback(&self) {
        self.finish_viewing_journal();
        self.persist_playback_checkpoint(true);
    }

    // Android suspension also checkpoints the active player. PiP keeps that
    // session playing, so persistence alone must never emit a terminal scrobble.
    pub(super) fn persist_playback_checkpoint(&self, stopped: bool) {
        persistence::flush_report();
        self.flush_playback_preferences(true);
        let outgoing = self.shared.lock().unwrap().playback.clone();
        if let Some(target) = outgoing.filter(|_| stopped) {
            self.list_services_playback(&target, target.last_pos, target.last_dur, false, true);
        }
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
                stamp_playback_history(entry, &target.history);
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
            self.request_progress_projection();
        }
        nova_sync::flush_local_sync();
        self.stremio_checkpoint();
    }

    pub(super) fn publish_player_context(&self, context: &PlaybackContext) {
        // Remote and offline playback can bypass the episode picker. Apply
        // the local profile policy again at the final OSD/OS-title boundary.
        let mut safe_context = context.clone();
        let protected = {
            let state = self.shared.lock().unwrap();
            spoiler::hidden(
                &state,
                &context.media_type,
                &context.media_id,
                &context.request_id,
            )
        };
        if protected {
            let number = if context.episode_number.is_empty() {
                text::tr("Episode").to_string()
            } else {
                context.episode_number.clone()
            };
            safe_context.episode_label = number.clone();
            safe_context.osd_episode_title = number;
        }
        let context = &safe_context;
        let request = self
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .map(|intent| intent.token.clone());
        self.load_player_title_logo(context.logo_url.clone(), request);
        if let Some(app) = self.app() {
            app.set_player_episode_title(context.osd_episode_title.clone().into());
            app.set_player_show_title(context.title.clone().into());
            app.set_player_media_summary(context.media_summary.clone().into());
            app.set_screenshot_busy(false);
            app.set_screenshot_notice(SharedString::default());
        }
        let ep = (!context.episode_label.is_empty()).then_some(context.episode_label.as_str());
        let display_title = match ep {
            Some(ep) => format!("{} · {ep}", context.title),
            None => context.title.clone(),
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
    }

    fn open_provider_loading(&self, request: &crate::torrent::PlaybackRequest) {
        if self.shared.lock().unwrap().casting.selected {
            return;
        }
        if !self.playback_request_is_current(request)
            || !self.player.wait_for_source("Resolving stream…")
        {
            return;
        }
        let context = {
            let mut state = self.shared.lock().unwrap();
            let intent = state.playback_intent.as_mut().unwrap();
            intent.in_app = true;
            intent.context.clone()
        };
        self.clear_next_episode_prompt();
        self.publish_player_context(&context);
    }

    pub(super) fn provider_resolution_failed(
        &self,
        request: &crate::torrent::PlaybackRequest,
        message: String,
    ) {
        if !self.playback_request_is_current(request) {
            return;
        }
        let receiver_owned = {
            let mut state = self.shared.lock().unwrap();
            state.playback_intent.as_mut().is_some_and(|intent| {
                if !intent.detached {
                    return false;
                }
                // Revoke worker authority while preserving the visible failed
                // session so its controller can inspect it and explicitly Stop.
                intent.token.cancel();
                intent.pending = false;
                intent.in_app = true;
                true
            })
        };
        if !receiver_owned {
            let preferences = self.prepare_playback_preferences();
            let retry = {
                let state = self.shared.lock().unwrap();
                state.playback_intent.as_ref().and_then(|intent| {
                    let mut context = intent.context.clone();
                    if context.retry_preferences.is_none() {
                        context.retry_preferences = Some(preferences);
                    }
                    context.history_resume = context
                        .history_resume
                        .or_else(|| state.playback.as_ref().and_then(|target| target.resume_pos));
                    Some(RetryContext {
                        context,
                        modal_token: intent.modal_token.clone()?,
                    })
                })
            };
            self.finish_playback_request();
            self.shared.lock().unwrap().playback_tools.failed_retry = retry;
            self.set_stream_hint(Some(StreamHint::ResolverFailure(message.clone())));
        }
        self.player.source_failed(&message);
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
        let (series_id, request_id, is_episode, is_movie) = (
            context.media_id.clone(),
            context.request_id.clone(),
            context.is_episode,
            context.is_movie,
        );
        // A party owns the timeline even while its snapshot is unavailable or
        // this source has not yet passed compatibility/readiness checks. Never
        // block its coordinator behind a local history prompt or resume seek.
        let party_active = self.party_active();
        let party_start = self.party_start_position();
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
                .filter(|p| playback_can_resume(p))
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
                intro_download: context.intro_download.clone(),
                resume_pos: if party_active {
                    party_start
                } else if let Some(position) = context.history_resume {
                    Some(position)
                } else {
                    match (behavior, resume_choice) {
                        (EpisodeStartBehavior::StartOver, _) | (_, Some(false)) => None,
                        (_, Some(true)) => history_pos,
                        (_, None) => keep_resume.or(resume_pos),
                    }
                },
                resume_done: party_active,
                history: context.history.clone(),
                ..Default::default()
            });
            if !party_active
                && context.history_resume.is_none()
                && ask
                && history_pos.is_some()
                && resume_choice.is_none()
            {
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
                .filter(|p| playback_can_resume(p))
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
                session: next_episode::new_session(),
                resume_pos: if party_active {
                    party_start
                } else {
                    context.history_resume.or(resume_pos)
                },
                resume_done: party_active,
                history: context.history.clone(),
                ..Default::default()
            });
        } else {
            self.shared.lock().unwrap().playback = None;
        }
        if context.recovery_timeline.is_some()
            && !party_active
            && let Some(target) = self.shared.lock().unwrap().playback.as_mut()
        {
            target.resume_pos = None;
            target.resume_done = true;
        }
        // Arming playback re-surfaces the item in Continue Watching right away:
        // drop any "removed" stamp now instead of waiting for the first
        // progress tick to outdate it (the `hidden_at >= updated_at` rule in
        // `rebuild_continue_list` only un-hides once progress actually moves).
        let armed = {
            let state = self.shared.lock().unwrap();
            state.playback.as_ref().map(|t| t.series_id.clone())
        };
        if armed.is_some() {
            let map = {
                let mut state = self.shared.lock().unwrap();
                home::clear_home_continue_hidden(&mut state)
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
        // load-time start) and disarms it once confirmed. During a party the
        // room coordinator owns any correction instead of local history.
        let start_at = {
            let state = self.shared.lock().unwrap();
            state
                .playback
                .as_ref()
                .and_then(|t| t.resume_pos)
                .unwrap_or(0.0)
        };

        self.publish_player_context(&context);
        if self.cast_source(&request, &url, &headers, &subtitles, start_at) {
            return true;
        }
        let preferences = context
            .retry_preferences
            .clone()
            .unwrap_or_else(|| self.prepare_playback_preferences());
        let allow_external = !self.remote_receiver_playback()
            && !self.party_active()
            && !self
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .is_some_and(|intent| intent.private_playback);
        let guarded = context.recovery_timeline.is_some() && !party_active;
        let queued = self.player.play_with_preferences_and_load_policy(
            &url,
            start_at,
            &headers,
            &subtitles,
            preferences,
            crate::player::PlaybackLoadPolicy {
                allow_external: allow_external && !guarded,
                paused: guarded,
            },
        );
        self.complete_player_queue(&context, queued)
    }

    /// Queue acceptance establishes the in-app intent, while the first-frame
    /// progress notification owns disarming its recovery preferences.
    fn complete_player_queue(&self, context: &PlaybackContext, queued: Result<(), String>) -> bool {
        match queued {
            Ok(()) => {
                self.mark_playback_opened(true);
                self.prepare_secondary_subtitles();
                self.start_addon_subtitles(
                    &context.media_type,
                    &context.request_id,
                    &context.subtitle_hints,
                );
                true
            }
            Err(e) => {
                eprintln!("nova: player unavailable: {e}");
                false
            }
        }
    }

    /// A replacement loads at zero before consulting its own native duration.
    /// Exact file evidence plus corresponding duration can retain the timeline;
    /// equal durations by themselves never identify a cut.
    fn check_recovery_timeline(&self) -> bool {
        if self.party_active() || self.remote_receiver_playback() {
            return false;
        }
        let Some(app) = self.app() else {
            return false;
        };
        let mut state = self.shared.lock().unwrap();
        if let Some(position) = state.playback_tools.guarded_resume {
            drop(state);
            self.player.set_paused(true);
            app.set_resume_source_can_resume(source_recovery::resume_in_bounds(
                position,
                app.get_duration() as f64,
            ));
            return true;
        }
        let Some(intent) = state.playback_intent.as_mut() else {
            return false;
        };
        if intent.context.recovery_timeline.is_none() {
            return false;
        }
        if !app.get_playback_started() {
            return true;
        }
        let timeline = intent.context.recovery_timeline.take().unwrap();
        let same_timeline = timeline.can_resume_without_question(
            intent
                .context
                .selected_stream
                .as_ref()
                .and_then(|selected| selected.continuity.as_deref()),
            app.get_duration() as f64,
        );
        if same_timeline {
            drop(state);
            self.player.release_start_pause();
            self.player.seek(timeline.position as f32);
            app.set_position(timeline.position as f32);
            self.player.set_paused(false);
        } else {
            state.playback_tools.guarded_resume = Some(timeline.position);
            drop(state);
            self.player.set_paused(true);
            app.set_resume_source_position(timeline.position as f32);
            app.set_resume_source_can_resume(source_recovery::resume_in_bounds(
                timeline.position,
                app.get_duration() as f64,
            ));
            app.set_resume_source_changed(true);
            app.set_resume_prompt_visible(true);
        }
        true
    }

    pub(super) fn resume_prompt_choice(&self, resume: bool) {
        let guarded_position = self.shared.lock().unwrap().playback_tools.guarded_resume;
        if resume
            && let Some(position) = guarded_position
            && self.app().is_none_or(|app| {
                !source_recovery::resume_in_bounds(position, app.get_duration() as f64)
            })
        {
            self.player.set_paused(true);
            if let Some(app) = self.app() {
                app.set_resume_source_can_resume(false);
            }
            return;
        }
        let guarded = self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .guarded_resume
            .take();
        if let Some(position) = guarded {
            if let Some(app) = self.app() {
                app.set_resume_prompt_visible(false);
                app.set_resume_source_changed(false);
                if app.get_player_open() {
                    let position = if resume { position } else { 0.0 };
                    self.player.release_start_pause();
                    self.player.seek(position as f32);
                    app.set_position(position as f32);
                    self.player.set_paused(false);
                }
            }
            return;
        }
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
        if self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .guarded_resume
            .take()
            .is_some()
        {
            self.finish_playback_request();
            self.player.close();
        }
        self.cancel_sleep_timer();
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
        if !app.get_player_open() || app.get_playback_started() || self.player.playback_failed() {
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
    pub(super) fn restore_cast_playback(
        &self,
        mut intent: PlaybackIntent,
        target: Option<PlaybackTarget>,
    ) -> crate::torrent::PlaybackRequest {
        let token = crate::torrent::PlaybackRequest::default();
        intent.token = token.clone();
        intent.pending = false;
        intent.in_app = true;
        intent.detached = false;
        let context = intent.context.clone();
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(old) = state.playback_intent.take() {
                old.token.cancel();
            }
            state.playback_intent = Some(intent);
            state.playback = target;
            state.pending_resume_stream = None;
            state.casting.capture_destination(&token);
        }
        self.publish_player_context(&context);
        token
    }

    pub(super) fn mark_cast_opened(&self) {
        if let Some(intent) = self.shared.lock().unwrap().playback_intent.as_mut() {
            intent.pending = false;
            intent.in_app = false;
        }
        static OBSERVATION: AtomicU64 = AtomicU64::new(1 << 63);
        self.shared.lock().unwrap().casting.observation_id =
            OBSERVATION.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn note_cast_progress(
        &self,
        expected: &mut PlaybackTarget,
        position: f64,
        duration: f64,
        paused: bool,
    ) {
        if !position.is_finite() || !duration.is_finite() || position < 0.0 || duration < 0.0 {
            return;
        }
        let map = record_cast_progress(
            &mut self.shared.lock().unwrap(),
            expected,
            position,
            duration,
            paused,
        );
        let target = expected.clone();
        self.note_journal_observation(
            nova_playback_api::MediaIdentity {
                media_type: target
                    .history
                    .as_ref()
                    .map(|h| h.type_.clone())
                    .unwrap_or_default(),
                title_id: target.series_id.clone(),
                episode_id: target.episode_id.clone(),
                title: target
                    .history
                    .as_ref()
                    .map(|h| h.title.clone())
                    .unwrap_or_default(),
                episode: target
                    .history
                    .as_ref()
                    .map(|h| h.episode_label.clone())
                    .unwrap_or_default(),
            },
            position,
            duration,
            !paused,
            1.,
        );
        if let Some(map) = map {
            nova_sync::playback_checkpoint(|| write_progress_map(&map));
            self.shared.lock().unwrap().casting.progress_dirty = true;
        }
        let observation_id = self.shared.lock().unwrap().casting.observation_id;
        self.list_services_playback_observation(
            &target,
            position,
            duration,
            paused,
            false,
            observation_id,
        );
    }

    pub(super) fn note_player_progress_from_ui(&self) {
        {
            let state = self.shared.lock().unwrap();
            if state.casting.active || state.casting.loading {
                return;
            }
        }
        let Some(app) = self.app() else {
            return;
        };
        if self.check_recovery_timeline() {
            return;
        }
        if app.get_player_open()
            && app.get_playback_started()
            && let Some(intent) = self.shared.lock().unwrap().playback_intent.as_mut()
        {
            intent.context.bookmark_resume = false;
            intent.context.retry_preferences = None;
        }
        self.note_viewing_journal();
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
            self.list_services_playback(
                &target,
                pos,
                dur,
                app.get_is_paused(),
                self.player.playback_readiness().ended,
            );
            observe_history_playback(&mut target, pos, app.get_is_paused());
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
                    t.history_observed_at = target.history_observed_at;
                    if let (Some(current), Some(observed)) =
                        (t.history.as_mut(), target.history.as_ref())
                    {
                        current.engaged_secs = observed.engaged_secs;
                        current.played_at_secs = observed.played_at_secs;
                    }
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
                stamp_playback_history(entry, &target.history);
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
            self.request_progress_projection();
            if newly_watched {
                self.auto_delete_watched_downloads(&target.series_id, &[target.episode_id.clone()]);
            }
            if ui_refresh {
                self.refresh_progress_ui();
            }
        } else {
            self.list_services_playback(&target, target.last_pos, target.last_dur, false, true);
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
                    stamp_playback_history(entry, &target.history);
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
            self.refresh_detail_progress_ui();
            self.request_progress_projection();
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
/// A genuine partial rewatch may resume without clearing the watched flag.
pub(super) fn playback_can_resume(progress: &EpisodeProgress) -> bool {
    progress.position_secs.is_finite()
        && progress.duration_secs.is_finite()
        && resumable_position(
            progress.position_secs,
            progress.duration_secs,
            progress.watched
                && !progress
                    .history
                    .as_ref()
                    .is_some_and(|history| history.rewatch),
        )
}

/// Receiver observations carry their original target even while a different source is resolving.
fn record_cast_progress(
    state: &mut Shared,
    expected: &mut PlaybackTarget,
    position: f64,
    duration: f64,
    paused: bool,
) -> Option<HashMap<String, EpisodeProgress>> {
    if !position.is_finite() || !duration.is_finite() || position < 0.0 || duration < 0.0 {
        return None;
    }
    let mut target = expected.clone();
    observe_history_playback(&mut target, position, paused);
    target.last_pos = position;
    target.last_dur = duration;
    target.resume_done = true;
    let now = now_secs();
    let due = (position - target.last_saved_pos).abs() >= PROGRESS_MIN_DELTA_SECS
        || (!paused && now.saturating_sub(target.last_saved_at) >= PROGRESS_SAVE_INTERVAL_SECS)
        || (paused && (position - target.last_saved_pos).abs() > 0.001);
    let key = progress_map_key(&target.series_id, &target.episode_id);
    let newly_watched = is_watched_position(position, duration)
        && !state.progress.get(&key).is_some_and(|p| p.watched);
    if due || newly_watched {
        target.last_saved_at = now;
        target.last_saved_pos = position;
        let entry = state
            .progress
            .entry(key)
            .or_insert_with(|| EpisodeProgress {
                series_id: target.series_id.clone(),
                episode_id: target.episode_id.clone(),
                ..Default::default()
            });
        entry.position_secs = position;
        entry.duration_secs = duration;
        entry.updated_at_secs = now;
        stamp_playback_history(entry, &target.history);
        if position > 0.0 {
            entry.unwatched_at_secs = 0;
        }
        if newly_watched {
            entry.watched = true;
        }
    }
    *expected = target.clone();
    if state
        .playback
        .as_ref()
        .is_some_and(|current| current.session == target.session)
    {
        state.playback = Some(target.clone());
    }
    (due || newly_watched).then(|| state.progress.clone())
}

/// Count short, advancing samples only: buffering/pauses contribute nothing,
/// seeks cannot turn their media distance into observed viewing time, and a
/// suspended app cannot count a long gap as playback.
fn observe_history_playback(target: &mut PlaybackTarget, position: f64, paused: bool) {
    let now = std::time::Instant::now();
    let elapsed = target
        .history_observed_at
        .replace(now)
        .map_or(0.0, |last| now.duration_since(last).as_secs_f64());
    let delta = position - target.last_pos;
    if let Some(history) = target.history.as_mut()
        && !paused
        && elapsed > 0.0
        && elapsed <= 2.0
        && delta.is_finite()
        && delta > 0.0
        && delta <= elapsed * 8.0 + 0.5
    {
        history.played_at_secs = now_secs();
        if let Some(seconds) = history.engaged_secs.as_mut() {
            *seconds = (*seconds + elapsed).min(watch_history::MEANINGFUL_PLAYBACK_SECS);
        }
    }
}

/// Only observations of real playback advance History's clock. Manual watched
/// changes and metadata refreshes must not move an old episode to the front.
fn stamp_playback_history(entry: &mut EpisodeProgress, captured: &Option<PlaybackHistory>) {
    if let Some(mut history) = captured.clone() {
        // Modern captures carry the clock of the last advancing sample.
        // Closing a paused player must not reorder History or undo dismissal.
        if history.engaged_secs.is_none() {
            history.played_at_secs = entry.updated_at_secs;
        }
        // Seeking back after EOF keeps the same player session. Recognize
        // its partial replay too, without clearing sticky completion.
        history.rewatch |=
            entry.watched && resumable_position(entry.position_secs, entry.duration_secs, false);
        entry.history = Some(history);
    }
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
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
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

    fn finalize_closed_playback(bridge: &Bridge) {
        let app = bridge.app().unwrap();
        assert!(!app.get_player_open());
        let home = app.get_show_home();
        let before = progress_projection::publication_count();
        bridge.note_player_progress_from_ui();
        assert!(bridge.shared.lock().unwrap().playback.is_none());
        // Closing persists progress immediately; the metadata joins and Home
        // publication deliberately happen on the coalesced worker. Await its
        // accepted publication, rather than forcing a synchronous rebuild or
        // reading the previous Home card before the event loop can deliver it.
        let started = std::time::Instant::now();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(3),
            move || {
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "closed-playback Home projection did not publish"
                );
                if progress_projection::publication_count() > before {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        assert_eq!(
            app.get_show_home(),
            home,
            "publication cannot navigate away from Home"
        );
    }

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
        i_slint_backend_testing::init_integration_test_with_system_time();
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
        // Deferred addons mount the same player before returning a media URL.
        let resolving = bridge.begin_playback_request();
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context = PlaybackContext {
            title: "Selected show".into(),
            osd_episode_title: "S3E2: Selected episode".into(),
            media_id: "resolver-show".into(),
            request_id: "resolver-episode".into(),
            is_episode: true,
            ..Default::default()
        };
        bridge.open_provider_loading(&resolving);
        assert!(app.get_player_open(), "open before the resolver completes");
        assert!(!app.get_playback_started());
        assert_eq!(app.get_player_status(), "Resolving stream…");
        assert_eq!(app.get_player_show_title(), "Selected show");
        assert_eq!(app.get_player_episode_title(), "S3E2: Selected episode");
        bridge.note_player_progress_from_ui();
        assert!(resolving.is_current());
        assert!(bridge.shared.lock().unwrap().progress.is_empty());
        bridge.cancel_pending_playback();
        assert!(!app.get_player_open());
        assert!(!resolving.is_current());
        let replacement = bridge.begin_playback_request();
        bridge.open_provider_loading(&replacement);
        let stale_url = addons::Stream {
            url: Some("https://example.invalid/stale".into()),
            ..Default::default()
        };
        bridge.complete_provider_resolution(resolving.clone(), false, Ok(stale_url));
        bridge.complete_provider_resolution(
            resolving,
            false,
            Err(nova_providers::ProviderResolveError::generic()),
        );
        assert!(replacement.is_current());
        assert_eq!(app.get_player_status(), "Resolving stream…");
        bridge.complete_provider_resolution(
            replacement.clone(),
            false,
            Err(nova_providers::ProviderResolveError::generic()),
        );
        assert!(!replacement.is_current());
        assert!(
            app.get_player_open(),
            "resolution failures remain readable in the player"
        );
        assert_eq!(app.get_player_status(), "Could not resolve this stream.");
        bridge.player.tick();
        assert_eq!(app.get_player_status(), "Could not resolve this stream.");
        bridge.player.close();

        let ask = bridge.begin_playback_request();
        let previous_behavior = {
            let mut state = bridge.shared.lock().unwrap();
            state.playback_intent.as_mut().unwrap().context = PlaybackContext {
                title: "Resume show".into(),
                media_id: "resolver-show".into(),
                request_id: "resolver-episode".into(),
                is_episode: true,
                ..Default::default()
            };
            state.progress.insert(
                progress_map_key("resolver-show", "resolver-episode"),
                EpisodeProgress {
                    series_id: "resolver-show".into(),
                    episode_id: "resolver-episode".into(),
                    position_secs: 300.0,
                    duration_secs: 1000.0,
                    ..Default::default()
                },
            );
            let previous = state.cache_settings.episode_start_behavior;
            state.cache_settings.episode_start_behavior = EpisodeStartBehavior::Ask;
            previous
        };
        bridge.open_provider_loading(&ask);
        let resolved: addons::Stream = serde_json::from_value(serde_json::json!({
            "url":"https://example.invalid/resolved", "novaSensitivePlayback":true,
            "behaviorHints":{"proxyHeaders":{"request":{"Authorization":"Bearer fixture"}}},
            "subtitles":[{"id":"fixture-en", "url":"https://example.invalid/subtitle.srt", "lang":"en"}]
        }))
        .unwrap();
        bridge.complete_provider_resolution(ask.clone(), false, Ok(resolved));
        assert!(app.get_player_open());
        assert!(
            app.get_resume_prompt_visible(),
            "the resume prompt stays above the resolving overlay"
        );
        {
            let state = bridge.shared.lock().unwrap();
            assert!(state.playback_intent.as_ref().unwrap().private_playback);
            let pending = state.pending_resume_stream.as_ref().unwrap();
            assert_eq!(pending.url, "https://example.invalid/resolved");
            assert_eq!(
                pending.headers,
                [("Authorization".into(), "Bearer fixture".into())]
            );
            assert_eq!(pending.subtitles, ["https://example.invalid/subtitle.srt"]);
        }
        bridge.cancel_resume_prompt();
        assert!(!ask.is_current());
        assert!(!app.get_player_open());
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.episode_start_behavior = previous_behavior;
            state.progress.clear();
            state.playback = None;
        }
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
        finalize_closed_playback(&bridge);
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
        finalize_closed_playback(&bridge);
        assert!((app.get_home_continue().row_data(0).unwrap().progress - 0.5).abs() < 0.001);

        // Complete the known show, announce a future episode, then release it.
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode-2".into(),
            last_pos: 1000.0,
            last_dur: 1000.0,
            ..Default::default()
        });
        finalize_closed_playback(&bridge);
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
            finalize_closed_playback(&bridge);
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
        finalize_closed_playback(&bridge);
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

        // Manual planning keeps a partially watched title out of active rails
        // without deleting its resume point, history, categories or sync data.
        let progress_before_planning = serde_json::to_value(read_progress_map()).unwrap();
        let activity_before_planning = read_viewing_activity();
        let categories_before_planning =
            bridge.shared.lock().unwrap().entries[0].categories.clone();
        bridge.library_status_action(0, 5);
        assert_eq!(
            app.get_library().row_data(0).unwrap().status,
            "Plan to Watch"
        );
        assert_eq!(app.get_home_continue().row_count(), 0);
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        let planned = read_persisted_library();
        assert_eq!(planned[0].watch_status, WatchStatus::PlanToWatch);
        assert_eq!(planned[0].categories, categories_before_planning);
        assert_eq!(
            serde_json::to_value(read_progress_map()).unwrap(),
            progress_before_planning
        );
        assert_eq!(read_viewing_activity(), activity_before_planning);
        // The existing automatic action restores normal progress classification.
        bridge.library_status_action(0, 0);
        assert_eq!(app.get_library().row_data(0).unwrap().status, "Watching");
        assert_eq!(app.get_home_continue().row_count(), 1);
        assert_eq!(
            serde_json::to_value(read_progress_map()).unwrap(),
            progress_before_planning
        );

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
                detached: false,
                context: PlaybackContext {
                    media_type: "series".into(),
                    media_id: "title".into(),
                    ..Default::default()
                },
            });
        }
        bridge.settings_to_ui();
        let native = bridge.player.effective_subtitle_style();
        assert_eq!(app.get_subtitle_style().row_data(1), native.font_size);
        assert_eq!(app.get_subtitle_style().row_data(4), native.outline);
        // Saving unrelated Settings must not turn displayed native values into
        // explicit, synced font/outline overrides.
        bridge.capture_settings();
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .subtitle_style
                .font_size
                .is_none()
        );
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .subtitle_style
                .outline
                .is_none()
        );
        bridge.edit_subtitle_style(0, 0.3, false);
        bridge.edit_subtitle_style(1, 70.0, true);
        assert_eq!(bridge.player.effective_subtitle_style().delay, 0.3);
        assert_eq!(
            bridge.player.effective_subtitle_style().font_size,
            Some(70.0)
        );
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .subtitle_style
                .delay,
            0.0
        );
        bridge.flush_playback_preferences(true);
        let style_key = preference_key("series", "title", 3);
        let saved_style = read_json::<HashMap<String, PreferenceValue>>(
            super::super::playback_preferences::PREFERENCES_KEY,
        )
        .unwrap();
        assert!(
            matches!(saved_style.get(&style_key), Some(PreferenceValue::SubtitleStyle(patch)) if patch.delay == Some(0.3) && patch.font_size.is_none())
        );
        bridge.restore_playback_default(3);
        assert_eq!(bridge.player.effective_subtitle_style().delay, 0.0);
        assert_eq!(
            bridge.player.effective_subtitle_style().font_size,
            Some(70.0)
        );
        assert!(!app.get_subtitle_style_preference_saved());
        assert!(
            bridge
                .prepare_playback_preferences()
                .title
                .subtitle_style
                .is_none()
        );
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

#[cfg(all(test, feature = "desktop"))]
mod history_tests {
    use super::*;

    #[test]
    fn history_qualifies_real_play_time_without_counting_seeks_pauses_or_suspension() {
        let mut target = PlaybackTarget {
            last_pos: 10.0,
            history: Some(PlaybackHistory {
                engaged_secs: Some(0.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let second_ago = || Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        target.history_observed_at = second_ago();
        observe_history_playback(&mut target, 1000.0, false);
        target.history_observed_at = second_ago();
        observe_history_playback(&mut target, 11.0, true);
        target.history_observed_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
        observe_history_playback(&mut target, 11.0, false);
        target.history_observed_at = second_ago();
        observe_history_playback(&mut target, 10.0, false);
        assert_eq!(target.history.as_ref().unwrap().engaged_secs, Some(0.0));
        for _ in 0..31 {
            target.history_observed_at = second_ago();
            let position = target.last_pos + 1.0;
            observe_history_playback(&mut target, position, false);
            target.last_pos = position;
        }
        assert_eq!(target.history.as_ref().unwrap().engaged_secs, Some(30.0));
    }

    #[test]
    fn seeking_back_after_completion_records_a_resumable_rewatch() {
        let mut record = EpisodeProgress {
            watched: true,
            position_secs: 300.0,
            duration_secs: 1200.0,
            updated_at_secs: 123,
            ..Default::default()
        };
        stamp_playback_history(&mut record, &Some(PlaybackHistory::default()));
        assert!(record.watched);
        assert!(record.history.as_ref().unwrap().rewatch);
        assert!(playback_can_resume(&record));
        assert_eq!(record.history.unwrap().played_at_secs, 123);
    }

    #[test]
    fn history_opens_exact_nonlibrary_episode_and_keeps_context_through_refresh() {
        const ROOT: &str = "NOVA_WATCH_HISTORY_ACTION_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-history-actions-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::playback::history_tests::history_opens_exact_nonlibrary_episode_and_keeps_context_through_refresh", "--nocapture"])
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
        let entry = watch_history::HistoryEntry {
            series_id: "not-in-library".into(),
            type_: "series".into(),
            episode_id: "opaque-original-e3".into(),
            title: "Original source title".into(),
            poster_url: String::new(),
            episode_label: "Third episode".into(),
            season: Some(2),
            episode: Some(3),
            played_at_secs: 100,
            position_secs: 300.0,
            resumable: true,
            meaningful: true,
            rewatch: true,
            duration_secs: 1200.0,
        };
        {
            let mut state = bridge.shared.lock().unwrap();
            state.home_history_list = vec![entry.clone()];
            state.progress.insert(
                progress_map_key(&entry.series_id, &entry.episode_id),
                EpisodeProgress {
                    series_id: entry.series_id.clone(),
                    episode_id: entry.episode_id.clone(),
                    position_secs: 300.0,
                    duration_secs: 1200.0,
                    watched: true,
                    play_count: 2,
                    ..Default::default()
                },
            );
        }
        bridge.watch_history_picked(0);
        assert!(app.get_stream_selector_open());
        assert!(app.get_detail_deep_stream());
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        let refreshed: MetaItem = serde_json::from_value(serde_json::json!({
            "id": entry.series_id, "type": "series", "name": "Refreshed title", "videos": [
                {"id":"first-episode", "name":"First", "season":1,"episode":1},
                {"id":entry.episode_id, "name":"Third episode", "season":2,"episode":3}
            ]
        }))
        .unwrap();
        bridge.apply_episode_meta(entry.series_id.clone(), Some(refreshed), false, token);
        assert!(
            app.get_stream_selector_open(),
            "cold metadata must not reset the selected stream request"
        );
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .request_id,
            entry.episode_id
        );
        let request = bridge.begin_playback_request();
        let captured = {
            let state = bridge.shared.lock().unwrap();
            let context = &state.playback_intent.as_ref().unwrap().context;
            assert_eq!(context.history_resume, Some(300.0));
            let history = context.history.as_ref().unwrap();
            assert!(history.rewatch);
            assert_eq!((history.season, history.episode), (Some(2), Some(3)));
            assert!(state.entries.is_empty());
            assert!(state.progress[&progress_map_key(&entry.series_id, &entry.episode_id)].watched);
            history.clone()
        };
        // Another modal cannot hijack an in-flight source's captured metadata.
        bridge.open_preview(
            MetaPreview {
                id: "other".into(),
                type_: "movie".into(),
                name: "Other title".into(),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        assert!(!bridge.playback_request_is_current(&request));
        let replacement = bridge.begin_playback_request();
        assert!(replacement.is_current());
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .unwrap()
                .context
                .history_resume
                .is_none()
        );
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: entry.series_id.clone(),
            episode_id: entry.episode_id.clone(),
            last_pos: 320.0,
            last_dur: 1200.0,
            history: Some(PlaybackHistory {
                played_at_secs: now_secs(),
                ..captured.clone()
            }),
            ..Default::default()
        });
        bridge.persist_outgoing_playback();
        let saved = read_progress_map();
        let record = &saved[&progress_map_key(&entry.series_id, &entry.episode_id)];
        assert!(record.watched, "resuming a rewatch never clears completion");
        assert_eq!(record.position_secs, 320.0);
        assert_eq!(record.history.as_ref().unwrap().title, captured.title);
        assert!(record.history.as_ref().unwrap().played_at_secs > 100);
        assert_eq!(
            serde_json::from_value::<EpisodeProgress>(serde_json::to_value(record).unwrap())
                .unwrap(),
            *record
        );

        // Removal persists and syncs independently of playback completion.
        {
            let mut state = bridge.shared.lock().unwrap();
            state
                .progress
                .get_mut(&progress_map_key(&entry.series_id, &entry.episode_id))
                .unwrap()
                .history
                .as_mut()
                .unwrap()
                .engaged_secs = Some(30.0);
        }
        bridge.refresh_home_history_models();
        let before = bridge.shared.lock().unwrap().progress.clone();
        bridge.watch_history_action(&entry.type_, &entry.series_id, 2);
        let hidden: HashMap<String, u64> = read_json(watch_history::HIDDEN_KEY).unwrap();
        let hidden_at = hidden[&watch_history::title_key(&entry.type_, &entry.series_id)];
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .home_history_archive
                .is_empty()
        );
        assert_eq!(bridge.shared.lock().unwrap().progress, before);
        let owner = nova_sync::local_store().unwrap();
        assert_eq!(
            owner.lock().unwrap().records(DOMAIN_HISTORY_HIDDEN).len(),
            1
        );
        bridge.sync_apply(vec![DOMAIN_HISTORY_HIDDEN.into()]);
        assert_eq!(
            read_json::<HashMap<String, u64>>(watch_history::HIDDEN_KEY).unwrap(),
            hidden
        );
        // A metadata / progress edit cannot undo the explicit dismissal.
        {
            let mut state = bridge.shared.lock().unwrap();
            let record = state
                .progress
                .get_mut(&progress_map_key(&entry.series_id, &entry.episode_id))
                .unwrap();
            record.updated_at_secs = hidden_at + 100;
        }
        bridge.refresh_home_history_models();
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .home_history_archive
                .is_empty()
        );
        // Actual later playback can surface the title again on either device.
        {
            let mut state = bridge.shared.lock().unwrap();
            let record = state
                .progress
                .get_mut(&progress_map_key(&entry.series_id, &entry.episode_id))
                .unwrap();
            record.history.as_mut().unwrap().played_at_secs = hidden_at + 1;
        }
        bridge.refresh_home_history_models();
        assert_eq!(bridge.shared.lock().unwrap().home_history_archive.len(), 1);
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .progress
                .values()
                .all(|record| record.watched)
        );
        // Enter series opens Detail, without starting the source selector.
        bridge.modal_closed();
        app.set_modal_visible(false);
        // A resume row can retain a title older than the archive cutoff.
        bridge.shared.lock().unwrap().home_history_archive.clear();
        bridge.watch_history_action(&entry.type_, &entry.series_id, 1);
        assert!(app.get_modal_visible());
        assert!(!app.get_stream_selector_open());
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn retry_snapshot_never_crosses_episode_title_or_modal_reopening() {
        let token = Arc::new(());
        let mut modal = ModalItem {
            details: Default::default(),
            open_token: token.clone(),
            pending_watch_now: None,
            episodes_loading: false,
            id: "show".into(),
            type_: "series".into(),
            request_id: "episode-2".into(),
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
        };
        let retry = RetryContext {
            modal_token: token.clone(),
            context: PlaybackContext {
                media_id: "show".into(),
                media_type: "series".into(),
                request_id: "episode-2".into(),
                history_resume: Some(123.75),
                ..Default::default()
            },
        };
        assert!(retry.matches(&modal));
        modal.request_id = "episode-3".into();
        assert!(!retry.matches(&modal));
        modal.request_id = "episode-2".into();
        modal.type_ = "movie".into();
        assert!(!retry.matches(&modal));
        modal.type_ = "series".into();
        modal.open_token = Arc::new(());
        assert!(
            !retry.matches(&modal),
            "a reopened detail page owns a different stream list"
        );
        modal.open_token = token;
        modal.id = "other-show".into();
        assert!(!retry.matches(&modal));
    }
}

#[cfg(all(test, feature = "desktop"))]
mod retry_regressions {
    use super::*;
    use nova_config::playback_preferences::{
        SessionPreferences, TrackChoice, TrackDescriptor, TrackKind,
    };

    fn row(id: &str) -> StreamUi {
        let url = format!("https://example.invalid/{id}");
        StreamUi {
            id: id.into(),
            choice_id: Some(stream_choice_identity(
                "owner",
                "movie",
                "movie",
                &addons::Stream {
                    url: Some(url.clone()),
                    ..Default::default()
                },
            )),
            owner_url: "owner".into(),
            display: id.into(),
            card: Default::default(),
            source: StreamSource::UrlWithOptions {
                url,
                headers: Vec::new(),
                subtitles: Vec::new(),
            },
            addon: "test".into(),
            download: None,
        }
    }

    #[test]
    fn recovery_deduplicates_exact_files_across_addon_rows() {
        let first = row("first");
        let mut mirror = first.clone();
        mirror.id = "mirror".into();
        mirror.owner_url = "other-owner".into();
        mirror.choice_id = row("mirror").choice_id;
        let selected = SelectedStream::from_row(&first);
        let mut budget = source_recovery::RetryBudget::default();
        budget.exclude_original(selected.retry_identity());
        let rows = vec![first, mirror, row("different")];
        assert_eq!(
            ranked_recovery_index(&rows, &selected, &Default::default(), Some(&budget), 1),
            Some(2)
        );
        assert!(!budget.accepts(SelectedStream::from_row(&rows[1]).retry_identity()));
    }

    #[test]
    fn recovery_honors_filters_and_sort_without_mutating_picker_order() {
        let mut rows = vec![row("current"), row("low"), row("best"), row("excluded")];
        for (row, height) in rows.iter_mut().zip([1080, 480, 720, 2160]) {
            row.card.filter_facts.height = Some(height);
        }
        let selected = SelectedStream::from_row(&rows[0]);
        let rules = nova_config::StreamFilterRules {
            enabled: true,
            max_height: Some(1080),
            ..Default::default()
        };
        let before = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let mut budget = source_recovery::RetryBudget::default();
        budget.exclude_original(selected.retry_identity());
        assert_eq!(
            ranked_recovery_index(&rows, &selected, &rules, Some(&budget), 1),
            Some(2)
        );
        assert!(budget.attempt(SelectedStream::from_row(&rows[2]).retry_identity()));
        assert_eq!(
            ranked_recovery_index(&rows, &selected, &rules, Some(&budget), 1),
            Some(1)
        );
        assert!(budget.attempt(SelectedStream::from_row(&rows[1]).retry_identity()));
        assert_eq!(
            ranked_recovery_index(&rows, &selected, &rules, Some(&budget), 1),
            None
        );
        assert_eq!(
            rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn pointer_selected_second_offer_and_semantic_retry_survive_promotion_and_failed_startup() {
        const ROOT: &str = "NOVA_RETRY_SOURCE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-retry-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::playback::retry_regressions::pointer_selected_second_offer_and_semantic_retry_survive_promotion_and_failed_startup", "--nocapture"]).output().unwrap();
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
        let token = Arc::new(());
        let prefs = SessionPreferences::from_settings(
            &CacheSettings::default(),
            nova_config::playback_preferences::TitlePreferences {
                audio: Some(TrackChoice::Track(TrackDescriptor {
                    language: Some("ja".into()),
                    kind: Some(TrackKind::Normal),
                    title: None,
                })),
                subtitles: Some(TrackChoice::Off),
                ..Default::default()
            },
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: token.clone(),
                pending_watch_now: None,
                episodes_loading: false,
                id: "movie".into(),
                type_: "movie".into(),
                request_id: "movie".into(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: "Test".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            });
            state.stream_all = vec![row("A"), row("B")];
            state.streams = state.stream_all.clone();
            // This is the semantic snapshot from A's untouched Automatic audio
            // and disabled subtitles. No title preference is persisted.
            state.playback_tools.pending_retry = Some(RetryContext {
                modal_token: token,
                context: PlaybackContext {
                    media_type: "movie".into(),
                    media_id: "movie".into(),
                    request_id: "movie".into(),
                    is_movie: true,
                    history_resume: Some(123.75),
                    retry_preferences: Some(prefs.clone()),
                    ..Default::default()
                },
            });
        }
        app.set_kb_active(false);
        bridge.stream_picked(1); // Pointer B, then last-used promotion moves B to 0.
        // The testing backend cannot install mpv's GL rendering notifier.
        // Model the native queue's accepted completion at the same app boundary
        // used in production; no source is loaded and no external app is opened.
        let context = bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .unwrap()
            .context
            .clone();
        assert!(bridge.player.wait_for_source("Loading stream…"));
        assert!(bridge.complete_player_queue(&context, Ok(())));
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.streams[0].id, "B");
            assert_eq!(
                state
                    .playback_intent
                    .as_ref()
                    .unwrap()
                    .context
                    .selected_stream
                    .as_ref()
                    .unwrap()
                    .row,
                "B"
            );
            assert!(
                state
                    .playback_intent
                    .as_ref()
                    .unwrap()
                    .context
                    .retry_preferences
                    .is_some(),
                "queue acceptance is not a first frame"
            );
        }
        assert_eq!(bridge.alternate_stream_index(), Some(1));
        app.set_playback_started(false); // Decoder B fails before producing a frame.
        {
            let mut state = bridge.shared.lock().unwrap();
            state.streams = vec![row("C"), row("B"), row("A")]; // Late reorder.
            state.stream_all = state.streams.clone();
        }
        assert_eq!(bridge.alternate_stream_index(), Some(2));
        bridge.shared.lock().unwrap().streams = vec![row("C")]; // B removed by a fresh snapshot.
        bridge.shared.lock().unwrap().stream_all = vec![row("C")];
        assert_eq!(bridge.alternate_stream_index(), Some(0));
        bridge.try_another_stream();
        let state = bridge.shared.lock().unwrap();
        let context = &state.playback_intent.as_ref().unwrap().context;
        assert_eq!(context.selected_stream.as_ref().unwrap().row, "C");
        let retained = context
            .retry_preferences
            .as_ref()
            .expect("retry snapshot survives B startup failure");
        assert_eq!(retained.title.audio, prefs.title.audio);
        assert_eq!(retained.title.subtitles, Some(TrackChoice::Off));
        assert_eq!(context.history_resume, Some(123.75));
        drop(state);
        assert!(
            read_json::<HashMap<String, nova_config::playback_preferences::PreferenceValue>>(
                playback_preferences::PREFERENCES_KEY
            )
            .unwrap_or_default()
            .is_empty()
        );
        // A replacement with an uncertain cut must never apply the old raw
        // playhead before the user sees its own duration and confirms.
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_duration(1200.0);
        app.set_position(0.25);
        {
            let mut state = bridge.shared.lock().unwrap();
            state
                .playback_intent
                .as_mut()
                .unwrap()
                .context
                .recovery_timeline = Some(source_recovery::Timeline {
                position: 123.75,
                duration: 1400.0,
                source: Some("different-cut".into()),
            });
        }
        assert!(bridge.check_recovery_timeline());
        assert!(app.get_resume_prompt_visible() && app.get_resume_source_changed());
        assert_eq!(app.get_position(), 0.25);
        app.set_duration(60.0); // The old 123.75 checkpoint does not fit this cut.
        bridge.resume_prompt_choice(true);
        assert!(app.get_resume_prompt_visible());
        assert!(!app.get_resume_source_can_resume());
        assert_eq!(app.get_position(), 0.25);
        assert_eq!(
            bridge.shared.lock().unwrap().playback_tools.guarded_resume,
            Some(123.75)
        );
        bridge.resume_prompt_choice(false);
        assert!(!app.get_resume_prompt_visible());
        assert_eq!(app.get_position(), 0.0);
        app.set_playback_started(false);
        let saved_choice = bridge.last_stream.identity(nova_config::now_secs());
        let automatic_context = bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .unwrap()
            .context
            .clone();
        {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_all = vec![row("automatic-other"), row("automatic-alternative")];
            state.streams = state.stream_all.clone();
            let mut context = automatic_context;
            context.automatic_source_selection = true;
            context.retry_budget = Default::default();
            state.cache_settings.auto_retry_streams = true;
            state.playback_tools.pending_retry = Some(RetryContext {
                context,
                modal_token: state.modal_item.as_ref().unwrap().open_token.clone(),
            });
        }
        bridge.stream_picked(1);
        // As with B above, this headless backend cannot install mpv's render
        // notifier. Model native queue acceptance before testing recovery of
        // that opening; a rejected/unowned start intentionally cannot retry.
        let accepted_context = bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .unwrap()
            .context
            .clone();
        assert!(bridge.player.wait_for_source("Loading stream…"));
        assert!(bridge.complete_player_queue(&accepted_context, Ok(())));
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .streams
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["automatic-other", "automatic-alternative"],
            "automatic choice must not reorder the picker"
        );
        assert_eq!(
            bridge.last_stream.identity(nova_config::now_secs()),
            saved_choice,
            "automatic retry must not replace an explicit source preference"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            let context = &mut state.playback_intent.as_mut().unwrap().context;
            context.retry_budget = Default::default();
            context
                .retry_budget
                .exclude_original(SelectedStream::from_row(&row("C")).retry_identity());
            assert!(
                context.retry_budget.attempt(
                    SelectedStream::from_row(&row("automatic-alternative")).retry_identity()
                )
            );
        }
        // A paused load is an internal hold, not an explicit user pause.
        let _ = bridge.player.play_with_preferences_and_load_policy(
            "https://example.invalid/held",
            0.0,
            &[],
            &[],
            prefs.clone(),
            crate::player::PlaybackLoadPolicy {
                allow_external: false,
                paused: true,
            },
        );
        app.set_player_open(true);
        app.set_playback_started(false);
        app.set_resume_prompt_visible(false);
        bridge.refresh_playback_tools();
        let preserved_budget = bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .unwrap()
            .context
            .retry_budget
            .clone();
        assert!(
            !preserved_budget.cancelled,
            "a held replacement must retain its second attempt"
        );
        bridge
            .execute_playback_action(nova_playback_api::PlaybackAction::SetPaused(true))
            .unwrap();
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .unwrap()
                .context
                .retry_budget
                .cancelled,
            "an explicit Pause must cancel even while internally held"
        );
        bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_mut()
            .unwrap()
            .context
            .retry_budget = preserved_budget;
        assert!(bridge.player.wait_for_source("Loading stream…")); // A terminal failed candidate is stopped.
        bridge
            .player
            .source_failed("Could not resolve this stream.");
        assert!(!bridge.player.awaiting_start_confirmation());
        bridge.refresh_playback_tools();
        {
            let state = bridge.shared.lock().unwrap();
            let context = &state.playback_intent.as_ref().unwrap().context;
            assert!(!context.retry_budget.cancelled);
            assert!(
                context
                    .retry_budget
                    .accepts(SelectedStream::from_row(&row("automatic-other")).retry_identity())
            );
        }
        assert_eq!(
            bridge.alternate_stream_index(),
            Some(0),
            "the accepted failed source keeps recovery authority"
        );
        bridge.retry_another_stream(true);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .playback_intent
                .as_ref()
                .unwrap()
                .context
                .selected_stream
                .as_ref()
                .unwrap()
                .row,
            "automatic-other",
            "the failed held alternative still permits the second distinct source"
        );
        let automatic_request = {
            let mut state = bridge.shared.lock().unwrap();
            let intent = state.playback_intent.as_mut().unwrap();
            intent.pending = true; // Resolver/native start has not committed.
            intent.token.clone()
        };
        assert!(bridge.playback_request_is_current(&automatic_request));
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .auto_retry_streams = false;
        assert!(
            !bridge.playback_request_is_current(&automatic_request),
            "turning automatic recovery off revokes its pending result"
        );
        assert!(!automatic_request.is_current());
        let local_context = bridge
            .shared
            .lock()
            .unwrap()
            .playback_intent
            .as_ref()
            .unwrap()
            .context
            .clone();
        let modal_token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.player.close();
        bridge.finish_playback_request();
        // A receiver can be started while Detail still owns a failed local
        // retry/bookmark selection. Neither a current-media replacement nor
        // a fresh detached Start may consume that modal's context.
        for recovering in [true, false] {
            let remote_context = PlaybackContext {
                media_type: "series".into(),
                media_id: "receiver-series".into(),
                request_id: "receiver-episode".into(),
                is_episode: true,
                history_resume: Some(321.25),
                retry_preferences: Some(prefs.clone()),
                bookmark_resume: true,
                ..Default::default()
            };
            {
                let mut state = bridge.shared.lock().unwrap();
                state.playback_tools.pending_retry = Some(RetryContext {
                    context: local_context.clone(),
                    modal_token: modal_token.clone(),
                });
                state.playback_tools.bookmark_resume = Some(BookmarkResume {
                    identity: nova_playback_api::MediaIdentity {
                        media_type: "movie".into(),
                        title_id: "movie".into(),
                        episode_id: "movie".into(),
                        ..Default::default()
                    },
                    position: 444.0,
                    modal_token: modal_token.clone(),
                });
                state.playback_tools.pending_stream_selection =
                    Some(SelectedStream::from_row(&row("local-row")));
                state.remote_playback.start_context = Some(nova_playback_api::MediaIdentity {
                    media_type: "series".into(),
                    title_id: "receiver-series".into(),
                    episode_id: "receiver-episode".into(),
                    ..Default::default()
                });
                state.remote_playback.start_retry = recovering.then_some(RemoteRetry {
                    context: remote_context,
                });
            }
            let request = bridge.begin_playback_request();
            assert!(request.is_current());
            let state = bridge.shared.lock().unwrap();
            let intent = state.playback_intent.as_ref().unwrap();
            assert!(intent.detached);
            assert_eq!(intent.context.media_id, "receiver-series");
            assert_eq!(intent.context.request_id, "receiver-episode");
            assert_eq!(intent.context.history_resume, recovering.then_some(321.25));
            assert!(intent.context.selected_stream.is_none());
            assert_eq!(intent.context.bookmark_resume, recovering);
            drop(state);
            bridge.finish_playback_request();
        }
    }
}

#[cfg(test)]
mod cast_observation_tests {
    use super::*;
    #[test]
    fn old_receiver_observation_cannot_mark_a_newly_selected_episode() {
        let mut state = Shared::default();
        state.playback = Some(PlaybackTarget {
            series_id: "new-title".into(),
            episode_id: "new-episode".into(),
            session: 2,
            last_pos: 12.,
            ..Default::default()
        });
        let mut old = PlaybackTarget {
            series_id: "old-title".into(),
            episode_id: "old-episode".into(),
            session: 1,
            ..Default::default()
        };
        let map = record_cast_progress(&mut state, &mut old, 990., 1000., false).unwrap();
        assert!(
            map.get(&progress_map_key("old-title", "old-episode"))
                .unwrap()
                .watched
        );
        assert!(!map.contains_key(&progress_map_key("new-title", "new-episode")));
        let current = state.playback.as_ref().unwrap();
        assert_eq!(current.session, 2);
        assert_eq!(current.last_pos, 12.);
    }
    #[test]
    fn nonfinite_receiver_positions_never_rewrite_resume_history() {
        let mut state = Shared::default();
        let mut expected = PlaybackTarget {
            series_id: "title".into(),
            episode_id: "episode".into(),
            session: 1,
            last_pos: 42.,
            ..Default::default()
        };
        assert!(record_cast_progress(&mut state, &mut expected, f64::NAN, 1000., false).is_none());
        assert!(
            record_cast_progress(&mut state, &mut expected, 42., f64::INFINITY, false).is_none()
        );
        assert_eq!(expected.last_pos, 42.);
        assert!(state.progress.is_empty());
    }
}

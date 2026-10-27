//! Open the exact recent episode without changing its sticky watched state.
use super::*;

#[derive(Clone)]
pub(super) struct HistorySelection {
    pub(super) modal_token: Arc<()>,
    pub(super) entry: watch_history::HistoryEntry,
}

impl HistorySelection {
    pub(super) fn matches(&self, modal: &ModalItem) -> bool {
        Arc::ptr_eq(&self.modal_token, &modal.open_token)
            && self.entry.type_ == modal.type_
            && self.entry.series_id == modal.id
            && self.entry.episode_id == modal.request_id
    }

    pub(super) fn resume_position(&self) -> f64 {
        if self.entry.can_resume() {
            self.entry.position_secs
        } else {
            0.0
        }
    }
}

impl Bridge {
    pub(super) fn watch_history_picked(&self, index: usize) {
        let Some(entry) = self
            .shared
            .lock()
            .unwrap()
            .home_history_list
            .get(index)
            .cloned()
        else {
            return;
        };
        self.open_history_entry(entry);
    }

    fn open_history_entry(&self, entry: watch_history::HistoryEntry) {
        if self.party_active() {
            let videos =
                read_episodes_cache_for(&entry.type_, &entry.series_id).unwrap_or_default();
            if !self.party_preflight_media_for_title(
                nova_party::PartyMedia {
                    identity: party_source::canonical_media(
                        nova_playback_api::MediaIdentity {
                            media_type: entry.type_.clone(),
                            title_id: entry.series_id.clone(),
                            episode_id: entry.episode_id.clone(),
                            title: entry.title.clone(),
                            episode: entry.episode_label.clone(),
                        },
                        &videos,
                    ),
                    season: entry.season,
                    episode: entry.episode,
                    duration: 0.0,
                },
                &entry.type_,
                &entry.series_id,
            ) {
                return;
            }
        }
        let preview = MetaPreview {
            id: entry.series_id.clone(),
            type_: entry.type_.clone(),
            name: entry.title.clone(),
            poster: (!entry.poster_url.is_empty()).then(|| entry.poster_url.clone()),
            ..Default::default()
        };
        // open_preview schedules fresh metadata but does not run the event loop:
        // the exact selection is installed before a late response can arrive.
        self.open_preview(preview, 0, false, None, false);
        let video = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_mut() else {
                return;
            };
            let token = modal.open_token.clone();
            let video = modal
                .videos
                .iter()
                .find(|video| video.id == entry.episode_id)
                .cloned()
                .unwrap_or_else(|| Video {
                    id: entry.episode_id.clone(),
                    name: entry.episode_label.clone(),
                    season: entry.season,
                    episode: entry.episode,
                    ..Default::default()
                });
            // Offline/cold metadata still has a usable canonical episode ID.
            // Retain it for source lookup and captured playback context.
            if entry.type_ != "movie" && !modal.videos.iter().any(|v| v.id == video.id) {
                modal.videos.push(video.clone());
            }
            state.history_selection = Some(HistorySelection {
                modal_token: token,
                entry: entry.clone(),
            });
            video
        };
        if entry.type_ == "movie" {
            self.watch_now();
        } else {
            self.select_episode_streams(entry.series_id, entry.episode_id, video, 0);
        }
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(true);
        }
    }

    /// Stable typed identities keep an open menu attached to its original title
    /// even if playback or sync reorders the Home models underneath it.
    pub(super) fn watch_history_action(&self, type_: &str, id: &str, action: i32) {
        if self.recommendation_action(type_, id, action) {
            return;
        }
        let entry = {
            let state = self.shared.lock().unwrap();
            // The archive and resume row have independent 100-title limits:
            // a still-resumable card may be older than the archive's cutoff.
            state
                .home_history_list
                .iter()
                .chain(state.home_history_archive.iter())
                .find(|entry| entry.type_ == type_ && entry.series_id == id)
                .cloned()
        };
        let Some(entry) = entry else { return };
        match action {
            0 => self.open_history_entry(entry),
            1 => self.open_preview(
                MetaPreview {
                    id: entry.series_id,
                    type_: entry.type_,
                    name: entry.title,
                    poster: (!entry.poster_url.is_empty()).then_some(entry.poster_url),
                    ..Default::default()
                },
                0,
                false,
                None,
                false,
            ),
            2 => {
                let mut hidden: HashMap<String, u64> =
                    read_json(watch_history::HIDDEN_KEY).unwrap_or_default();
                let keys = watch_history::hidden_title_keys(&self.shared.lock().unwrap(), &entry);
                let at = now_secs().max(entry.played_at_secs);
                for key in keys {
                    hidden
                        .entry(key)
                        .and_modify(|old| *old = (*old).max(at))
                        .or_insert(at);
                }
                write_json(watch_history::HIDDEN_KEY, &hidden);
                self.apply_home_to_ui();
            }
            _ => {}
        }
    }

    /// A background metadata response may enrich the selected episode, but
    /// must never reopen the picker or replace its exact stream request.
    pub(super) fn history_stream_selected(&self) -> bool {
        let state = self.shared.lock().unwrap();
        let matches = state
            .history_selection
            .as_ref()
            .zip(state.modal_item.as_ref())
            .is_some_and(|(selection, modal)| selection.matches(modal));
        drop(state);
        matches && self.app().is_some_and(|app| app.get_stream_selector_open())
    }
}

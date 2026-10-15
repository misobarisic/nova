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

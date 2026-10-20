//! Invalidate speculative source choices when effective profile/device rules change.
use super::*;
use nova_config::stream_filters::StreamFilterRules;

#[derive(Default)]
pub(super) struct State {
    last: Option<StreamFilterRules>,
}

impl Bridge {
    pub(super) fn refresh_stream_preferences_policy(&self) -> bool {
        {
            let mut state = self.shared.lock().unwrap();
            let rules = stream_filters::effective_rules(&state.cache_settings);
            plugin_settings::publish_stream_preferences(&rules);
            if state.stream_preferences.last.as_ref() == Some(&rules) {
                return false;
            }
            state.stream_preferences.last = Some(rules);
            state.stream_prefetch.invalidate();
            state.stream_prefetch_live_session = None;
            if let Some(playback) = state.playback.as_mut() {
                playback.next_episode_prefetched = None;
            }
            state.playback_end.invalidate_stream_preferences();
        }
        if !profiles::is_current(&profiles::capture()) {
            return true;
        }
        let Some(app) = self.app() else {
            return true;
        };
        app.set_auto_next_preview(false);
        // Upstream providers may already have filtered or truncated their
        // answer. Refiltering old rows cannot recover newly allowed offers.
        if app.get_modal_visible()
            && app.get_stream_selector_open()
            && !app.get_player_open()
            && !app.get_season_download_review()
            && !self.remote_output_selected()
        {
            let request = self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .map(|modal| modal.request_id.clone())
                .filter(|id| !id.is_empty());
            if let Some(request) = request {
                self.start_stream_search_inner(request, true);
            }
        }
        true
    }
}

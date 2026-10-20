//! Profile-owned recommendations from actual recent playback. Dismissals live
//! in synced settings; volatile state never owns its originating session.
use super::*;

const MAX_SEEDS: usize = 3;
const MAX_CARDS: usize = 20;
const REFRESH_AFTER: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, PartialEq, Eq)]
struct Signature {
    seeds: Vec<(String, String)>,
    providers: Vec<(String, u64)>,
    settings: u64,
    recommendation_settings: u64,
}

#[derive(Default)]
pub(super) struct State {
    signature: Option<Signature>,
    generation: u64,
    pending: bool,
    refreshed: Option<std::time::Instant>,
    candidates: Vec<MetaPreview>,
}

fn visible_candidates(state: &Shared) -> Vec<MetaPreview> {
    let mut excluded: HashSet<_> = state
        .home_history_archive
        .iter()
        .map(|entry| watch_history::title_key(&entry.type_, &entry.series_id))
        .chain(
            state
                .entries
                .iter()
                .map(|entry| watch_history::title_key(&entry.type_, &entry.id)),
        )
        .chain(
            state
                .cache_settings
                .home_recommendations_hidden
                .iter()
                .cloned(),
        )
        .chain(
            state
                .progress
                .values()
                .filter(|record| record.play_count > 0)
                .filter_map(|record| {
                    record
                        .history
                        .as_ref()
                        .filter(|history| !history.type_.is_empty())
                        .map(|history| watch_history::title_key(&history.type_, &record.series_id))
                }),
        )
        .collect();
    state
        .recommendations
        .candidates
        .iter()
        .filter(|preview| {
            !preview.id.is_empty()
                && !preview.title().trim().is_empty()
                && excluded.insert(watch_history::title_key(&preview.type_, &preview.id))
        })
        .take(MAX_CARDS)
        .cloned()
        .collect()
}

impl Bridge {
    pub(super) fn recommendation_group(&self) -> Option<HomeCatalogGroup> {
        let state = self.shared.lock().unwrap();
        if !state
            .cache_settings
            .home_row_enabled(&HomeRowSource::BecauseYouWatched)
        {
            return None;
        }
        let previews = visible_candidates(&state);
        (!previews.is_empty()).then(|| HomeCatalogGroup {
            source: HomeRowSource::BecauseYouWatched,
            title: text::tr("Because you watched").into(),
            previews,
        })
    }

    pub(super) fn ensure_recommendations_loaded(&self) {
        let (signature, generation) = {
            let mut state = self.shared.lock().unwrap();
            if !state
                .cache_settings
                .home_row_enabled(&HomeRowSource::BecauseYouWatched)
            {
                // Disable also invalidates an already running request. It may
                // finish on its worker, but cannot publish after re-enabling.
                if state.recommendations.pending {
                    state.recommendations.generation =
                        state.recommendations.generation.wrapping_add(1);
                    state.recommendations.pending = false;
                    state.recommendations.signature = None;
                }
                return;
            }
            let signature = Signature {
                seeds: state
                    .home_history_archive
                    .iter()
                    .filter(|entry| {
                        entry.meaningful
                            && matches!(entry.type_.as_str(), "series" | "movie")
                            && similar::supported_identity(&entry.type_, &entry.series_id)
                    })
                    .map(|entry| (entry.type_.clone(), entry.series_id.clone()))
                    .take(MAX_SEEDS)
                    .collect(),
                providers: state
                    .installed
                    .iter()
                    .filter(|addon| addon.enabled && addon.available)
                    .map(|addon| (addon.url.clone(), addon.generation))
                    .collect(),
                settings: nova_providers::plugin_settings::settings_generation(),
                recommendation_settings: similar::settings_authority(&state.cache_settings),
            };
            let same = state.recommendations.signature.as_ref() == Some(&signature);
            if same
                && (state.recommendations.pending
                    || state
                        .recommendations
                        .refreshed
                        .is_some_and(|at| at.elapsed() < REFRESH_AFTER))
            {
                return;
            }
            let recommendations = &mut state.recommendations;
            recommendations.generation = recommendations.generation.wrapping_add(1);
            recommendations.signature = Some(signature.clone());
            recommendations.pending = !signature.seeds.is_empty();
            recommendations.refreshed = Some(std::time::Instant::now());
            if !same {
                recommendations.candidates.clear();
            }
            (signature, recommendations.generation)
        };
        self.refresh_home_history_models();
        if !signature.seeds.is_empty() {
            self.fetch_recommendation_seed(signature, generation, 0, Vec::new());
        }
    }

    fn fetch_recommendation_seed(
        &self,
        signature: Signature,
        generation: u64,
        index: usize,
        candidates: Vec<MetaPreview>,
    ) {
        let (type_, id) = signature.seeds[index].clone();
        let bridge = self.clone();
        self.fetch_similar(&type_, &id, move |rows| {
            let _ = profiles::invoke_from_event_loop(move || {
                let mut candidates = candidates;
                // Keep the bounded source pool before exclusions; saved or
                // hidden leading titles must not crowd out later suggestions.
                candidates.extend(rows.into_iter().take(24));
                {
                    let state = bridge.shared.lock().unwrap();
                    if state.recommendations.generation != generation
                        || state.recommendations.signature.as_ref() != Some(&signature)
                        || !state
                            .cache_settings
                            .home_row_enabled(&HomeRowSource::BecauseYouWatched)
                        || signature.settings
                            != nova_providers::plugin_settings::settings_generation()
                        || signature.recommendation_settings
                            != similar::settings_authority(&state.cache_settings)
                        || signature.providers
                            != state
                                .installed
                                .iter()
                                .filter(|a| a.enabled && a.available)
                                .map(|a| (a.url.clone(), a.generation))
                                .collect::<Vec<_>>()
                    {
                        return;
                    }
                }
                if index + 1 < signature.seeds.len() {
                    bridge.shared.lock().unwrap().recommendations.candidates = candidates.clone();
                    bridge.refresh_home_history_models();
                    bridge.fetch_recommendation_seed(signature, generation, index + 1, candidates);
                    return;
                }
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.recommendations.candidates = candidates;
                    state.recommendations.pending = false;
                    state.recommendations.refreshed = Some(std::time::Instant::now());
                }
                bridge.refresh_home_history_models();
            });
        });
    }

    /// Existing typed card actions are shared with history. The dismissal
    /// identity comes from the opened menu, even after a candidate refresh.
    pub(super) fn recommendation_action(&self, type_: &str, id: &str, action: i32) -> bool {
        if !matches!(action, 1 | 3) {
            return false;
        }
        if type_.is_empty()
            || type_.len() > 64
            || id.is_empty()
            || id.len() > 512
            || type_.chars().chain(id.chars()).any(char::is_control)
        {
            return action == 3;
        }
        if action == 1 {
            let preview = {
                let state = self.shared.lock().unwrap();
                state
                    .recommendations
                    .candidates
                    .iter()
                    .find(|p| p.type_ == type_ && p.id == id)
                    .cloned()
            };
            let Some(preview) = preview else { return false };
            self.open_preview(preview, 0, false, None, false);
        } else {
            allow_setting_field("home_recommendations_hidden");
            let settings = {
                let mut state = self.shared.lock().unwrap();
                state
                    .cache_settings
                    .home_recommendations_hidden
                    .insert(watch_history::title_key(type_, id));
                state.cache_settings.clone()
            };
            set_active_cache_settings(settings.clone());
            write_settings(&settings);
            self.refresh_home_history_models();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendations_exclude_saved_watched_hidden_and_duplicates_by_typed_identity() {
        let mut state = Shared::default();
        let preview = |type_: &str, id: &str| MetaPreview {
            type_: type_.into(),
            id: id.into(),
            name: id.into(),
            ..Default::default()
        };
        state
            .cache_settings
            .home_recommendations_hidden
            .insert(watch_history::title_key("movie", "hidden"));
        state.entries.push(
            serde_json::from_value(
                serde_json::json!({"id":"saved", "type_":"series", "name":"Saved"}),
            )
            .unwrap(),
        );
        // A played title remains excluded even after falling outside the
        // bounded archive or being removed from visible watch history.
        state.progress.insert(
            "older".into(),
            EpisodeProgress {
                series_id: "older".into(),
                play_count: 1,
                history: Some(PlaybackHistory {
                    type_: "series".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        state.recommendations.candidates = vec![
            preview("movie", "hidden"),
            preview("series", "saved"),
            preview("series", "older"),
            preview("movie", "saved"),
            preview("series", "new"),
            preview("series", "new"),
        ];
        let visible = visible_candidates(&state);
        assert_eq!(
            visible
                .iter()
                .map(|p| (p.type_.as_str(), p.id.as_str()))
                .collect::<Vec<_>>(),
            vec![("movie", "saved"), ("series", "new")]
        );
        // A fresh result or restart uses the same persisted exclusion set.
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&state.cache_settings).unwrap()).unwrap();
        state.cache_settings = restored;
        assert_eq!(visible_candidates(&state), visible);
    }
}

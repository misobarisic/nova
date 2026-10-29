//! Completion preferences never manufacture watched records or tracker updates.
use super::*;

impl Bridge {
    pub(super) fn refresh_progress_scope_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let state = self.shared.lock().unwrap();
        let policy = state
            .modal_item
            .as_ref()
            .filter(|m| m.type_ != "movie")
            .map(|m| state.cache_settings.progress_policy(&m.id))
            .unwrap_or_default();
        let ui = app.global::<crate::DetailBridge>();
        ui.set_progress_include_specials(policy.include_specials);
        ui.set_progress_override_index(
            policy
                .title
                .include_specials
                .map_or(0, |included| if included { 2 } else { 1 }),
        );
    }

    fn edit_progress_scope(&self, change: impl FnOnce(&mut nova_config::progress::TitleProgress)) {
        if !writable_key("settings") {
            return;
        }
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let mut settings = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref().filter(|m| m.type_ != "movie") else {
                return;
            };
            let mut settings = state.cache_settings.clone();
            change(settings.title_progress.entry(modal.id.clone()).or_default());
            settings
        };
        settings
            .title_progress
            .retain(|_, value| value != &Default::default());
        // Persist preferences only: changing viewing scope is not a watch action.
        let failures = write_failures();
        write_settings(&settings);
        if failures != write_failures() {
            return;
        }
        self.shared.lock().unwrap().cache_settings = settings.clone();
        set_active_cache_settings(settings);
        self.refresh_progress_scope_ui();
        self.apply_episode_rows();
        self.apply_season_cards();
        self.refresh_watch_action();
        self.request_progress_projection();
        release_reminders::cancel_profile_notifications(profiles::ProfileStore::capture().id());
    }

    pub(super) fn change_progress_scope(&self, index: i32) {
        if !(0..=3).contains(&index) {
            return;
        }
        self.edit_progress_scope(|title| match index {
            0 => title.include_specials = None,
            1 => title.include_specials = Some(false),
            2 => title.include_specials = Some(true),
            _ => {
                title.excluded_episodes.clear();
                title.included_episodes.clear();
                title.excluded_seasons.clear();
            }
        });
    }

    pub(super) fn toggle_episode_progress_scope(&self, index: usize) {
        let Some((id, episode, _)) = self.resolve_filtered_episode(index) else {
            return;
        };
        // Gate in the backend as well as the menus: stale UI cannot edit scope
        // after the effective global/title preference has turned specials off.
        if !nova_config::progress::active_progress_policy(&id).include_specials {
            return;
        }
        let season = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .and_then(|m| {
                m.videos
                    .iter()
                    .find(|v| v.id == episode)
                    .and_then(|v| v.season)
            });
        let included =
            nova_config::progress::active_progress_policy(&id).includes(&episode, season);
        self.edit_progress_scope(|title| {
            if included {
                title.included_episodes.remove(&episode);
                title.excluded_episodes.insert(episode);
            } else {
                title.excluded_episodes.remove(&episode);
                title.included_episodes.insert(episode);
            }
        });
    }

    pub(super) fn toggle_season_progress_scope(&self, index: usize) {
        let selection = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            if !state
                .cache_settings
                .progress_policy(&modal.id)
                .include_specials
            {
                return;
            }
            catalog_detail::seasons(&state, modal)
                .get(index)
                .copied()
                .map(|season| {
                    (
                        season,
                        modal
                            .videos
                            .iter()
                            .filter(|v| v.season == Some(season))
                            .map(|v| v.id.clone())
                            .collect::<Vec<_>>(),
                    )
                })
        };
        if let Some((season, ids)) = selection {
            self.edit_progress_scope(|title| {
                for id in ids {
                    title.included_episodes.remove(&id);
                }
                if !title.excluded_seasons.remove(&season) {
                    title.excluded_seasons.insert(season);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_changes_completion_resume_counts_and_successors_without_watched_edits() {
        const CHILD: &str = "NOVA_PROGRESS_SCOPE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args(["--exact", "app::progress_scope::tests::scope_changes_completion_resume_counts_and_successors_without_watched_edits", "--nocapture"])
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let videos: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"one","season":1,"episode":1,"released":"2020-01-01"},
            {"id":"two","season":1,"episode":2,"released":"2020-01-02"},
            {"id":"special","season":0,"episode":1,"released":"2020-01-03"},
            {"id":"future-special","season":0,"episode":2,"released":"2999-01-01"}
        ]))
        .unwrap();
        let progress: HashMap<String, EpisodeProgress> = [
            ("one", true, 100.0),
            ("two", true, 100.0),
            ("special", false, 30.0),
        ]
        .into_iter()
        .map(|(id, watched, position)| {
            (
                progress_map_key("show", id),
                serde_json::from_value(serde_json::json!({
                    "series_id":"show", "episode_id":id, "watched":watched,
                    "position_secs":position, "duration_secs":100.0, "updated_at_secs":1, "play_count":1
                }))
                .unwrap(),
            )
        })
        .collect();
        let history_before = serde_json::to_value(&progress).unwrap();
        let mut settings = CacheSettings::default();
        set_active_cache_settings(settings.clone());
        assert!(available_completed(
            "show",
            &videos,
            &progress,
            today_days()
        ));
        assert_eq!(
            activity_bucket("show", &videos, &progress, None, today_days()),
            "Completed"
        );
        assert_eq!(library_episode_counts("show", &videos, &progress), (2, 2));
        assert_eq!(library_badge_for("show", &videos, &progress), "2 specials");
        assert!(next_episode_to_watch("show", &videos, &progress).is_none());
        assert!(next_episode::successor("show", &videos, "two", today_days()).is_none());
        assert!(watch_now_episode("show", &videos, &progress).is_none());
        let (future, count, watched) = upcoming_tally("show", &videos, |_| true, today_days());
        assert!(future.is_empty());
        assert_eq!((count, watched), (2, 2));

        settings.include_specials_in_progress = true;
        set_active_cache_settings(settings.clone());
        assert!(!available_completed(
            "show",
            &videos,
            &progress,
            today_days()
        ));
        assert_eq!(library_episode_counts("show", &videos, &progress), (2, 4));
        assert_eq!(
            watch_now_episode("show", &videos, &progress).unwrap().id,
            "special"
        );
        assert_eq!(
            next_episode::successor("show", &videos, "two", today_days())
                .unwrap()
                .id,
            "special"
        );
        settings
            .title_progress
            .entry("show".into())
            .or_default()
            .excluded_episodes
            .insert("special".into());
        set_active_cache_settings(settings.clone());
        assert!(available_completed(
            "show",
            &videos,
            &progress,
            today_days()
        ));
        assert_eq!(library_episode_counts("show", &videos, &progress), (2, 3));
        assert!(watch_now_episode("show", &videos, &progress).is_none());
        // The excluded episode is skipped; its unaired successor still blocks.
        assert!(next_episode::successor("show", &videos, "two", today_days()).is_none());
        settings
            .title_progress
            .get_mut("show")
            .unwrap()
            .include_specials = Some(false);
        set_active_cache_settings(settings);
        assert_eq!(library_episode_counts("show", &videos, &progress), (2, 2));
        assert_eq!(history_before, serde_json::to_value(&progress).unwrap());
    }
}

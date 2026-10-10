//! Unit tests for the app modules (moved out of `app.rs`).

#[cfg(test)]
mod episode_helpers_tests {
    use super::super::*;

    fn video(id: &str, season: Option<u32>, episode: Option<u32>, name: &str) -> Video {
        Video {
            id: id.into(),
            name: name.into(),
            title: String::new(),
            season,
            episode,
            number: None,
            ..Default::default()
        }
    }

    #[test]
    fn seasons_order_numbered_first_extras_last() {
        let videos = vec![
            video("a:3:1", Some(3), Some(1), "S3"),
            video("a:1:1", Some(1), Some(1), "S1"),
            video("a:0:1", Some(0), Some(1), "Special"),
            video("a:2:1", Some(2), Some(1), "S2"),
            video("no-season", None, None, "? no"),
        ];
        assert_eq!(ordered_seasons(&videos), vec![1, 2, 3, 0]);
    }

    #[test]
    fn season_episodes_order_and_labels() {
        let videos = vec![
            video("a:1:2", Some(1), Some(2), "Two"),
            video("a:1:10", Some(1), Some(10), "Ten"),
            video("a:1:1", Some(1), Some(1), "One"),
            video("a:1:x", Some(1), None, "No number"),
        ];
        let eps = season_episodes(&videos, 1);
        assert_eq!(eps[0].id, "a:1:1");
        assert_eq!(eps[1].id, "a:1:2");
        assert_eq!(eps[2].id, "a:1:10");
        assert_eq!(eps[3].id, "a:1:x"); // missing number sorts last
        assert_eq!(episode_row_label(eps[0]), "One");
        assert_eq!(episode_row_label(eps[3]), "No number");
        assert_eq!(episode_context_label(eps[2]), "Ten");
        assert_eq!(episode_badge(eps[0]), "S1 E1");
        assert_eq!(episode_badge(eps[2]), "S1 E10");
    }

    #[test]
    fn season_art_prefers_own_backdrop_then_first_episode_in_number_order() {
        let mut videos = vec![
            video("s1e2", Some(1), Some(2), "Two"),
            video("s2e1", Some(2), Some(1), "Other season"),
            video("s1e1", Some(1), Some(1), "One"),
        ];
        for v in &mut videos {
            v.thumbnail = Some(format!("https://img/{}", v.id));
        }
        let backdrops = HashMap::from([(1, "https://img/season-one".into())]);
        assert_eq!(
            season_thumb_url(&videos, &backdrops, 1).as_deref(),
            Some("https://img/season-one")
        );
        assert_eq!(
            season_thumb_url(&videos, &backdrops, 2).as_deref(),
            Some("https://img/s2e1")
        );
        assert_eq!(
            season_thumb_url(&videos, &HashMap::new(), 1).as_deref(),
            Some("https://img/s1e1")
        );
        assert_eq!(season_thumb_url(&videos, &backdrops, 3), None);

        // Missing art on the first episode should not choose a later scene.
        videos[2].thumbnail = Some(" ".into());
        let blank = HashMap::from([(1, " ".into())]);
        assert_eq!(season_thumb_url(&videos, &blank, 1), None);
        assert_eq!(
            season_thumb_url(&videos, &backdrops, 1).as_deref(),
            Some("https://img/season-one")
        );
    }

    #[test]
    fn row_label_collapses_interior_whitespace() {
        // Addon titles with line breaks would reserve multi-line card
        // height while painting one elided line (uneven grid rows).
        let v = video("a:1:4", Some(1), Some(4), "  Part\nOne\tTwo  ");
        assert_eq!(episode_row_label(&v), "Part One Two");
    }

    fn prog(series: &str, ep: &str, watched: bool, at: u64) -> EpisodeProgress {
        EpisodeProgress {
            series_id: series.into(),
            episode_id: ep.into(),
            position_secs: if watched { 600.0 } else { 0.0 },
            duration_secs: 600.0,
            watched,
            unwatched_at_secs: 0,
            play_count: 1,
            updated_at_secs: at,
        }
    }

    #[test]
    fn next_episode_offers_first_released_unwatched() {
        let mut eps = vec![
            video("s:1:1", Some(1), Some(1), "One"),
            video("s:1:2", Some(1), Some(2), "Two"),
            video("s:1:3", Some(1), Some(3), "Three"),
        ];
        for e in &mut eps {
            e.released = Some("2020-01-01".to_string());
        }
        // Nothing watched: the first released episode.
        assert_eq!(
            next_episode_to_watch("s", &eps, &HashMap::new()).map(|v| v.id.as_str()),
            Some("s:1:1")
        );
        // Sequential progress: the episode right after the finished one.
        let mut map = HashMap::new();
        map.insert(progress_map_key("s", "s:1:1"), prog("s", "s:1:1", true, 1));
        map.insert(progress_map_key("s", "s:1:2"), prog("s", "s:1:2", true, 2));
        assert_eq!(
            next_episode_to_watch("s", &eps, &map).map(|v| v.id.as_str()),
            Some("s:1:3")
        );
        // A skipped episode is offered first (first unwatched, not last+1).
        map.insert(progress_map_key("s", "s:1:3"), prog("s", "s:1:3", true, 3));
        map.remove(&progress_map_key("s", "s:1:2"));
        assert_eq!(
            next_episode_to_watch("s", &eps, &map).map(|v| v.id.as_str()),
            Some("s:1:2")
        );
        // Everything released watched: nothing to offer.
        map.insert(progress_map_key("s", "s:1:2"), prog("s", "s:1:2", true, 4));
        assert!(next_episode_to_watch("s", &eps, &map).is_none());
    }

    #[test]
    fn watch_action_resumes_the_latest_episode_and_then_offers_the_next_release() {
        let mut eps = vec![
            video("s:1:1", Some(1), Some(1), "One"),
            video("s:1:2", Some(1), Some(2), "Two"),
            video("s:2:1", Some(2), Some(1), "Three"),
        ];
        for e in &mut eps {
            e.released = Some("2020-01-01".into());
        }
        let mut map = HashMap::new();
        assert_eq!(
            episode_se_label(watch_now_episode("s", &eps, &map).unwrap()),
            "S1 E1"
        );
        let mut resume = prog("s", "s:2:1", false, 10);
        resume.position_secs = 300.0;
        resume.duration_secs = 1400.0;
        map.insert(progress_map_key("s", "s:2:1"), resume);
        assert_eq!(watch_now_episode("s", &eps, &map).unwrap().id, "s:2:1");
        // Explicit dateless progress still resumes; a known future date does not.
        eps[2].released = None;
        assert_eq!(watch_now_episode("s", &eps, &map).unwrap().id, "s:2:1");
        eps[2].released = Some("2999-01-01".into());
        assert_eq!(watch_now_episode("s", &eps, &map).unwrap().id, "s:1:1");
        eps[2].released = Some("2020-01-01".into());
        map.insert(progress_map_key("s", "s:2:1"), prog("s", "s:2:1", true, 11));
        map.insert(progress_map_key("s", "s:1:1"), prog("s", "s:1:1", true, 12));
        assert_eq!(watch_now_episode("s", &eps, &map).unwrap().id, "s:1:2");
        map.insert(progress_map_key("s", "s:1:2"), prog("s", "s:1:2", true, 13));
        assert!(watch_now_episode("s", &eps, &map).is_none());
    }

    #[test]
    fn next_episode_skips_dateless() {
        let mut eps = vec![
            video("s:1:1", Some(1), Some(1), "One"),
            video("s:1:2", Some(1), Some(2), "Two"),
            video("s:1:3", Some(1), Some(3), "Three"),
        ];
        eps[0].released = Some("2020-01-01".to_string());
        eps[2].released = Some("2020-01-03".to_string());
        // s:1:2 has no air date: skipped even though it airs "next".
        let mut map = HashMap::new();
        map.insert(progress_map_key("s", "s:1:1"), prog("s", "s:1:1", true, 1));
        assert_eq!(
            next_episode_to_watch("s", &eps, &map).map(|v| v.id.as_str()),
            Some("s:1:3")
        );
        // Everything dated watched: nothing — the dateless episode waits for
        // a manual start instead of surfacing here.
        map.insert(progress_map_key("s", "s:1:3"), prog("s", "s:1:3", true, 2));
        assert!(next_episode_to_watch("s", &eps, &map).is_none());
    }

    #[test]
    fn upcoming_tally_ignores_dateless() {
        let mut eps = vec![
            video("s:1:1", Some(1), Some(1), "One"),
            video("s:1:2", Some(1), Some(2), "Two"),
            video("s:1:3", Some(1), Some(3), "Three"),
        ];
        eps[0].released = Some("2020-01-01".to_string());
        eps[2].released = Some("2999-01-01".to_string());
        // s:1:2 is dateless: neither future nor available.
        let mut map = HashMap::new();
        map.insert(progress_map_key("s", "s:1:1"), prog("s", "s:1:1", true, 1));
        let (future, available, watched) = upcoming_tally(
            &eps,
            |v| {
                map.get(&progress_map_key("s", &v.id))
                    .is_some_and(|p| p.watched)
            },
            today_days(),
        );
        assert_eq!(future.len(), 1);
        assert_eq!(future[0].0, 2);
        assert_eq!((available, watched), (1, 1));
        // All dateless: nothing to count anywhere.
        let (future, available, watched) = upcoming_tally(&eps[1..2], |_| false, today_days());
        assert!(future.is_empty());
        assert_eq!((available, watched), (0, 0));
    }

    #[test]
    fn next_episode_skips_unaired_and_orders_seasons_extras_last() {
        let mut eps = vec![
            video("s:1:1", Some(1), Some(1), "One"),
            video("s:2:1", Some(2), Some(1), "S2 One"),
            video("s:0:1", Some(0), Some(1), "Special"),
        ];
        eps[1].released = Some("2999-01-01".to_string()); // unaired
        eps[0].released = Some("2020-01-01".to_string());
        eps[2].released = Some("2020-01-02".to_string());
        let mut map = HashMap::new();
        map.insert(progress_map_key("s", "s:1:1"), prog("s", "s:1:1", true, 1));
        // Season 2 is not out yet; the special (season 0) is offered last.
        assert_eq!(
            next_episode_to_watch("s", &eps, &map).map(|v| v.id.as_str()),
            Some("s:0:1")
        );
        // An empty list never panics.
        assert!(next_episode_to_watch("s", &[], &map).is_none());
    }
}

#[cfg(test)]
mod episode_pagination_tests {
    #[test]
    fn pages_are_fifty_episodes_with_a_minimum_of_one() {
        // Big seasons paginate in 50s; a season that fits keeps one page.
        assert_eq!(crate::app::page_count(0), 1);
        assert_eq!(crate::app::page_count(1), 1);
        assert_eq!(crate::app::page_count(50), 1);
        assert_eq!(crate::app::page_count(51), 2);
        assert_eq!(crate::app::page_count(200), 4);
        // The last page's slice: the index math the Slint side mirrors with
        // `episode_page_start + i`.
        let total = 200usize;
        let start = 3 * crate::app::EPISODE_PAGE_SIZE;
        assert_eq!(start, 150);
        assert_eq!((total - start).min(crate::app::EPISODE_PAGE_SIZE), 50);
    }
}

#[cfg(test)]
mod playback_tests {
    use super::super::*;

    fn video(id: &str, season: u32, episode: u32, name: &str) -> Video {
        Video {
            id: id.into(),
            name: name.into(),
            title: String::new(),
            season: Some(season),
            episode: Some(episode),
            number: None,
            ..Default::default()
        }
    }

    fn entry(
        series: &str,
        ep: &str,
        pos: f64,
        dur: f64,
        watched: bool,
        at: u64,
    ) -> EpisodeProgress {
        EpisodeProgress {
            series_id: series.into(),
            episode_id: ep.into(),
            position_secs: pos,
            duration_secs: dur,
            watched,
            unwatched_at_secs: 0,
            play_count: 1,
            updated_at_secs: at,
        }
    }

    #[test]
    fn library_counts_whole_known_series_and_only_watched_episodes() {
        let mut eps = vec![
            video("s:1:1", 1, 1, "Aired"),
            video("s:1:2", 1, 2, "Dateless"),
            video("s:1:3", 1, 3, "Future"),
            video("s:0:1", 0, 1, "Special"),
        ];
        eps[0].released = Some("2020-01-01".into());
        eps[2].released = Some("2099-01-01".into());
        assert_eq!(library_episode_counts("s", &eps, &HashMap::new()), (0, 4));
        let mut map = HashMap::new();
        for id in ["s:1:1", "s:0:1", "stale"] {
            map.insert(progress_map_key("s", id), entry("s", id, 0.0, 0.0, true, 1));
        }
        // A started episode doesn't become a watched episode just by having
        // a saved position. Another show's history cannot count either.
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 60.0, 600.0, false, 2),
        );
        map.insert(
            progress_map_key("other", "s:1:3"),
            entry("other", "s:1:3", 0.0, 0.0, true, 3),
        );
        assert_eq!(library_episode_counts("s", &eps, &map), (2, 4));
        assert_eq!(library_episode_counts("s", &[], &map), (0, 0));
        eps.push(eps[0].clone());
        assert_eq!(library_episode_counts("s", &eps, &map), (2, 4));
        for v in &eps {
            map.insert(
                progress_map_key("s", &v.id),
                entry("s", &v.id, 0.0, 0.0, true, 4),
            );
        }
        assert_eq!(library_episode_counts("s", &eps, &map), (4, 4));
        map.get_mut(&progress_map_key("s", "s:1:1"))
            .unwrap()
            .watched = false;
        assert_eq!(library_episode_counts("s", &eps, &map), (3, 4));
    }

    #[test]
    fn progress_keys_distinguish_series_and_episode() {
        assert_ne!(progress_map_key("a", "e1"), progress_map_key("a", "e2"));
        assert_ne!(progress_map_key("a", "e1"), progress_map_key("b", "e1"));
    }

    #[test]
    fn watched_threshold_needs_known_duration() {
        assert!(is_watched_position(54.0, 60.0));
        assert!(!is_watched_position(53.0, 60.0));
        assert!(!is_watched_position(60.0, 0.0));
        assert!(!is_watched_position(0.0, 60.0));
        // Exact 90% boundary counts as watched; overrun clamps, not errors.
        assert!(is_watched_position(540.0, 600.0));
        assert!(is_watched_position(900.0, 600.0));
        assert_eq!(progress_fraction(30.0, 60.0), 0.5);
        assert_eq!(progress_fraction(90.0, 60.0), 1.0);
        assert_eq!(progress_fraction(10.0, 0.0), 0.0);
    }

    #[test]
    fn resumable_position_skips_shorts_and_watched() {
        assert!(!resumable_position(5.0, 600.0, false)); // cold open
        assert!(resumable_position(120.0, 600.0, false));
        assert!(!resumable_position(590.0, 600.0, false)); // past threshold
        assert!(!resumable_position(120.0, 600.0, true)); // watched is sticky
        // Exact boundaries: 10 s offers resume, exact 90% does not (watched).
        assert!(resumable_position(10.0, 600.0, false));
        assert!(!resumable_position(540.0, 600.0, false));
        // Unknown duration still resumes past the cold-open window.
        assert!(resumable_position(120.0, 0.0, false));
    }

    #[test]
    fn resume_step_lands_disarms_and_only_seeks_when_missed() {
        // No frames yet / unknown duration / already handled: nothing to do.
        assert_eq!(resume_step(120.0, 0.0, 0.0, true, false), ResumeStep::Idle);
        assert_eq!(
            resume_step(120.0, 0.0, 600.0, false, false),
            ResumeStep::Idle
        );
        assert_eq!(resume_step(120.0, 0.0, 600.0, true, true), ResumeStep::Idle);
        // Reached within the 5 s slack (either side): land and disarm.
        assert_eq!(
            resume_step(120.0, 120.0, 600.0, true, false),
            ResumeStep::Landed
        );
        assert_eq!(
            resume_step(120.0, 125.0, 600.0, true, false),
            ResumeStep::Landed
        );
        assert_eq!(
            resume_step(120.0, 115.0, 600.0, true, false),
            ResumeStep::Landed
        );
        // Engine opened elsewhere (ignored the load-time start): seek once.
        assert_eq!(
            resume_step(120.0, 0.0, 600.0, true, false),
            ResumeStep::Seek
        );
        assert_eq!(
            resume_step(120.0, 125.1, 600.0, true, false),
            ResumeStep::Seek
        );
        assert_eq!(
            resume_step(120.0, 114.9, 600.0, true, false),
            ResumeStep::Seek
        );
    }

    #[test]
    fn badge_reports_seen_resume_and_left() {
        let mut eps = vec![
            video("s:1:1", 1, 1, "One"),
            video("s:1:2", 1, 2, "Two"),
            video("s:1:3", 1, 3, "Three"),
        ];
        for e in &mut eps {
            e.released = Some("2020-01-01".to_string());
        }
        // Fresh series: no badge noise.
        assert_eq!(library_badge_for("s", &eps, &HashMap::new()), "");
        // One watched, rest fresh: "N left".
        let mut map = HashMap::new();
        map.insert(
            progress_map_key("s", "s:1:1"),
            entry("s", "s:1:1", 600.0, 600.0, true, 1),
        );
        assert_eq!(library_badge_for("s", &eps, &map), "2 left");
        // A resumable episode wins over the count.
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 120.0, 600.0, false, 2),
        );
        assert_eq!(library_badge_for("s", &eps, &map), "▶ Resume Two");
        // All watched: no badge (the card's top-right checkmark carries it).
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 600.0, 600.0, true, 3),
        );
        map.insert(
            progress_map_key("s", "s:1:3"),
            entry("s", "s:1:3", 600.0, 600.0, true, 4),
        );
        assert_eq!(library_badge_for("s", &eps, &map), "");
    }

    #[test]
    fn badge_ignores_dateless_in_left_counts() {
        let mut eps = vec![
            video("s:1:1", 1, 1, "One"),
            video("s:1:2", 1, 2, "Two"),
            video("s:1:3", 1, 3, "Three"),
        ];
        eps[0].released = Some("2020-01-01".to_string());
        eps[1].released = Some("2020-01-02".to_string());
        // s:1:3 has no air date.
        let mut map = HashMap::new();
        map.insert(
            progress_map_key("s", "s:1:1"),
            entry("s", "s:1:1", 600.0, 600.0, true, 1),
        );
        // One dated episode left; the dateless one is not counted.
        assert_eq!(library_badge_for("s", &eps, &map), "1 left");
        // A watched dateless episode neither inflates nor underflows the count.
        map.insert(
            progress_map_key("s", "s:1:3"),
            entry("s", "s:1:3", 600.0, 600.0, true, 2),
        );
        assert_eq!(library_badge_for("s", &eps, &map), "1 left");
        // Everything dated watched with the dateless episode still pending:
        // "Caught up" (no unaired tail to name here).
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 600.0, 600.0, true, 3),
        );
        map.remove(&progress_map_key("s", "s:1:3"));
        assert_eq!(library_badge_for("s", &eps, &map), "Caught up");
        // A started dateless episode still earns its resume badge: starting
        // it was explicit, so it surfaces like any other resume.
        map.insert(
            progress_map_key("s", "s:1:3"),
            entry("s", "s:1:3", 120.0, 600.0, false, 4),
        );
        assert_eq!(library_badge_for("s", &eps, &map), "▶ Resume Three");
    }

    #[test]
    fn badge_without_episode_list_shows_resume() {
        let mut map = HashMap::new();
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 120.0, 600.0, false, 2),
        );
        // No episode list to label from: still reports a resume badge.
        assert_eq!(library_badge_for("s", &[], &map), "▶ Resume");
    }

    #[test]
    fn auto_bucket_derives_plan_watching_completed() {
        let eps = vec![video("s:1:1", 1, 1, "One"), video("s:1:2", 1, 2, "Two")];
        // Untouched: plan to watch (movies with no episodes land here too).
        assert_eq!(auto_bucket("s", &eps, &HashMap::new()), "Plan to Watch");
        assert_eq!(auto_bucket("m", &[], &HashMap::new()), "Plan to Watch");
        // Partial progress: watching.
        let mut map = HashMap::new();
        map.insert(
            progress_map_key("s", "s:1:1"),
            entry("s", "s:1:1", 600.0, 600.0, true, 1),
        );
        assert_eq!(auto_bucket("s", &eps, &map), "Watching");
        // Unwatched resume position also counts as watching.
        let mut map2 = HashMap::new();
        map2.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 120.0, 600.0, false, 2),
        );
        assert_eq!(auto_bucket("s", &eps, &map2), "Watching");
        // All watched: completed.
        map.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 600.0, 600.0, true, 2),
        );
        assert_eq!(auto_bucket("s", &eps, &map), "Completed");
        // Completion means everything currently available is watched. The
        // full-list helper still includes the future tail.
        let mut eps2 = vec![
            video("s:1:1", 1, 1, "One"),
            video("s:1:2", 1, 2, "Two"),
            video("s:1:3", 1, 3, "Three"),
        ];
        eps2[0].released = Some("2020-01-01".to_string());
        eps2[1].released = Some("2020-01-02".to_string());
        eps2[2].released = Some("2999-01-01".to_string());
        assert!(!series_fully_watched("s", &eps2, &map));
        assert_eq!(auto_bucket("s", &eps2, &map), "Completed");
        assert_eq!(library_badge_for("s", &eps2, &map), "Caught up · 1 unaired");
        // …but a list with nothing out yet never completes, even untouched.
        let eps3 = vec![eps2[2].clone()];
        assert!(!series_fully_watched("s", &eps3, &HashMap::new()));
        assert_eq!(auto_bucket("s", &eps3, &HashMap::new()), "Plan to Watch");
        assert_eq!(library_badge_for("s", &eps3, &HashMap::new()), "1 unaired");
        // Resume and left counts name the unaired tail too…
        let mut map3 = HashMap::new();
        map3.insert(
            progress_map_key("s", "s:1:1"),
            entry("s", "s:1:1", 600.0, 600.0, true, 1),
        );
        map3.insert(
            progress_map_key("s", "s:1:2"),
            entry("s", "s:1:2", 120.0, 600.0, false, 2),
        );
        assert_eq!(
            library_badge_for("s", &eps2, &map3),
            "▶ Resume Two · 1 unaired"
        );
        map3.remove(&progress_map_key("s", "s:1:2"));
        assert_eq!(library_badge_for("s", &eps2, &map3), "1 left · 1 unaired");
        // A dated-complete series with an unstarted dateless episode pending
        // still names the wait (dateless episodes stay out of the counts).
        let mut eps4 = eps2.clone();
        eps4.push(video("s:0:1", 0, 1, "Special"));
        assert!(!series_fully_watched("s", &eps4, &map));
        assert_eq!(auto_bucket("s", &eps4, &map), "Completed");
        assert_eq!(library_badge_for("s", &eps4, &map), "Caught up · 1 unaired");
        // Built-in names are reserved and distinct from user categories.
        assert!(BUILTIN_FILTERS.contains(&"Watching"));
        assert_eq!(WatchStatus::default(), WatchStatus::Auto);
        assert_eq!(WatchStatus::OnHold.badge_label(), Some("On Hold"));
        assert_eq!(WatchStatus::Auto.badge_label(), None);
    }

    #[test]
    fn movie_and_listless_entries_badge_bucket_and_check() {
        // Watched movie (no episode list): "✓ Seen" badge, Watching bucket,
        // never a card checkmark (completion needs an episode list).
        let mut map = HashMap::new();
        map.insert(
            progress_map_key("m", "m"),
            entry("m", "m", 3500.0, 3600.0, true, 1),
        );
        assert_eq!(library_badge_for("m", &[], &map), "✓ Seen");
        assert_eq!(auto_bucket("m", &[], &map), "Watching");
        assert!(!series_fully_watched("m", &[], &map));
        // In-progress movie: resume badge, Watching, no checkmark.
        let mut map2 = HashMap::new();
        map2.insert(
            progress_map_key("m", "m"),
            entry("m", "m", 600.0, 3600.0, false, 2),
        );
        assert_eq!(library_badge_for("m", &[], &map2), "▶ Resume");
        assert_eq!(auto_bucket("m", &[], &map2), "Watching");
        assert!(!series_fully_watched("m", &[], &map2));
        // Dateless-only list, untouched: silent badge, Plan to Watch.
        let eps = vec![video("s:0:1", 0, 1, "Special")];
        assert_eq!(library_badge_for("s", &eps, &HashMap::new()), "");
        assert_eq!(auto_bucket("s", &eps, &HashMap::new()), "Plan to Watch");
        assert!(!series_fully_watched("s", &eps, &HashMap::new()));
        // Dateless-only list, started: resume badge, Watching, no checkmark.
        let mut map3 = HashMap::new();
        map3.insert(
            progress_map_key("s", "s:0:1"),
            entry("s", "s:0:1", 120.0, 600.0, false, 3),
        );
        assert_eq!(library_badge_for("s", &eps, &map3), "▶ Resume Special");
        assert_eq!(auto_bucket("s", &eps, &map3), "Watching");
        assert!(!series_fully_watched("s", &eps, &map3));
        // Dateless-only list, all watched: completed with checkmark.
        let mut map4 = HashMap::new();
        map4.insert(
            progress_map_key("s", "s:0:1"),
            entry("s", "s:0:1", 600.0, 600.0, true, 4),
        );
        assert_eq!(library_badge_for("s", &eps, &map4), "");
        assert_eq!(auto_bucket("s", &eps, &map4), "Completed");
        assert!(series_fully_watched("s", &eps, &map4));
    }

    #[test]
    fn synopsis_truncates_at_word_boundary() {
        assert_eq!(Bridge::truncate_synopsis("  short  ", 180), "short");
        assert_eq!(Bridge::truncate_synopsis("", 180), "");
        let long = "word ".repeat(50); // 250 chars
        let cut = Bridge::truncate_synopsis(&long, 180);
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 181);
        assert!(!cut.contains("  "));
        // No whitespace at all: hard cut with ellipsis.
        let solid = "x".repeat(200);
        assert_eq!(Bridge::truncate_synopsis(&solid, 180).chars().count(), 181);
        // Interior line breaks/tabs collapse to single spaces so the card
        // text is genuinely single-line (no multi-line preferred height).
        assert_eq!(
            Bridge::truncate_synopsis("line one\n\nline\ttwo   three", 180),
            "line one line two three"
        );
        let paragraphed = "a\nb ".repeat(100);
        let cut = Bridge::truncate_synopsis(&paragraphed, 120);
        assert!(!cut.contains('\n'));
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn episode_playback_progress_does_not_follow_manual_watched_marks() {
        let key = progress_map_key("series", "episode");
        let mut map = std::collections::HashMap::new();
        assert_eq!(
            Bridge::episode_watch_state(&map, "series", "episode").1,
            0.0
        );
        map.insert(
            key.clone(),
            EpisodeProgress {
                watched: true,
                ..Default::default()
            },
        );
        let (watched, progress, _) = Bridge::episode_watch_state(&map, "series", "episode");
        assert!(watched);
        assert_eq!(progress, 0.0, "a manual watched mark has no playback rail");
        let saved = map.get_mut(&key).unwrap();
        saved.position_secs = 30.0;
        saved.duration_secs = 120.0;
        saved.watched = false;
        assert_eq!(
            Bridge::episode_watch_state(&map, "series", "episode").1,
            0.25
        );
        map.get_mut(&key).unwrap().watched = true;
        assert_eq!(
            Bridge::episode_watch_state(&map, "series", "episode").1,
            1.0
        );
    }

    #[test]
    fn episode_details_counts_resume_suffix_in_budget() {
        // The resume suffix rides outside truncate_synopsis, so the
        // combined string must still fit the 150-char card budget:
        // otherwise the suffix alone could push a borderline
        // synopsis onto a 4th line that the card clips mid-glyph.
        let suffix = " · ▶ Resume 1:23:45";
        let long = "word ".repeat(50); // 250 chars
        let details = Bridge::episode_details(Some(long.as_str()), suffix);
        assert!(details.ends_with(suffix));
        assert!(details.chars().count() <= 150);
        // The truncation ellipsis lands at a word boundary before
        // the suffix, so no mid-word cut abuts the resume text.
        let body = &details[..details.len() - suffix.len()];
        assert!(body.ends_with('…'));
        // No overview at all: just the suffix, no ellipsis.
        assert_eq!(Bridge::episode_details(None, suffix), suffix);
        // Short overview that fits together with the suffix stays
        // whole (no ellipsis added).
        let short = Bridge::episode_details(Some("short overview"), suffix);
        assert_eq!(short, format!("short overview{suffix}"));
        assert!(!short.contains('…'));
    }

    #[test]
    fn series_finished_matches_ended_status() {
        // Only an explicit "Ended" counts as finished; anything else —
        // including a missing status — refreshes, so ongoing shows can
        // never serve stale episode data and posters indefinitely.
        assert!(Bridge::series_finished(Some("Ended")));
        assert!(Bridge::series_finished(Some("ended")));
        assert!(Bridge::series_finished(Some("ENDED")));
        assert!(!Bridge::series_finished(Some("Continuing")));
        assert!(!Bridge::series_finished(Some("Returning Series")));
        assert!(!Bridge::series_finished(Some("")));
        assert!(!Bridge::series_finished(None));
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::super::*;

    /// A scratch directory unique to this test binary/name.
    fn scratch(sub: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-persist-test-{}-{sub}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fnv1a_is_stable_and_distinguishes_urls() {
        // FNV-1a offset basis for the empty input.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        let a = fnv1a(b"https://v3-cinemeta.strem.io/");
        let b = fnv1a(b"https://example.com/stremio/torrentio/");
        assert_ne!(a, b);
        assert_eq!(a, fnv1a(b"https://v3-cinemeta.strem.io/"));
    }

    #[test]
    fn manifest_cache_round_trips_and_deletes() {
        let dir = scratch("manifests");
        let url = "https://v3-cinemeta.strem.io/";
        let manifest: Manifest = serde_json::from_str(
            r#"{"id":"community.cinemeta","version":"3.6.0","name":"Cinemeta",
                "resources":["catalog","meta"],"types":["movie","series"]}"#,
        )
        .unwrap();

        assert!(read_cached_manifest(&dir, url).is_none());
        write_cached_manifest(&dir, url, &manifest);

        let back = read_cached_manifest(&dir, url).expect("cached manifest readable");
        assert_eq!(back.id, "community.cinemeta");
        assert_eq!(back.name, "Cinemeta");
        assert_eq!(back.resources.len(), 2);
        assert!(manifest_cache_path(&dir, url).starts_with(manifest_cache_dir(&dir)));

        delete_cached_manifest(&dir, url);
        assert!(read_cached_manifest(&dir, url).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn poster_bytes_round_trip_and_distinct_urls() {
        let dir = scratch("posters");
        let a = "https://img.example/tt1/img";
        let b = "https://img.example/tt2/img";
        let png = |color| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba(color)))
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            out.into_inner()
        };
        let bytes_a = png([255, 0, 0, 255]);
        let bytes_b = png([0, 0, 255, 255]);

        // Nothing cached yet.
        assert!(read_poster_bytes(&dir, a).is_none());

        write_poster_bytes(&dir, a, &bytes_a);
        write_poster_bytes(&dir, b, &bytes_b);
        assert_eq!(
            read_poster_bytes(&dir, a).as_deref(),
            Some(bytes_a.as_slice())
        );
        assert_eq!(
            read_poster_bytes(&dir, b).as_deref(),
            Some(bytes_b.as_slice())
        );
        // Different URLs map to different cache files.
        assert_ne!(poster_cache_path_in(&dir, a), poster_cache_path_in(&dir, b));
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod library_tests {
    use super::super::*;

    fn entry(id: &str, type_: &str, name: &str, year: &str, poster_url: &str) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            year: year.into(),
            poster_url: poster_url.into(),
            background_url: String::new(),
            genres: Vec::new(),
            description: String::new(),
            categories: Vec::new(),
            watch_status: WatchStatus::Auto,
            added_at_secs: 0,
        }
    }

    fn entry_bg(
        id: &str,
        type_: &str,
        name: &str,
        year: &str,
        poster_url: &str,
        background_url: &str,
    ) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            year: year.into(),
            poster_url: poster_url.into(),
            background_url: background_url.into(),
            genres: Vec::new(),
            description: String::new(),
            categories: Vec::new(),
            watch_status: WatchStatus::Auto,
            added_at_secs: 0,
        }
    }

    fn entry_full(
        id: &str,
        type_: &str,
        name: &str,
        year: &str,
        poster_url: &str,
        background_url: &str,
        header: (&[&str], &str),
    ) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            year: year.into(),
            poster_url: poster_url.into(),
            background_url: background_url.into(),
            genres: header.0.iter().map(|g| g.to_string()).collect(),
            description: header.1.into(),
            categories: Vec::new(),
            watch_status: WatchStatus::Auto,
            added_at_secs: 0,
        }
    }

    #[test]
    fn upsert_dedupes_by_id_and_removal_drops_entry() {
        let mut items = vec![entry("tt1", "movie", "Alpha", "2020", "u1")];
        // Same id overwrites in place (kept position), returns "not added".
        assert!(!upsert_library(
            &mut items,
            entry("tt1", "movie", "Alpha 2", "2021", "u2")
        ));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Alpha 2");
        assert_eq!(items[0].poster_url, "u2");

        // A new id is appended.
        assert!(upsert_library(
            &mut items,
            entry("tt2", "series", "Beta", "", "")
        ));
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].id, "tt2");

        // Removal by id.
        assert!(remove_library_entry(&mut items, "tt1"));
        assert!(!remove_library_entry(&mut items, "tt1"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "tt2");
        let _ = std::mem::take(&mut items);
    }

    #[test]
    fn library_backdrop_url_back_compat_missing_field() {
        // Entries written before the backdrop-URL fix carry no
        // `background_url` key; serde(default) must fill "" without failing.
        let old: Vec<LibraryEntry> = serde_json::from_str(
            r#"[{"id":"tt1","type_":"series","name":"Alpha","year":"2020","poster_url":"u","categories":[]}]"#,
        )
        .expect("old JSON parses");
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].background_url, "");
        assert_eq!(old[0].genres, Vec::<String>::new());
        assert_eq!(old[0].description, "");
        // Upsert with a backdrop URL replaces the empty default in place.
        let mut items = old;
        assert!(!upsert_library(
            &mut items,
            entry_bg("tt1", "series", "Alpha", "2020", "u", "https://img/bg")
        ));
        assert_eq!(items[0].background_url, "https://img/bg");
    }

    #[test]
    fn library_json_round_trips_header_metadata() {
        let items = vec![
            entry_full(
                "tt1",
                "series",
                "Alpha",
                "2020",
                "https://img/a",
                "https://img/bg",
                (&["Drama", "Sci-Fi"], "A synopsis."),
            ),
            entry("tt2", "series", "Beta", "1999", ""),
        ];
        let json = serde_json::to_string(&items).unwrap();
        let back: Vec<LibraryEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, items, "genres + description round-trip");
        assert_eq!(
            back[0].genres,
            vec!["Drama".to_string(), "Sci-Fi".to_string()]
        );
        assert_eq!(back[0].description, "A synopsis.");
        assert!(back[1].genres.is_empty());
        assert_eq!(back[1].description, "");
    }

    #[test]
    fn library_header_text_merge_backfills_empty_only() {
        // Backfill mode fills empty slots but never overwrites stored text
        // (no addon churn on prefetch/entry paints).
        let mut e = entry("tt1", "series", "S", "2020", "p");
        assert!(merge_library_header_text(
            &mut e,
            &["Drama".to_string()],
            "Desc",
            "2021",
            false
        ));
        assert_eq!(e.genres, vec!["Drama".to_string()]);
        assert_eq!(e.description, "Desc");
        // The helper pre-fills year, so backfill leaves the stored value.
        assert_eq!(e.year, "2020");
        // Different fresh text must not displace stored text...
        assert!(!merge_library_header_text(
            &mut e,
            &["Comedy".to_string()],
            "New",
            "2022",
            false
        ));
        assert_eq!(e.genres, vec!["Drama".to_string()]);
        assert_eq!(e.description, "Desc");
        // ...but overwrite mode (unfinished-show refresh) replaces stored
        // text with fresh non-empty values so updated data is saved...
        assert!(merge_library_header_text(
            &mut e,
            &["Comedy".to_string()],
            "New",
            "2022",
            true
        ));
        assert_eq!(e.genres, vec!["Comedy".to_string()]);
        assert_eq!(e.description, "New");
        assert_eq!(e.year, "2022");
        // ...while fresh empty values never blank stored text in either mode.
        assert!(!merge_library_header_text(&mut e, &[], "", "", true));
        assert!(!merge_library_header_text(&mut e, &[], "", "", false));
        assert_eq!(e.description, "New");
        // Identical values report no change (avoids pointless rewrites).
        assert!(!merge_library_header_text(
            &mut e,
            &["Comedy".to_string()],
            "New",
            "2022",
            true
        ));
    }

    #[test]
    fn library_header_text_back_compat_missing_fields() {
        // Entries written before the header-text fix carry no `genres` /
        // `description` keys; serde(default) must fill empties.
        let old: Vec<LibraryEntry> = serde_json::from_str(
            r#"[{"id":"tt1","type_":"series","name":"Alpha","year":"2020","poster_url":"u","background_url":"b","categories":[]}]"#,
        )
        .expect("old JSON parses");
        assert_eq!(old.len(), 1);
        assert!(old[0].genres.is_empty());
        assert_eq!(old[0].description, "");
        let mut items = old;
        assert!(!upsert_library(
            &mut items,
            entry_full(
                "tt1",
                "series",
                "Alpha",
                "2020",
                "u",
                "b",
                (&["Drama"], "A synopsis.")
            )
        ));
        assert_eq!(items[0].genres, vec!["Drama".to_string()]);
        assert_eq!(items[0].description, "A synopsis.");
    }
}

#[cfg(test)]
mod meta_header_tests {
    use super::super::*;

    fn item_with_header() -> MetaItem {
        MetaItem {
            preview: MetaPreview {
                background: Some("https://img/bg".to_string()),
                description: Some("A synopsis.".to_string()),
                genres: vec!["Drama".to_string()],
                release_info: Some(serde_json::Value::String("2020".to_string())),
                ..Default::default()
            },
            extra: HashMap::from([(
                "seasons".into(),
                serde_json::json!([
                    {"season":1, "background":"https://img/season-one"}
                ]),
            )]),
            ..Default::default()
        }
    }

    #[test]
    fn header_snapshot_extracts_meta_fields() {
        let header = meta_header_from_item(&item_with_header());
        assert_eq!(header.background_url, "https://img/bg");
        assert_eq!(header.description, "A synopsis.");
        assert_eq!(header.genres, vec!["Drama".to_string()]);
        assert_eq!(header.year, "2020");
        assert_eq!(
            header.season_backdrops.get(&1).map(String::as_str),
            Some("https://img/season-one")
        );
    }

    #[test]
    fn header_snapshot_empty_when_addon_sends_nothing() {
        let header = meta_header_from_item(&MetaItem::default());
        assert_eq!(header, MetaHeader::default());
        // Fully-empty snapshots never touch the store.
        assert!(!merge_meta_header_for("series", "tt-none", &header));
    }

    #[test]
    fn header_keys_distinguish_type_and_id() {
        let a = meta_header_key("series", "tt1");
        let b = meta_header_key("series", "tt2");
        let c = meta_header_key("movie", "tt1");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn header_json_round_trips() {
        let header = meta_header_from_item(&item_with_header());
        let json = serde_json::to_string(&header).expect("serializes");
        let back: MetaHeader = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, header);
        // Missing keys (older caches) default to empty, never fail.
        let old: MetaHeader = serde_json::from_str("{}").expect("parses");
        assert_eq!(old, MetaHeader::default());
    }
}

#[cfg(test)]
mod episodes_cache_tests {
    use super::super::*;

    fn scratch(sub: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nova-episodes-cache-test-{}-{sub}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn video(id: &str, season: Option<u32>, episode: Option<u32>) -> Video {
        Video {
            id: id.into(),
            name: format!("{id} name"),
            season,
            episode,
            number: episode,
            ..Default::default()
        }
    }

    #[test]
    fn episodes_cache_round_trips_in_order() {
        let dir = scratch("roundtrip");
        let id = "cinemeta:series:tt123";
        let videos = vec![
            video("cinemeta:series:tt123:1:1", Some(1), Some(1)),
            video("cinemeta:series:tt123:1:2", Some(1), Some(2)),
            video("cinemeta:series:tt123:2:1", Some(2), Some(1)),
        ];
        write_cached_episodes(&dir, "series", id, &videos);
        let back = read_cached_episodes(&dir, "series", id).expect("episode list readable");
        assert_eq!(back.len(), videos.len());
        for (got, want) in back.iter().zip(&videos) {
            assert_eq!(got.id, want.id);
            assert_eq!(got.season, want.season);
            assert_eq!(got.episode, want.episode);
            assert_eq!(got.name, want.name);
        }

        // Missing file and corrupt file both read as None without panicking.
        assert!(read_cached_episodes(&dir, "series", "other").is_none());
        assert!(read_cached_episodes(&scratch("none"), "series", id).is_none());
        let path = episodes_cache_path_in(&dir, "series", id);
        fs::write(&path, "{ not json").unwrap();
        assert!(read_cached_episodes(&dir, "series", id).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn episodes_cache_keys_distinguish_type_and_id() {
        let dir = scratch("keys");
        let a = episodes_cache_path_in(&dir, "series", "tt1");
        let b = episodes_cache_path_in(&dir, "series", "tt2");
        let c = episodes_cache_path_in(&dir, "movie", "tt1");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with(&dir));
    }

    #[test]
    fn episodes_cache_ignores_episode_less_meta() {
        // The cache is only ever written with season-filtered lists, but a
        // stray entry (e.g. an older format) must still round-trip harmlessly.
        let dir = scratch("filtered");
        let v = video("special", None, None);
        write_cached_episodes(&dir, "series", "ttX", std::slice::from_ref(&v));
        let back = read_cached_episodes(&dir, "series", "ttX").expect("readable");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, v.id);
        assert_eq!(back[0].season, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filtered_row_index_resolves_ids_and_filters() {
        let videos = vec![
            video("s1e1", Some(1), Some(1)),
            video("s1e2", Some(1), Some(2)),
            video("s2e1", Some(2), Some(1)),
        ];
        // Unfiltered: straight positions within the season.
        assert_eq!(Bridge::filtered_row_index(&videos, 1, "", "s1e1"), Some(0));
        assert_eq!(Bridge::filtered_row_index(&videos, 1, "", "s1e2"), Some(1));
        // Other seasons don't leak in.
        assert_eq!(Bridge::filtered_row_index(&videos, 2, "", "s1e1"), None);
        assert_eq!(Bridge::filtered_row_index(&videos, 1, "", "nope"), None);
        // Name filter narrows first ("s1e2 name" matches, "s1e1 name" doesn't).
        assert_eq!(
            Bridge::filtered_row_index(&videos, 1, "s1e2 name", "s1e2"),
            Some(0)
        );
        assert_eq!(
            Bridge::filtered_row_index(&videos, 1, "s1e2 name", "s1e1"),
            None
        );
        // Inverse mapping agrees.
        assert_eq!(
            Bridge::filtered_row_id(&videos, 1, "", 1),
            Some("s1e2".to_string())
        );
        assert_eq!(Bridge::filtered_row_id(&videos, 1, "", 7), None);
    }
}

#[cfg(test)]
mod settings_tests {
    use super::super::*;

    fn scratch(sub: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-settings-test-{}-{sub}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_json_round_trip_and_defaults() {
        let defaults = CacheSettings::default();
        assert!(!defaults.enabled);
        assert_eq!(defaults.format, CacheImageFormat::Webp);

        let custom = CacheSettings {
            enabled: true,
            format: CacheImageFormat::Jpeg,
            quality: 62,
            downscale: false,
            ..CacheSettings::default()
        };
        let json = serde_json::to_string(&custom).unwrap();
        let back: CacheSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, custom);
        assert_eq!(custom.format.label(), "JPEG");
        assert_ne!(custom.config_key(), CacheSettings::default().config_key());
    }

    #[test]
    fn cache_images_defaults_on_and_backfills() {
        assert!(CacheSettings::default().cache_images);
        // Configs written before the toggle existed carry no key; disk
        // caching (the historical behavior) must apply without failing.
        let old: CacheSettings = serde_json::from_str(
            r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#,
        )
        .expect("old JSON parses");
        assert!(old.cache_images);
        // Explicit off survives the JSON format used in the KV store.
        let off = CacheSettings {
            cache_images: false,
            ..CacheSettings::default()
        };
        let json = serde_json::to_string(&off).unwrap();
        let back: CacheSettings = serde_json::from_str(&json).unwrap();
        assert!(!back.cache_images);
    }

    #[test]
    fn animation_settings_default_on_and_round_trip() {
        let defaults = CacheSettings::default();
        assert!(defaults.animations);
        assert!(defaults.anim_transitions);
        assert!(defaults.anim_hover);
        assert!(defaults.anim_player);
        // Configs written before the animation toggles existed carry no keys;
        // animations (the historical behavior) must apply without failing.
        let old: CacheSettings = serde_json::from_str(
            r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#,
        )
        .expect("old JSON parses");
        assert!(old.animations);
        // Explicit off survives the JSON format used in the KV store.
        let off = CacheSettings {
            animations: false,
            anim_transitions: false,
            anim_hover: true,
            anim_player: false,
            ..CacheSettings::default()
        };
        let json = serde_json::to_string(&off).unwrap();
        let back: CacheSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, off);
    }

    #[test]
    fn player_backend_defaults_to_builtin_and_round_trips() {
        let defaults = CacheSettings::default();
        assert!(!defaults.player_external);
        assert_eq!(
            defaults.desktop_external_app,
            nova_config::DesktopExternalApp::SystemDefault
        );
        // Index mapping round-trips both ways.
        assert_eq!(
            nova_config::DesktopExternalApp::from_index(0),
            nova_config::DesktopExternalApp::SystemDefault
        );
        assert_eq!(
            nova_config::DesktopExternalApp::from_index(1),
            nova_config::DesktopExternalApp::Vlc
        );
        assert_eq!(
            nova_config::DesktopExternalApp::from_index(2),
            nova_config::DesktopExternalApp::Mpv
        );
        assert_eq!(nova_config::DesktopExternalApp::Vlc.index(), 1);
        assert_eq!(nova_config::DesktopExternalApp::Mpv.program(), "mpv");
        // Configs written before these keys existed parse with the defaults.
        let old: CacheSettings = serde_json::from_str(
            r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#,
        )
        .expect("old JSON parses");
        assert!(!old.player_external);
        // Explicit external selection survives the persisted JSON format.
        let ext = CacheSettings {
            player_external: true,
            desktop_external_app: nova_config::DesktopExternalApp::Vlc,
            ..CacheSettings::default()
        };
        let json = serde_json::to_string(&ext).unwrap();
        let back: CacheSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ext);
    }

    #[test]
    fn disk_usage_sums_files_and_ignores_missing_dirs() {
        let dir = scratch("disk-usage");
        // Missing dir reads as empty.
        assert_eq!(poster_cache_disk_usage(&dir), (0, 0));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(poster_cache_disk_usage(&dir), (0, 0));
        fs::write(dir.join("a.img"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("b.img"), vec![0u8; 200]).unwrap();
        fs::write(dir.join("c.d480.jpg"), vec![0u8; 45]).unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();
        fs::write(dir.join("subdir").join("nested.img"), vec![0u8; 9999]).unwrap();
        // Flat walk: 3 files, subdirectories not descended into.
        assert_eq!(poster_cache_disk_usage(&dir), (345, 3));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_cache_dir_empties_and_reports() {
        let dir = scratch("disk-clear");
        // Missing dir reads as nothing to free.
        assert_eq!(clear_poster_cache_dir(&dir), (0, 0));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.img"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("b.d480.jpg"), vec![0u8; 50]).unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();
        assert_eq!(clear_poster_cache_dir(&dir), (150, 2));
        assert_eq!(poster_cache_disk_usage(&dir), (0, 0));
        // Directory itself (and subdirs) survive the wipe.
        assert!(dir.is_dir());
        assert!(dir.join("subdir").is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn android_cache_dir_is_sibling_of_files_dir() {
        // `internal_data_path()` is `getFilesDir()` (`.../files`); the
        // system cache dir (`getCacheDir()`, `.../cache`) is its sibling,
        // not a child. A child would count as "Data" and survive
        // "Clear cache".
        assert_eq!(
            nova_config::android_cache_dir_for(&PathBuf::from("/data/user/0/dev.misob.nova/files")),
            PathBuf::from("/data/user/0/dev.misob.nova/cache")
        );
        // Defensive fallback for unexpected layouts.
        assert_eq!(
            nova_config::android_cache_dir_for(&PathBuf::from("/odd/location")),
            PathBuf::from("/odd/location/cache")
        );
    }

    #[test]
    fn decoded_cache_clear_drops_entries() {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        for b in buf.make_mut_bytes().iter_mut() {
            *b = 7;
        }
        decoded_cache_insert("nova-test-clear", buf);
        assert!(decoded_cache_get("nova-test-clear").is_some());
        decoded_cache_clear();
        assert!(decoded_cache_get("nova-test-clear").is_none());
    }

    #[test]
    fn image_buffers_equal_compares_pixels() {
        let mut a = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        for b in a.make_mut_bytes().iter_mut() {
            *b = 7;
        }
        let mut same = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        for b in same.make_mut_bytes().iter_mut() {
            *b = 7;
        }
        assert!(image_buffers_equal(&a, &same));
        // One byte differs: the refresh path must treat it as changed.
        same.make_mut_bytes()[0] = 8;
        assert!(!image_buffers_equal(&a, &same));
        // Dimensions differ: never equal, even with matching prefixes.
        let other = SharedPixelBuffer::<Rgba8Pixel>::new(2, 3);
        assert!(!image_buffers_equal(&a, &other));
    }

    #[test]
    fn disk_usage_formatting() {
        assert_eq!(format_disk_usage(0, 0), "0 B · 0 files");
        assert_eq!(format_disk_usage(512, 1), "512 B · 1 file");
        assert_eq!(format_disk_usage(2048, 2), "2.0 KB · 2 files");
        assert_eq!(
            format_disk_usage(5 * 1024 * 1024, 1203),
            "5.0 MB · 1,203 files"
        );
        assert_eq!(
            format_disk_usage(3 * 1024 * 1024 * 1024, 7),
            "3.0 GB · 7 files"
        );
        assert_eq!(grouped_count(0), "0");
        assert_eq!(grouped_count(999), "999");
        assert_eq!(grouped_count(1000), "1,000");
    }

    #[test]
    fn show_unwatched_thumbs_defaults_on_and_backfills() {
        assert!(CacheSettings::default().show_unwatched_thumbs);
        // Configs written before the toggle existed carry no key; the
        // default (show) must apply without failing.
        let old: CacheSettings = serde_json::from_str(
            r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#,
        )
        .expect("old JSON parses");
        assert!(old.show_unwatched_thumbs);
        // Visibility rule: only fresh unwatched episodes hide.
        assert!(Bridge::episode_thumb_visible(true, false, 0.0));
        assert!(Bridge::episode_thumb_visible(false, true, 1.0));
        assert!(Bridge::episode_thumb_visible(false, false, 0.5));
        assert!(!Bridge::episode_thumb_visible(false, false, 0.0));
    }

    #[test]
    fn torrent_settings_json_round_trip_and_backfill() {
        // Missing fields in saved KV JSON must backfill from their defaults.
        let custom = TorrentSettings {
            enabled: false,
            dir: "/media/torrents".into(),
            max_mb: 4096,
            down_limit_kbps: 2048,
            no_cache: true,
        };
        let json = serde_json::to_string(&custom).unwrap();
        let back: TorrentSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, custom);

        let partial: TorrentSettings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!partial.enabled);
        assert_eq!(partial.max_mb, 20480);
        assert!(partial.dir.is_empty());
        assert!(!partial.no_cache);
    }
}

#[cfg(test)]
mod stream_source_tests {
    use super::super::*;

    fn stream(
        info_hash: Option<&str>,
        url: Option<&str>,
        yt: Option<&str>,
        file_idx: Option<u32>,
    ) -> addons::Stream {
        addons::Stream {
            info_hash: info_hash.map(str::to_string),
            url: url.map(str::to_string),
            yt_id: yt.map(str::to_string),
            file_idx,
            ..Default::default()
        }
    }

    #[test]
    fn direct_url_maps_to_url() {
        match stream_source(&stream(None, Some("https://x/y.m3u8"), None, None)) {
            StreamSource::Url(u) => assert_eq!(u, "https://x/y.m3u8"),
            _ => panic!("expected Url"),
        }
    }

    #[test]
    fn provider_headers_survive_stream_classification_and_reject_injection() {
        let mut stream = stream(None, Some("https://example.com/video.mp4"), None, None);
        stream.extra.insert(
            "behaviorHints".into(),
            serde_json::json!({"proxyHeaders":{"request":{
                "Referer":"https://example.com/watch", "Accept":"video/mp4, */*",
                "Host":"evil.test", "Bad":"injected\r\nHeader: value"
            }}}),
        );
        let StreamSource::UrlWithOptions {
            url,
            headers,
            subtitles,
        } = stream_source(&stream)
        else {
            panic!("expected URL with headers")
        };
        assert_eq!(url, "https://example.com/video.mp4");
        assert!(subtitles.is_empty());
        assert_eq!(
            headers,
            vec![
                ("Accept".into(), "video/mp4, */*".into()),
                ("Referer".into(), "https://example.com/watch".into())
            ]
        );
    }

    #[test]
    fn external_subtitles_use_in_app_playback_and_reject_native_paths() {
        let mut stream = stream(None, Some("https://example.com/master.m3u8"), None, None);
        stream.subtitles = vec![
            addons::Subtitle {
                id: "eng".into(),
                url: "https://example.com/sub,one.vtt".into(),
                ..Default::default()
            },
            addons::Subtitle {
                id: "file".into(),
                url: "file:///tmp/native.vtt".into(),
                ..Default::default()
            },
            addons::Subtitle {
                id: "invalid".into(),
                url: "https://example.com/sub\0.vtt".into(),
                ..Default::default()
            },
        ];
        let StreamSource::UrlWithOptions {
            headers, subtitles, ..
        } = stream_source(&stream)
        else {
            panic!("expected URL with subtitles")
        };
        assert!(headers.is_empty());
        assert_eq!(subtitles, vec!["https://example.com/sub,one.vtt"]);
    }

    #[test]
    fn youtube_maps_to_watch_url() {
        match stream_source(&stream(None, None, Some("abc"), None)) {
            StreamSource::Url(u) => assert_eq!(u, "https://www.youtube.com/watch?v=abc"),
            _ => panic!("expected Url"),
        }
    }

    #[test]
    fn no_playable_target_is_unsupported() {
        assert!(matches!(
            stream_source(&stream(None, None, None, None)),
            StreamSource::Unsupported
        ));
        // A blank info hash is not a torrent.
        assert!(matches!(
            stream_source(&stream(Some("   "), None, None, None)),
            StreamSource::Unsupported
        ));
    }

    #[test]
    fn bare_infohash_maps_to_torrent_with_file_idx() {
        match stream_source(&stream(Some("deadbeef"), None, None, Some(3))) {
            StreamSource::Torrent {
                info_hash,
                file_idx,
            } => {
                assert_eq!(info_hash, "deadbeef");
                assert_eq!(file_idx, Some(3));
            }
            _ => panic!("expected Torrent"),
        }
    }

    #[test]
    fn direct_url_wins_over_infohash() {
        // A debrid-style row carries both; the direct URL plays without the
        // embedded engine, so it takes precedence on every target.
        match stream_source(&stream(Some("deadbeef"), Some("https://x/y"), None, None)) {
            StreamSource::Url(u) => assert_eq!(u, "https://x/y"),
            _ => panic!("expected Url"),
        }
    }
}

#[cfg(test)]
mod stream_display_tests {
    use super::super::*;

    fn s(name: Option<&str>, title: Option<&str>, description: Option<&str>) -> addons::Stream {
        addons::Stream {
            name: name.map(str::to_string),
            title_legacy: title.map(str::to_string),
            description: description.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn torrentio_shape_shows_title_as_detail() {
        // Torrentio: `name` is the short label, `title` carries the release
        // info, `description` is absent.
        let stream = s(
            Some("Torrentio\n4k"),
            Some("Mushoku Tensei S2 2160p\n👤 42 💾 12.3 GB"),
            None,
        );
        assert_eq!(
            stream_display(&stream),
            "Torrentio\n4k\nMushoku Tensei S2 2160p\n👤 42 💾 12.3 GB"
        );
    }

    #[test]
    fn description_wins_over_legacy_title() {
        let stream = s(Some("1080p"), Some("legacy"), Some("web\nH.264"));
        assert_eq!(stream_display(&stream), "1080p\nweb\nH.264");
    }

    #[test]
    fn legacy_title_is_not_duplicated_when_used_as_label() {
        // No `name`: `label()` already falls back to `title`, so it must not
        // be appended a second time as the detail line.
        let stream = s(None, Some("Only Title"), None);
        assert_eq!(stream_display(&stream), "Only Title");
    }

    #[test]
    fn crlf_is_normalised() {
        let stream = s(Some("Torrentio\r\n4k"), None, Some("a\r\n\r\nb\r"));
        assert_eq!(stream_display(&stream), "Torrentio\n4k\na\nb");
    }

    #[test]
    fn blank_stream_uses_placeholder() {
        assert_eq!(stream_display(&s(None, None, None)), "Stream");
    }
}

#[cfg(all(test, feature = "desktop"))]
mod poster_memory_tests {
    use super::super::*;

    fn buf() -> SharedPixelBuffer<Rgba8Pixel> {
        SharedPixelBuffer::new(10, 10)
    }

    #[test]
    fn evicts_oldest_first_over_cap() {
        let mut s = PosterStore::new(3);
        s.insert((1, 0), buf());
        s.insert((1, 1), buf());
        s.insert((1, 2), buf());
        assert_eq!(s.len(), 3);
        s.insert((1, 3), buf());
        assert_eq!(s.len(), 3);
        assert!(s.get(&(1, 0)).is_none());
        assert!(s.get(&(1, 3)).is_some());
    }

    #[test]
    fn keeps_old_generations_until_cap() {
        // Returning to a previous search/addon must stay instant: a
        // generation bump alone must never evict anything.
        let mut s = PosterStore::new(10);
        s.insert((1, 0), buf());
        s.insert((1, 1), buf());
        s.insert((2, 0), buf());
        assert_eq!(s.len(), 3);
        assert!(s.get(&(1, 0)).is_some());
        assert!(s.get(&(1, 1)).is_some());
        assert!(s.get(&(2, 0)).is_some());
    }

    #[test]
    fn downscale_caps_longest_side_and_keeps_aspect() {
        let big = SharedPixelBuffer::<Rgba8Pixel>::new(1000, 1500);
        let small = downscale_for_display(&big, DISPLAY_POSTER_SIDE);
        assert_eq!((small.width(), small.height()), (320, 480));
        let wide = SharedPixelBuffer::<Rgba8Pixel>::new(1500, 1000);
        let small_wide = downscale_for_display(&wide, DISPLAY_POSTER_SIDE);
        assert_eq!((small_wide.width(), small_wide.height()), (480, 320));
        let tiny = SharedPixelBuffer::<Rgba8Pixel>::new(100, 150);
        let same = downscale_for_display(&tiny, DISPLAY_POSTER_SIDE);
        assert_eq!((same.width(), same.height()), (100, 150));
    }

    #[test]
    fn sized_keys_never_shadow_full_entries() {
        // Display derivatives must not collide with the full-fidelity
        // entry the player backdrop and re-encode path read.
        assert_eq!(sized_cache_key("https://x/y.jpg", None), "https://x/y.jpg");
        assert_eq!(
            sized_cache_key("https://x/y.jpg", Some(480)),
            "https://x/y.jpg#480"
        );
        assert_eq!(
            sized_cache_key("https://x/y.jpg", Some(360)),
            "https://x/y.jpg#360"
        );
    }

    #[test]
    fn display_file_names_dodge_the_img_sweep() {
        // The re-encode sweep processes every `*.img`; derivatives use a
        // distinct suffix so they are never rewritten.
        let name = display_file_name("https://x/y.jpg", 480);
        assert!(name.ends_with(".d480.jpg"), "{name}");
        assert!(!name.ends_with(".img"));
        assert_ne!(name, display_file_name("https://x/y.jpg", 360));
        // Same URL, distinct sizes, distinct files.
        assert_ne!(
            display_file_name("https://x/y.jpg", 480),
            display_file_name("https://x/z.jpg", 480)
        );
    }
}

#[cfg(test)]
mod stream_row_shape_tests {
    use super::super::*;

    fn row(id: &str) -> StreamRow {
        StreamRow {
            id: id.into(),
            text: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn same_ids_in_same_order_update_in_place() {
        // Progress-only ticks keep the same rows in the same order, so the
        // model is refreshed with `set_row_data` and the row delegates (and
        // the Android hold-to-open timer) survive.
        let rows = vec![row("a"), row("download:1")];
        let model = VecModel::from(rows.clone());
        assert!(stream_rows_same_shape(&model, &rows));
    }

    #[test]
    fn reordered_or_resized_rows_force_a_rebuild() {
        let rows = vec![row("a"), row("download:1")];
        let model = VecModel::from(rows.clone());
        // Same ids but different order: the pinned download rows move, so the
        // model must be rebuilt rather than patched in place.
        let reordered = vec![rows[1].clone(), rows[0].clone()];
        assert!(!stream_rows_same_shape(&model, &reordered));
        // Membership change (a new download pinned / one removed): rebuild.
        let grown = vec![row("download:2"), rows[0].clone(), rows[1].clone()];
        assert!(!stream_rows_same_shape(&model, &grown));
        assert!(!stream_rows_same_shape(&model, &rows[..1]));
    }
}

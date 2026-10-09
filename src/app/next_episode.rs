//! Ephemeral end-of-episode offer; selecting a source remains explicit.
use super::*;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

pub(super) fn new_session() -> u64 {
    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
}

fn near_end(position: f64, duration: f64) -> bool {
    position.is_finite()
        && duration.is_finite()
        && duration > 0.0
        && position >= duration - 120.0_f64.min(duration * 0.1)
}

// A successor is relative to the playing episode, never to watched history.
// Don't leap over an unaired immediate successor to offer a later episode.
fn successor<'a>(videos: &'a [Video], current: &str, today: i64) -> Option<&'a Video> {
    let ordered: Vec<_> = ordered_seasons(videos)
        .into_iter()
        .filter(|season| *season > 0)
        .flat_map(|season| season_episodes(videos, season))
        .filter(|v| v.episode_number().is_some_and(|n| n > 0))
        .collect();
    let index = ordered.iter().position(|v| v.id == current)?;
    let next = *ordered.get(index + 1)?;
    episode_is_out(next, today).then_some(next)
}

fn token(session: u64, episode: &str) -> String {
    format!("{session}:{episode}")
}

impl Bridge {
    pub(super) fn clear_next_episode_prompt(&self) {
        if let Some(app) = self.app() {
            app.set_next_episode_visible(false);
            app.set_next_episode_token(SharedString::default());
            app.set_next_episode_thumb(Image::default());
            app.set_next_episode_has_thumb(false);
        }
    }

    pub(super) fn refresh_next_episode_prompt(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_player_open()
            || !app.get_playback_started()
            || !near_end(app.get_position() as f64, app.get_duration() as f64)
        {
            app.set_next_episode_visible(false);
            return;
        }
        let offer = {
            let mut state = self.shared.lock().unwrap();
            let next = state.playback.as_ref().and_then(|playing| {
                if playing.session == 0 || playing.next_episode_dismissed {
                    return None;
                }
                let modal = state
                    .modal_item
                    .as_ref()
                    .filter(|m| m.id == playing.series_id)?;
                successor(&modal.videos, &playing.episode_id, today_days()).cloned()
            });
            next.and_then(|next| {
                let playing = state.playback.as_mut()?;
                let refresh_art = playing.next_episode_id != next.id
                    || playing.next_episode_art_url != next.thumbnail;
                playing.next_episode_id.clone_from(&next.id);
                playing.next_episode_art_url.clone_from(&next.thumbnail);
                Some((playing.session, next, refresh_art))
            })
        };
        let Some((session, next, refresh_art)) = offer else {
            app.set_next_episode_visible(false);
            return;
        };
        let offer_token = token(session, &next.id);
        let changed = app.get_next_episode_token().as_str() != offer_token;
        app.set_next_episode_token(offer_token.clone().into());
        app.set_next_episode_title(episode_row_label(&next).into());
        app.set_next_episode_context(episode_badge(&next).into());
        app.set_next_episode_visible(true);
        if changed || refresh_art {
            app.set_next_episode_thumb(Image::default());
            app.set_next_episode_has_thumb(false);
            if let Some(url) = next.thumbnail.filter(|url| !url.trim().is_empty()) {
                let bridge = self.clone();
                net::fetch_image(url.clone(), None, move |pixels| {
                    let Some(pixels) = pixels else { return };
                    let _ = slint::invoke_from_event_loop(move || {
                        let valid =
                            bridge
                                .shared
                                .lock()
                                .unwrap()
                                .playback
                                .as_ref()
                                .is_some_and(|p| {
                                    p.session == session
                                        && p.next_episode_id == next.id
                                        && p.next_episode_art_url.as_deref() == Some(url.as_str())
                                        && !p.next_episode_dismissed
                                });
                        if valid
                            && let Some(app) = bridge.app()
                            && app.get_player_open()
                            && app.get_next_episode_token().as_str() == offer_token
                        {
                            app.set_next_episode_thumb(Image::from_rgba8(pixels));
                            app.set_next_episode_has_thumb(true);
                        }
                    });
                });
            }
        }
    }

    pub(super) fn dismiss_next_episode(&self, offer_token: &str) {
        let mut state = self.shared.lock().unwrap();
        if let Some(p) = state.playback.as_mut()
            && p.session > 0
            && token(p.session, &p.next_episode_id) == offer_token
        {
            p.next_episode_dismissed = true;
            drop(state);
            self.clear_next_episode_prompt();
        }
    }

    pub(super) fn choose_next_episode_streams(&self, offer_token: &str) {
        let Some(app) = self.app() else { return };
        if !app.get_player_open()
            || !app.get_next_episode_visible()
            || !near_end(app.get_position() as f64, app.get_duration() as f64)
        {
            return;
        }
        let selection = {
            let state = self.shared.lock().unwrap();
            state.playback.as_ref().and_then(|p| {
                if p.session == 0
                    || p.next_episode_dismissed
                    || token(p.session, &p.next_episode_id) != offer_token
                {
                    return None;
                }
                let modal = state.modal_item.as_ref().filter(|m| m.id == p.series_id)?;
                let next = successor(&modal.videos, &p.episode_id, today_days())?;
                (next.id == p.next_episode_id).then(|| (p.series_id.clone(), next.id.clone()))
            })
        };
        let Some((series, episode)) = selection else {
            return;
        };
        // Cache the latest values before close resets the player properties.
        // The normal close callback finalizes history and torrent/brightness state.
        self.note_player_progress_from_ui();
        app.invoke_close_player();
        self.episode_streams_by_id(&series, &episode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, season: u32, number: u32) -> Video {
        Video {
            id: id.into(),
            season: Some(season),
            episode: Some(number),
            ..Default::default()
        }
    }

    #[test]
    fn timing_is_last_two_minutes_capped_at_ten_percent() {
        assert!(!near_end(1319.0, 1440.0));
        assert!(near_end(1320.0, 1440.0));
        assert!(!near_end(539.0, 600.0));
        assert!(near_end(540.0, 600.0));
        assert!(near_end(1440.0, 1440.0));
        for (pos, dur) in [
            (0.0, 0.0),
            (-1.0, 10.0),
            (f64::NAN, 10.0),
            (5.0, f64::INFINITY),
        ] {
            assert!(!near_end(pos, dur));
        }
    }

    #[test]
    fn successor_follows_current_across_seasons_and_ignores_specials() {
        let videos = vec![
            video("s2e1", 2, 1),
            video("s1e2", 1, 2),
            video("special", 0, 1),
            video("s1e1", 1, 1),
        ];
        assert_eq!(successor(&videos, "s1e1", 0).unwrap().id, "s1e2");
        assert_eq!(successor(&videos, "s1e2", 0).unwrap().id, "s2e1");
        for id in ["s2e1", "special", "missing"] {
            assert!(successor(&videos, id, 0).is_none());
        }
    }

    #[test]
    fn unaired_successor_blocks_offer_but_missing_date_is_available() {
        let mut videos = vec![video("one", 1, 1), video("two", 1, 2), video("three", 1, 3)];
        videos[1].released = Some("2099-01-01".into());
        assert!(successor(&videos, "one", today_days()).is_none());
        videos[1].released = None;
        assert_eq!(successor(&videos, "one", today_days()).unwrap().id, "two");
    }

    #[test]
    fn sessions_distinguish_reopening_the_same_episode() {
        assert_ne!(token(new_session(), "same"), token(new_session(), "same"));
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn banner_lifecycle_and_navigation_finalize_the_old_episode() {
        // Slint and redb have process-global initialization. Never let this
        // test write real user history or interfere with other app tests.
        const ROOT: &str = "NOVA_NEXT_EPISODE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-next-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::next_episode::tests::banner_lifecycle_and_navigation_finalize_the_old_episode", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(&root);
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
            Arc::new(Mutex::new(PosterStore::new(1))),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let videos = (1..=55)
            .map(|n| video(&format!("episode-{n}"), 1, n))
            .collect();
        bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "series".into(),
            type_: "series".into(),
            request_id: "episode-50".into(),
            videos,
            season_backdrops: HashMap::new(),
            seasons: vec![1],
            season_index: 0,
            episode_page: 0,
            name: "Series".into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        });
        bridge.shared.lock().unwrap().playback = Some(PlaybackTarget {
            session: new_session(),
            series_id: "series".into(),
            episode_id: "episode-50".into(),
            ..Default::default()
        });
        let close_bridge = bridge.clone();
        app.on_close_player(move || {
            close_bridge.player.close();
            close_bridge.clear_next_episode_prompt();
            close_bridge.note_player_progress_from_ui();
        });
        app.set_player_open(true);
        app.set_duration(1440.0);
        app.set_position(1320.0);
        bridge.refresh_next_episode_prompt();
        assert!(!app.get_next_episode_visible(), "loading cannot offer next");
        app.set_playback_started(true);
        bridge.refresh_next_episode_prompt();
        assert!(app.get_next_episode_visible());
        assert!(!app.get_next_episode_has_thumb());
        let old_token = app.get_next_episode_token();
        bridge.dismiss_next_episode(old_token.as_str());
        app.set_position(200.0);
        bridge.refresh_next_episode_prompt();
        app.set_position(1320.0);
        bridge.refresh_next_episode_prompt();
        assert!(!app.get_next_episode_visible(), "dismiss survives seeking");
        {
            let mut state = bridge.shared.lock().unwrap();
            let p = state.playback.as_mut().unwrap();
            p.session = new_session();
            p.next_episode_dismissed = false;
        }
        bridge.refresh_next_episode_prompt();
        assert!(app.get_next_episode_visible());
        bridge.choose_next_episode_streams(old_token.as_str());
        assert!(
            app.get_player_open(),
            "stale action cannot close a new session"
        );
        app.set_position(200.0);
        bridge.refresh_next_episode_prompt();
        assert!(!app.get_next_episode_visible());
        app.set_position(1321.0);
        bridge.refresh_next_episode_prompt();
        let current_token = app.get_next_episode_token();
        app.set_episode_filter("no matches".into());
        bridge.choose_next_episode_streams(current_token.as_str());
        assert!(!app.get_player_open());
        assert!(!app.get_next_episode_visible());
        assert_eq!(app.get_episode_filter(), "");
        let state = bridge.shared.lock().unwrap();
        let modal = state.modal_item.as_ref().unwrap();
        assert_eq!(modal.request_id, "episode-51");
        assert_eq!(modal.episode_page, 1, "ID routing selects the correct page");
        assert_eq!(state.playback.as_ref().unwrap().episode_id, "episode-51");
        assert_eq!(
            state.progress[&progress_map_key("series", "episode-50")].position_secs,
            1321.0
        );
        drop(state);
        let saved = read_progress_map();
        assert_eq!(
            saved[&progress_map_key("series", "episode-50")].position_secs,
            1321.0
        );
        bridge.choose_next_episode_streams(current_token.as_str());
        assert!(
            !app.get_player_open(),
            "duplicate actions never start playback"
        );
    }
}

//! Profile-owned, listing-only season planning. The queue is touched only by
//! confirm, after every captured source authority has been validated.
use super::*;

#[derive(Clone)]
pub(super) struct Plan {
    context: profiles::Context,
    opening: Arc<()>,
    media_type: String,
    media_id: String,
    title: String,
    year: String,
    original_request: String,
    view_generation: u64,
    providers: Vec<(String, u64)>,
    settings: u64,
    episodes: Vec<Video>,
    selected: Vec<StreamUi>,
    primary: Option<StreamPreference>,
    secondary: Option<StreamPreference>,
    review: bool,
    replacement: Option<DownloadJob>,
}

pub(super) fn downloadable(row: &StreamUi) -> bool {
    row.download.is_none()
        && match &row.source {
            StreamSource::Url(url) | StreamSource::UrlWithOptions { url, .. } => {
                crate::download::validate_download_url(url).is_ok()
            }
            StreamSource::Torrent { .. } => {
                active_torrent_settings().enabled && crate::torrent::engine().is_some()
            }
            StreamSource::Resolver { .. } => true,
            _ => false,
        }
}

/// Repeated pack hashes with no file index cannot prove a different episode.
/// Do not fetch torrent metadata just to disambiguate a proposed plan.
fn duplicate_source(a: &StreamUi, b: &StreamUi) -> bool {
    match (&a.source, &b.source) {
        (
            StreamSource::Url(a) | StreamSource::UrlWithOptions { url: a, .. },
            StreamSource::Url(b) | StreamSource::UrlWithOptions { url: b, .. },
        ) => a == b,
        (
            StreamSource::Torrent {
                info_hash: a,
                file_idx: ai,
            },
            StreamSource::Torrent {
                info_hash: b,
                file_idx: bi,
            },
        ) => a.eq_ignore_ascii_case(b) && (ai == bi || ai.is_none() || bi.is_none()),
        _ => false,
    }
}

fn listing_bytes(row: &StreamUi) -> Option<u64> {
    // Never turn rounded UI text into an exact byte count. videoSize is the
    // provider's selected-file hint; other listing sizes may describe a pack.
    row.card
        .listing_bytes
        .or_else(|| advertised_bytes(&row.card.size))
}

fn advertised_bytes(label: &str) -> Option<u64> {
    if label.len() > 64 {
        return None;
    }
    let label = label.trim();
    let split = label.find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',')?;
    let number = &label[..split];
    if number.is_empty() || number.bytes().filter(|b| *b == b'.' || *b == b',').count() > 1 {
        return None;
    }
    let value = number.replace(',', ".").parse::<f64>().ok()?;
    let factor: f64 = match label[split..].trim().to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0_f64.powi(4),
        _ => return None,
    };
    let bytes = value * factor;
    (bytes.is_finite() && bytes > 0.0 && bytes < u64::MAX as f64).then_some(bytes.round() as u64)
}

impl Plan {
    #[cfg(test)]
    pub(super) fn catalog_scope_test_state(&self, state: &Shared) -> (Vec<String>, bool) {
        (
            self.episodes.iter().map(|video| video.id.clone()).collect(),
            self.current(state),
        )
    }
    fn current(&self, state: &Shared) -> bool {
        profiles::is_current(&self.context)
            && self.view_generation == state.catalog_detail.generation()
            && state.modal_item.as_ref().is_some_and(|m| {
                catalog_detail::ready(state, m)
                    && Arc::ptr_eq(&m.open_token, &self.opening)
                    && m.id == self.media_id
                    && m.type_ == self.media_type
            })
            && self.settings == nova_providers::plugin_settings::settings_generation()
            && self.providers
                == state
                    .installed
                    .iter()
                    .filter(|a| a.enabled && a.available)
                    .map(|a| (a.url.clone(), a.generation))
                    .collect::<Vec<_>>()
    }
}

impl Bridge {
    pub(super) fn season_download_is_replacement(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .season_download
            .as_ref()
            .is_some_and(|p| p.replacement.is_some())
    }

    pub(super) fn season_download_retry(&self) {
        if self
            .shared
            .lock()
            .unwrap()
            .season_download
            .as_ref()
            .is_some_and(|p| !p.review)
        {
            self.season_download_next();
        }
    }

    pub(super) fn reselect_failed_download(&self, job: DownloadJob) {
        if job.phase != crate::download::DownloadPhase::Failed {
            return;
        }
        self.open_preview(
            MetaPreview {
                id: job.media_id.clone(),
                type_: job.media_type.clone(),
                name: job.title.clone(),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        let plan = {
            let mut state = self.shared.lock().unwrap();
            let providers = state
                .installed
                .iter()
                .filter(|a| a.enabled && a.available)
                .map(|a| (a.url.clone(), a.generation))
                .collect();
            let Some(m) = state.modal_item.as_mut() else {
                return;
            };
            let video = m
                .videos
                .iter()
                .find(|v| v.id == job.request_id)
                .cloned()
                .or_else(|| {
                    read_episodes_cache_for(&job.media_type, &job.media_id)
                        .and_then(|rows| rows.into_iter().find(|v| v.id == job.request_id))
                })
                .unwrap_or_else(|| Video {
                    id: job.request_id.clone(),
                    name: job.request_id.clone(),
                    ..Default::default()
                });
            if !m.videos.iter().any(|v| v.id == video.id) {
                m.videos.push(video.clone());
            }
            let mut context = profiles::capture();
            context.session = None;
            Plan {
                context,
                opening: m.open_token.clone(),
                media_type: job.media_type.clone(),
                media_id: job.media_id.clone(),
                title: job.title.clone(),
                year: job.year.clone().unwrap_or_default(),
                original_request: m.request_id.clone(),
                view_generation: state.catalog_detail.generation(),
                providers,
                settings: nova_providers::plugin_settings::settings_generation(),
                episodes: vec![video],
                selected: vec![],
                primary: None,
                secondary: None,
                review: false,
                replacement: Some(job),
            }
        };
        self.shared.lock().unwrap().season_download = Some(plan);
        if let Some(app) = self.app() {
            app.set_season_download_active(true);
            app.set_modal_episodes(false);
            app.set_stream_selector_open(true);
            app.set_detail_deep_stream(false);
            app.set_detail_tab(3);
        }
        self.season_download_next();
    }

    pub(super) fn cancel_season_download(&self) {
        if let Some(app) = self.app() {
            app.global::<nova_ui_shared::DetailBridge>()
                .set_season_download_menu(-1);
        }
        let old = self.shared.lock().unwrap().season_download.take();
        if let Some(plan) = old {
            let mut state = self.shared.lock().unwrap();
            state.stream_generation = state.stream_generation.wrapping_add(1);
            if let Some(m) = state
                .modal_item
                .as_mut()
                .filter(|m| Arc::ptr_eq(&m.open_token, &plan.opening))
            {
                m.request_id = plan.original_request;
            }
        }
        if let Some(app) = self.app() {
            app.set_season_download_active(false);
            app.set_season_download_review(false);
            app.set_season_download_status(Default::default());
            app.set_season_download_rows(Default::default());
        }
    }

    pub(super) fn season_download_start(&self, season_index: usize, unwatched: bool) {
        self.cancel_season_download();
        self.cancel_watch_now_for_season_download();
        let jobs = self.downloads.jobs();
        let plan = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            let visible = catalog_detail::seasons(&state, m);
            let Some(season) = visible.get(season_index) else {
                return;
            };
            let mut seen = HashSet::new();
            let episodes: Vec<_> = catalog_detail::episodes(&state, m, *season)
                .into_iter()
                .filter(|v| {
                    !v.id.trim().is_empty()
                        && v.episode_number().is_some_and(|n| n > 0)
                        && episode_is_out(v, today_days())
                })
                .filter(|v| {
                    !unwatched
                        || !state
                            .progress
                            .get(&progress_map_key(&m.id, &v.id))
                            .is_some_and(|p| p.watched)
                })
                .filter(|v| {
                    !jobs.iter().any(|j| {
                        j.media_type == m.type_
                            && j.media_id == m.id
                            && j.request_id == v.id
                            && j.phase != crate::download::DownloadPhase::Failed
                    })
                })
                .filter(|v| seen.insert(v.id.clone()))
                .cloned()
                .collect();
            if episodes.is_empty() || episodes.len() > 500 {
                let message = if episodes.is_empty() {
                    "No eligible episodes to download."
                } else {
                    "Season download planning supports up to 500 episodes."
                };
                drop(state);
                if let Some(app) = self.app() {
                    app.set_season_download_status(text::tr(message).into());
                }
                return;
            }
            let mut context = profiles::capture();
            context.session = None; // Shared is owned by this session: never retain its Arc here.
            Plan {
                context,
                opening: m.open_token.clone(),
                media_type: m.type_.clone(),
                media_id: m.id.clone(),
                title: m.name.clone(),
                year: m.year.clone(),
                original_request: m.request_id.clone(),
                view_generation: state.catalog_detail.generation(),
                providers: state
                    .installed
                    .iter()
                    .filter(|a| a.enabled && a.available)
                    .map(|a| (a.url.clone(), a.generation))
                    .collect(),
                settings: nova_providers::plugin_settings::settings_generation(),
                episodes,
                selected: vec![],
                primary: None,
                secondary: None,
                review: false,
                replacement: None,
            }
        };
        self.shared.lock().unwrap().season_download = Some(plan);
        if let Some(app) = self.app() {
            app.set_season_download_active(true);
            app.set_modal_episodes(false);
            app.set_stream_selector_open(true);
            app.set_detail_deep_stream(false);
            app.set_detail_tab(3);
        }
        self.season_download_next();
    }

    fn cancel_watch_now_for_season_download(&self) {
        if let Some(m) = self.shared.lock().unwrap().modal_item.as_mut() {
            m.pending_watch_now = None;
        }
    }

    fn season_download_next(&self) {
        let next = {
            let mut state = self.shared.lock().unwrap();
            let Some(plan) = state.season_download.as_mut() else {
                return;
            };
            plan.episodes.get(plan.selected.len()).cloned()
        };
        if let Some(video) = next {
            if let Some(app) = self.app() {
                app.set_season_download_status(text::tr("Select a stream for download").into());
                apply_stream_episode_context(&app, Some(&video));
            }
            self.start_stream_search(video.id);
            // A season with no stream providers has no completion callback.
            self.season_download_list_ready();
        } else {
            let (rows, known, unknown) = {
                let mut state = self.shared.lock().unwrap();
                let Some(plan) = state.season_download.as_mut() else {
                    return;
                };
                plan.review = true;
                let mut known = 0u64;
                let mut unknown = 0usize;
                let rows = plan
                    .episodes
                    .iter()
                    .zip(&plan.selected)
                    .map(|(v, row)| {
                        let bytes = listing_bytes(row);
                        if let Some(n) = bytes {
                            if let Some(sum) = known.checked_add(n) {
                                known = sum;
                            } else {
                                unknown += 1;
                            }
                        } else {
                            unknown += 1;
                        }
                        format!(
                            "{} · {} · ~{}",
                            episode_row_label(v),
                            row.card.title,
                            bytes
                                .map(crate::download::format_bytes)
                                .unwrap_or_else(|| text::tr("Unknown size").into())
                        )
                    })
                    .map(SharedString::from)
                    .collect::<Vec<_>>();
                (rows, known, unknown)
            };
            let free = self.downloads.available_bytes();
            if let Some(app) = self.app() {
                app.set_season_download_rows(Rc::new(VecModel::from(rows)).into());
                app.set_season_download_status(
                    text::season_download_summary(known, unknown, free).into(),
                );
                app.set_season_download_review(true);
            }
        }
    }

    pub(super) fn season_download_list_ready(&self) {
        let selection = {
            let state = self.shared.lock().unwrap();
            let Some(plan) = state.season_download.as_ref() else {
                return;
            };
            if !plan.current(&state) || plan.review || !state.stream_pending.is_empty() {
                return;
            }
            let Some(current) = plan.episodes.get(plan.selected.len()) else {
                return;
            };
            if state
                .modal_item
                .as_ref()
                .is_none_or(|m| m.request_id != current.id)
            {
                return;
            }
            let mut rows: Vec<_> = state
                .stream_all
                .iter()
                .filter(|r| {
                    downloadable(r) && !plan.selected.iter().any(|s| duplicate_source(s, r))
                })
                .cloned()
                .collect();
            let rules = stream_filters::effective_rules(&state.cache_settings);
            stream_filters::apply_stream_rules(&mut rows, &rules);
            let rows = stream_filters::preferred_stream_rules(&rows, &rules);
            plan.primary
                .as_ref()
                .and_then(|p| recommended_stream(rows, None, p))
                .or_else(|| {
                    plan.secondary
                        .as_ref()
                        .and_then(|p| recommended_stream(rows, None, p))
                })
                .map(|i| rows[i].clone())
        };
        if let Some(row) = selection {
            self.season_download_accept(row, false);
        } else if let Some(app) = self.app()
            && self
                .shared
                .lock()
                .unwrap()
                .season_download
                .as_ref()
                .is_some_and(|p| p.primary.is_some())
        {
            app.set_season_download_status(
                text::tr("No matching release. Select a stream for this episode.").into(),
            );
        }
    }

    pub(super) fn season_download_pick(&self, index: usize) -> bool {
        let state = self.shared.lock().unwrap();
        if state.season_download.is_none() {
            return false;
        }
        let row = state.streams.get(index).cloned();
        drop(state);
        if let Some(row) = row {
            self.season_download_accept(row, true);
        }
        true
    }

    pub(super) fn season_download_pick_row(&self, row: StreamUi) -> bool {
        if self.shared.lock().unwrap().season_download.is_none() {
            return false;
        }
        self.season_download_accept(row, true);
        true
    }

    fn season_download_accept(&self, row: StreamUi, manual: bool) {
        let accepted = {
            let mut state = self.shared.lock().unwrap();
            let Some(plan) = state.season_download.as_ref() else {
                return;
            };
            if !plan.current(&state) || plan.review {
                return;
            }
            if !stream_filters::effective_rules(&state.cache_settings)
                .accepts(&row.card.filter_facts)
            {
                drop(state);
                if let Some(app) = self.app() {
                    app.set_season_download_status(
                        text::tr("This source no longer matches your stream rules.").into(),
                    );
                }
                return;
            }
            if !downloadable(&row) {
                return;
            }
            if plan.selected.iter().any(|s| duplicate_source(s, &row)) {
                drop(state);
                if let Some(app) = self.app() {
                    app.set_season_download_status(text::tr("This source does not identify a different episode. Choose another stream.").into());
                }
                return;
            }
            if !state
                .stream_all
                .iter()
                .any(|candidate| candidate.id == row.id && candidate.choice_id == row.choice_id)
            {
                return;
            }
            let plan = state.season_download.as_mut().unwrap();
            if plan.primary.is_none() {
                plan.primary = Some(StreamPreference::from_row(&row));
            } else if manual {
                plan.secondary = Some(StreamPreference::from_row(&row));
            }
            plan.selected.push(row);
            true
        };
        if accepted {
            self.season_download_next();
        }
    }

    pub(super) fn season_download_confirm(&self) {
        let matches_rules = {
            let state = self.shared.lock().unwrap();
            state.season_download.as_ref().is_none_or(|plan| {
                plan.selected.iter().all(|row| {
                    stream_filters::effective_rules(&state.cache_settings)
                        .accepts(&row.card.filter_facts)
                })
            })
        };
        if !matches_rules {
            if let Some(app) = self.app() {
                app.set_season_download_status(
                    text::tr(
                        "Selected sources no longer match your stream rules. Choose streams again.",
                    )
                    .into(),
                );
            }
            return;
        }
        let valid = {
            let state = self.shared.lock().unwrap();
            state.season_download.as_ref().is_some_and(|p| {
                p.review
                    && p.current(&state)
                    && p.selected.len() == p.episodes.len()
                    && p.episodes.iter().all(|v| {
                        state.modal_item.as_ref().is_some_and(|m| {
                            (p.replacement.is_some() && !m.videos.iter().any(|now| now.id == v.id))
                                || m.videos.iter().any(|now| {
                                    now.id == v.id
                                        && now.season == v.season
                                        && now.episode_number() == v.episode_number()
                                        && episode_is_out(now, today_days())
                                })
                        })
                    })
            })
        };
        if !valid {
            if let Some(app) = self.app() {
                app.set_season_download_status(
                    text::tr("This download plan is out of date. Cancel and start again.").into(),
                );
            }
            return;
        }
        let plan = self
            .shared
            .lock()
            .unwrap()
            .season_download
            .as_ref()
            .unwrap()
            .clone();
        let known = plan
            .selected
            .iter()
            .filter_map(listing_bytes)
            .try_fold(0u64, |sum, n| sum.checked_add(n));
        // A total beyond u64 cannot fit on any filesystem this API can
        // represent. Do not let overflow turn the capacity check into a bypass.
        if known.is_none()
            || self
                .downloads
                .available_bytes()
                .zip(known)
                .is_some_and(|(free, known)| known > free)
        {
            if let Some(app) = self.app() {
                app.set_season_download_status(
                    text::tr("Not enough free space for the known download size.").into(),
                );
            }
            return;
        }
        if plan
            .replacement
            .as_ref()
            .is_some_and(|old| self.downloads.job(&old.id).as_ref() != Some(old))
        {
            if let Some(app) = self.app() {
                app.set_season_download_status(
                    text::tr("This download plan is out of date. Cancel and start again.").into(),
                );
            }
            return;
        }
        // Validate every resolver authority before admitting even the first job.
        // Capturing a capability does not invoke its resolver or access the network.
        let prepared: Option<Vec<_>> = plan
            .selected
            .iter()
            .map(|r| {
                downloadable(r)
                    .then(|| self.prepare_download_source(r))
                    .flatten()
            })
            .collect();
        let Some(prepared) = prepared else {
            if let Some(app) = self.app() {
                app.set_season_download_status(
                    text::tr("This download plan is out of date. Cancel and start again.").into(),
                );
            }
            return;
        };
        for ((video, row), prepared) in plan.episodes.iter().zip(&plan.selected).zip(prepared) {
            // Another normal download may have completed while reviewing.
            if self.downloads.jobs().iter().any(|j| {
                j.media_type == plan.media_type
                    && j.media_id == plan.media_id
                    && j.request_id == video.id
                    && j.phase != crate::download::DownloadPhase::Failed
            }) {
                continue;
            }
            let id = self.enqueue_season_download(
                row,
                (
                    &plan.media_type,
                    &plan.media_id,
                    &video.id,
                    &plan.title,
                    &plan.year,
                ),
                prepared,
            );
            if let Some(old) = &plan.replacement
                && old.id != id
                && self.downloads.job(&old.id).as_ref() == Some(old)
            {
                self.downloads.remove(&old.id);
            }
        }
        // Restore the browse request even though the queue retains each captured
        // episode identity and its device/account owner independently.
        if let Some(m) = self.shared.lock().unwrap().modal_item.as_mut() {
            m.request_id = plan.original_request;
        }
        self.cancel_season_download();
        if plan.replacement.is_some() {
            if let Some(app) = self.app() {
                app.set_modal_visible(false);
            }
            self.modal_closed();
            self.refresh_downloads_page();
        } else {
            self.episodes_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(source: StreamSource) -> StreamUi {
        StreamUi {
            id: "x".into(),
            choice_id: Some("x".into()),
            owner_url: "addon".into(),
            display: String::new(),
            card: StreamCardText::default(),
            source,
            addon: String::new(),
            download: None,
        }
    }
    #[test]
    fn season_pack_needs_distinct_explicit_file_indices() {
        let torrent = |file_idx| {
            row(StreamSource::Torrent {
                info_hash: "abc".into(),
                file_idx,
            })
        };
        assert!(duplicate_source(&torrent(None), &torrent(None)));
        assert!(duplicate_source(&torrent(Some(1)), &torrent(None)));
        assert!(duplicate_source(&torrent(Some(1)), &torrent(Some(1))));
        assert!(duplicate_source(
            &torrent(Some(1)),
            &row(StreamSource::Torrent {
                info_hash: "ABC".into(),
                file_idx: None,
            })
        ));
        assert!(!duplicate_source(&torrent(Some(1)), &torrent(Some(2))));
    }
    #[test]
    fn estimates_use_listing_numbers_and_prefer_selected_file_size() {
        let mut row = row(StreamSource::Url("https://example.test/video.mp4".into()));
        row.card.size = "2 GB".into();
        assert_eq!(listing_bytes(&row), Some(2_000_000_000));
        row.card.listing_bytes = Some(1000);
        assert_eq!(listing_bytes(&row), Some(1000));
    }

    #[test]
    fn human_size_estimates_are_bounded_and_reject_rates_or_ambiguous_numbers() {
        assert_eq!(advertised_bytes("1.5 GiB"), Some(1_610_612_736));
        assert_eq!(advertised_bytes("1,5 GB"), Some(1_500_000_000));
        for invalid in [
            "-1 GB",
            "NaN GB",
            "inf GB",
            "1.2.3 GB",
            "1 GB/s",
            "999999999999999999999999 TB",
            "0 MB",
            "1080p",
        ] {
            assert_eq!(advertised_bytes(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn season_plan_keeps_primary_and_latest_outlier_without_resolving_or_enqueuing() {
        const CHILD: &str = "NOVA_SEASON_PLAN_TEST_ROOT";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-season-plan-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::season_downloads::tests::season_plan_keeps_primary_and_latest_outlier_without_resolving_or_enqueuing", "--nocapture"])
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
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );
        let mut videos: Vec<_> = (1..=6)
            .map(|n| Video {
                id: format!("show:1:{n}"),
                name: format!("Episode {n}"),
                season: Some(1),
                episode: Some(n),
                ..Default::default()
            })
            .collect();
        videos.push(Video {
            id: "future".into(),
            season: Some(1),
            episode: Some(7),
            released: Some("2999-01-01".into()),
            ..Default::default()
        });
        videos.push(Video {
            id: "placeholder".into(),
            season: Some(1),
            ..Default::default()
        });
        videos.push(Video {
            id: "watched".into(),
            season: Some(1),
            episode: Some(8),
            ..Default::default()
        });
        videos.push(videos[0].clone());
        {
            let mut state = bridge.shared.lock().unwrap();
            state.modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: "show".into(),
                type_: "series".into(),
                request_id: "show".into(),
                videos,
                season_backdrops: HashMap::new(),
                seasons: vec![1],
                season_index: 0,
                episode_page: 0,
                name: "Show".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: vec![],
            });
            state.progress.insert(
                progress_map_key("show", "watched"),
                EpisodeProgress {
                    watched: true,
                    ..Default::default()
                },
            );
        }
        app.set_modal_visible(true);
        let prior_choice = bridge.last_stream.identity(now_secs());
        bridge.season_download_start(0, true);
        {
            let state = bridge.shared.lock().unwrap();
            let plan = state.season_download.as_ref().unwrap();
            assert_eq!(
                plan.episodes.len(),
                6,
                "future, placeholders, watched and duplicate identities are excluded"
            );
            assert!(
                plan.context.session.is_none(),
                "a profile-owned plan cannot retain its session Arc"
            );
        }
        let stream = |family: &str, episode: usize| {
            let mut result = row(StreamSource::Resolver {
                provider_url: "not-installed".into(),
                token: format!("{family}-{episode}"),
                settings_generation: 0,
                addon_generation: 0,
            });
            result.id = format!("{family}-{episode}");
            result.choice_id = Some(result.id.clone());
            result.card.title = format!("Show {family} episode {episode}");
            result.card.binge_group = Some(family.into());
            result
        };
        let publish = |rows: Vec<StreamUi>| {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_all = rows.clone();
            state.streams = rows;
            state.stream_pending.clear();
        };
        publish(vec![stream("primary", 1)]);
        assert!(bridge.season_download_pick(0));
        let primary = bridge
            .shared
            .lock()
            .unwrap()
            .season_download
            .as_ref()
            .unwrap()
            .primary
            .clone();
        publish(vec![stream("beta", 2)]);
        bridge.season_download_list_ready();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .season_download
                .as_ref()
                .unwrap()
                .selected
                .len(),
            1,
            "an unrelated top stream is never selected"
        );
        bridge.season_download_pick(0);
        publish(vec![stream("beta", 3)]);
        bridge.season_download_list_ready();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .season_download
                .as_ref()
                .unwrap()
                .selected
                .len(),
            3,
            "latest manually chosen fallback is reusable"
        );
        publish(vec![stream("gamma", 4)]);
        bridge.season_download_pick(0);
        publish(vec![stream("beta", 5)]);
        bridge.season_download_list_ready();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .season_download
                .as_ref()
                .unwrap()
                .selected
                .len(),
            4,
            "an earlier outlier is no longer a fallback"
        );
        publish(vec![stream("gamma", 5)]);
        bridge.season_download_pick(0);
        publish(vec![stream("gamma", 6), stream("primary", 6)]);
        bridge.season_download_list_ready();
        {
            let state = bridge.shared.lock().unwrap();
            let plan = state.season_download.as_ref().unwrap();
            assert!(plan.review);
            assert!(
                plan.primary == primary,
                "outlier choices never replace the primary baseline"
            );
            assert_eq!(
                plan.selected.last().unwrap().id,
                "primary-6",
                "primary matches win over a newer fallback"
            );
        }
        assert!(
            bridge.downloads.jobs().is_empty(),
            "even deferred resolver rows cause no queue admission before Confirm"
        );
        assert_eq!(
            bridge.last_stream.identity(now_secs()),
            prior_choice,
            "planning does not save playback choices"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            let plan = state.season_download.as_mut().unwrap();
            plan.selected[0].card.listing_bytes = Some(u64::MAX);
            plan.selected[1].card.listing_bytes = Some(1);
        }
        bridge.season_download_confirm();
        assert!(bridge.downloads.jobs().is_empty());
        assert_eq!(
            app.get_season_download_status(),
            text::tr("Not enough free space for the known download size.")
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            for row in &mut state.season_download.as_mut().unwrap().selected {
                row.card.listing_bytes = None;
            }
        }
        // Captured provider capability cannot be prepared: confirmation retains
        // the complete review without enqueuing any earlier episode.
        bridge.season_download_confirm();
        assert!(bridge.downloads.jobs().is_empty());
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .season_download
                .as_ref()
                .unwrap()
                .review
        );
        bridge.stream_selector_back();
        assert!(bridge.shared.lock().unwrap().season_download.is_none());
        assert!(bridge.downloads.jobs().is_empty());
        assert!(!app.get_season_download_active());
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .request_id,
            "show"
        );
    }

    #[test]
    fn cancelled_replacement_keeps_failed_artifact_and_confirmed_replacement_cleans_it() {
        const CHILD: &str = "NOVA_SEASON_REPLACEMENT_TEST_ROOT";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-season-replacement-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root).env("XDG_DATA_HOME", root.join("data")).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::season_downloads::tests::cancelled_replacement_keeps_failed_artifact_and_confirmed_replacement_cleans_it", "--nocapture"])
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
        use std::io::{Read, Write};
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let artifact = root.join("downloads/http/old/video.mp4");
        let partial = crate::download::part_path(&artifact);
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(&partial, b"failed partial").unwrap();
        let mut failed = DownloadJob::new(
            "old",
            crate::download::DownloadSource::http("http://127.0.0.1:1/old.mp4"),
        );
        failed.media_type = "series".into();
        failed.media_id = "show".into();
        failed.request_id = "show:1:1".into();
        failed.title = "Show".into();
        failed.phase = crate::download::DownloadPhase::Failed;
        failed.artifact_path = Some(artifact);
        failed.bytes_downloaded = 14;
        let mut manifest = crate::download::DownloadManifest::default();
        manifest.jobs.push(failed);
        crate::storage::set_str("downloads:v1", &manifest.to_json().unwrap());
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        let failed = downloads.job("old").unwrap();
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );
        bridge.reselect_failed_download(failed.clone());
        bridge.stream_selector_back();
        assert_eq!(bridge.downloads.job("old").as_ref(), Some(&failed));
        assert!(
            partial.is_file(),
            "cancelling replacement never removes the failed partial"
        );

        // Reselecting the same URL re-arms the existing job ID. Cleanup must
        // never remove that ID merely because it was the failed predecessor.
        bridge.reselect_failed_download(failed.clone());
        let retry = row(StreamSource::Url("http://127.0.0.1:1/old.mp4".into()));
        {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_all = vec![retry.clone()];
            state.streams = vec![retry];
            state.stream_pending.clear();
        }
        bridge.season_download_pick(0);
        bridge.season_download_confirm();
        assert!(
            bridge.downloads.job("old").is_some(),
            "same-source reselection retains its original queue identity"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if bridge
                .downloads
                .job("old")
                .is_some_and(|j| j.phase == crate::download::DownloadPhase::Failed)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "local connection refusal must finalize the retry"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let failed = bridge.downloads.job("old").unwrap();
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(&partial, b"retry partial").unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/replacement.mp4", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0u8; 8192];
            let count = socket.read(&mut request).unwrap();
            server_requests.fetch_add(1, Ordering::SeqCst);
            assert!(
                String::from_utf8_lossy(&request[..count]).starts_with("GET "),
                "confirmation starts the existing transfer directly, never a HEAD probe"
            );
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata").unwrap();
            sent.send(()).unwrap();
        });
        bridge.reselect_failed_download(failed.clone());
        let mut source = row(StreamSource::Url(url));
        source.card.listing_bytes = Some(4);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_all = vec![source.clone()];
            state.streams = vec![source];
            state.stream_pending.clear();
        }
        bridge.season_download_pick(0);
        assert!(app.get_season_download_review());
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert_eq!(bridge.downloads.jobs(), vec![failed.clone()]);
        assert!(partial.is_file());
        bridge.season_download_confirm();
        assert!(
            bridge.downloads.job("old").is_none(),
            "only confirmed different-source replacement retires the failed attempt"
        );
        assert!(
            !partial.exists(),
            "coordinator cleanup owns the old partial"
        );
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(bridge.downloads.jobs().len(), 1);
        assert_eq!(bridge.downloads.jobs()[0].request_id, "show:1:1");
        assert_ne!(bridge.downloads.jobs()[0].id, "old");
    }
}

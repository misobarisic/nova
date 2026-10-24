//! Device-local downloaded library projection. All mutations stay in the
//! download coordinator, which owns artifact lifetime and shared torrents.
use super::*;
use crate::download::{DownloadPhase, format_bytes};
use crate::{DownloadEpisode, DownloadGroup};
use std::collections::BTreeMap;

/// Remember the inputs that produced the retained page models. Artwork has
/// its own direct deliveries and retry scheduler, independent of this cache.
#[derive(Default)]
pub(super) struct RefreshState {
    last: Option<RefreshSignature>,
    #[cfg(test)]
    projections: usize,
}

impl RefreshState {
    pub(super) fn invalidate(&mut self) {
        self.last = None;
    }
}

#[derive(PartialEq)]
struct RefreshRevisions {
    projection: u64,
    downloads: u64,
    images: u64,
    profile: (String, u64),
    language: Language,
    day: i64,
    artwork_allowed: bool,
    spoiler_protection: bool,
}

struct RefreshSignature {
    revisions: RefreshRevisions,
    query: String,
    filter: i32,
    recent: bool,
    collapsed: HashSet<String>,
}

fn episode_numbers(request: &str) -> Option<(u32, u32)> {
    let mut parts = request.rsplit(':');
    let episode = parts.next()?.parse().ok()?;
    let season = parts.next()?.parse().ok()?;
    Some((season, episode))
}

fn cached_episode_numbers(request: &str, videos: &[Video]) -> Option<(u32, u32)> {
    videos
        .iter()
        .find(|video| video.id == request)
        .and_then(|video| video.season.zip(video.episode.or(video.number)))
        .or_else(|| episode_numbers(request))
}

fn viewing_state(progress: Option<&EpisodeProgress>) -> (i32, f32) {
    match progress {
        Some(p) if p.watched => (2, 1.0),
        Some(p) if p.position_secs.is_finite() && p.position_secs > 0.0 => {
            let fraction = if p.duration_secs.is_finite() && p.duration_secs > 0.0 {
                (p.position_secs / p.duration_secs).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            (1, fraction)
        }
        _ => (0, 0.0),
    }
}

// Keep delegate identities (and their keyboard focus) while transfer/progress
// data changes on the timer. Structural/filter/sort changes replace the model.
fn update_episode_model(
    model: &slint::ModelRc<DownloadEpisode>,
    rows: Vec<DownloadEpisode>,
) -> slint::ModelRc<DownloadEpisode> {
    if model.row_count() != rows.len()
        || rows
            .iter()
            .enumerate()
            .any(|(i, row)| model.row_data(i).is_none_or(|old| old.id != row.id))
    {
        return Rc::new(VecModel::from(rows)).into();
    }
    for (i, row) in rows.into_iter().enumerate() {
        let old = model.row_data(i).unwrap();
        if old.title != row.title
            || old.badge != row.badge
            || old.details != row.details
            || old.state != row.state
            || old.progress != row.progress
            || old.action_label != row.action_label
            || old.action_kind != row.action_kind
        {
            model.set_row_data(i, row);
        }
    }
    model.clone()
}

fn update_group_model(
    model: &slint::ModelRc<DownloadGroup>,
    mut rows: Vec<DownloadGroup>,
    retained: &HashSet<String>,
) -> slint::ModelRc<DownloadGroup> {
    if model.row_count() != rows.len()
        || rows
            .iter()
            .enumerate()
            .any(|(i, row)| model.row_data(i).is_none_or(|old| old.id != row.id))
    {
        // A completed download can add or reorder groups while their direct
        // completion pixels no longer reside in the LRU. Match by identity
        // across structural rebuilds too, otherwise the scheduler remembers
        // a successful delivery while the replacement model stays blank.
        let live: HashMap<_, _> = model
            .iter()
            .filter(|row| retained.contains(row.id.as_str()))
            .map(|row| (row.id.clone(), row))
            .collect();
        for row in &mut rows {
            if let Some(old) = live.get(&row.id)
                && old.artwork_identity == row.artwork_identity
                && row.artwork.size().width == 0
            {
                row.artwork = old.artwork.clone();
            }
        }
        return Rc::new(VecModel::from(rows)).into();
    }
    for (i, mut row) in rows.into_iter().enumerate() {
        let old = model.row_data(i).unwrap();
        let episodes = update_episode_model(&old.episodes, row.episodes.iter().collect());
        let structure_changed = old.episodes.row_count() != row.episodes.row_count()
            || old
                .episodes
                .iter()
                .zip(row.episodes.iter())
                .any(|(a, b)| a.id != b.id);
        row.episodes = episodes;
        // Preserve directly delivered pixels after decoded-cache eviction only
        // for the same URL/recipe and a card still inside its retention window.
        if retained.contains(row.id.as_str())
            && old.artwork_identity == row.artwork_identity
            && row.artwork.size().width == 0
        {
            row.artwork = old.artwork.clone();
        }
        let artwork_changed =
            old.artwork != row.artwork || old.artwork_identity != row.artwork_identity;
        if old.title != row.title
            || old.subtitle != row.subtitle
            || old.collapsed != row.collapsed
            || structure_changed
            || artwork_changed
        {
            model.set_row_data(i, row);
        }
    }
    model.clone()
}

impl Bridge {
    pub(super) fn downloads_page_query_changed(&self, query: &str) {
        self.shared.lock().unwrap().downloads_page_query = query.into();
        self.refresh_downloads_page();
    }
    pub(super) fn downloads_page_filter_changed(&self, filter: i32) {
        self.shared.lock().unwrap().downloads_page_filter = filter.clamp(0, 3);
        self.refresh_downloads_page();
    }
    pub(super) fn downloads_page_sort_changed(&self, recent: bool) {
        self.shared.lock().unwrap().downloads_page_sort_recent = recent;
        self.refresh_downloads_page();
    }
    pub(super) fn downloads_page_group_toggled(&self, id: &str) {
        let mut state = self.shared.lock().unwrap();
        if !state.downloads_page_collapsed.remove(id) {
            state.downloads_page_collapsed.insert(id.into());
        }
        drop(state);
        self.refresh_downloads_page();
    }
    pub(super) fn downloads_page_action(&self, id: &str, action: i32) {
        let owner = profiles::ProfileStore::capture();
        if self
            .downloads
            .job(id)
            .is_none_or(|job| !smart_offline::allows_download(&owner, &job))
            || matches!(action, 1 | 3) && !profile_policy::management_allowed(&owner)
        {
            return;
        }
        let Some(job) = self.downloads.job(id) else {
            return;
        };
        match action {
            // The coordinator checks completion under the same lock that
            // claims removal: a transfer finishing after a stale × click must
            // never turn cancellation into deletion of an offline file.
            1 | 3 => {
                if !self.downloads.remove_from_page(id, action == 3) {
                    return;
                }
                self.apply_stream_filter();
                self.downloads_list_to_ui();
            }
            2 if job.phase == DownloadPhase::Failed => self.reselect_failed_download(job.clone()),
            0 => match job.phase {
                DownloadPhase::Completed => self.play_download(id),
                DownloadPhase::Paused | DownloadPhase::Failed => self.downloads.resume(id),
                _ => self.downloads.pause(id),
            },
            _ => return,
        }
        self.refresh_downloads_page();
    }

    pub(super) fn refresh_downloads_page(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_show_downloads() {
            self.shared
                .lock()
                .unwrap()
                .downloads_page_refresh
                .invalidate();
            return;
        }
        let (query, filter, recent, collapsed, progress, entries) = {
            let context = profiles::capture();
            let mut state = self.shared.lock().unwrap();
            let revisions = RefreshRevisions {
                projection: super::progress_projection::revision(),
                downloads: self.downloads.revision(),
                images: image_settings_revision(),
                // Retaining the session Arc here would make its Shared state
                // own itself. Identity and activation epoch are sufficient.
                profile: (context.id, context.epoch),
                language: state.cache_settings.language,
                day: today_days(),
                artwork_allowed: app.get_page_artwork_allowed(),
                spoiler_protection: state.cache_settings.spoiler_protection,
            };
            if state
                .downloads_page_refresh
                .last
                .as_ref()
                .is_some_and(|last| {
                    last.revisions == revisions
                        && last.query == state.downloads_page_query
                        && last.filter == state.downloads_page_filter
                        && last.recent == state.downloads_page_sort_recent
                        && last.collapsed == state.downloads_page_collapsed
                })
            {
                drop(state);
                // Failed/cancelled image requests still need their normal
                // cooldown retry even when no display data has changed.
                self.schedule_page_artwork(super::page_artwork::Page::Downloads);
                return;
            }
            state.downloads_page_refresh.last = Some(RefreshSignature {
                revisions,
                query: state.downloads_page_query.clone(),
                filter: state.downloads_page_filter,
                recent: state.downloads_page_sort_recent,
                collapsed: state.downloads_page_collapsed.clone(),
            });
            #[cfg(test)]
            {
                state.downloads_page_refresh.projections += 1;
            }
            (
                state.downloads_page_query.clone(),
                state.downloads_page_filter,
                state.downloads_page_sort_recent,
                state.downloads_page_collapsed.clone(),
                state.progress.clone(),
                state.entries.clone(),
            )
        };
        let owner = profiles::ProfileStore::capture();
        let jobs = self
            .downloads
            .jobs()
            .into_iter()
            .filter(|job| smart_offline::allows_download(&owner, job))
            .collect::<Vec<_>>();
        let protected_jobs: HashSet<_> = {
            let state = self.shared.lock().unwrap();
            jobs.iter()
                .filter(|job| {
                    spoiler::hidden(&state, &job.media_type, &job.media_id, &job.request_id)
                })
                .map(|job| job.id.clone())
                .collect()
        };
        let mut metadata = HashMap::new();
        for job in &jobs {
            metadata
                .entry((job.media_type.clone(), job.media_id.clone()))
                .or_insert_with(|| {
                    read_episodes_cache_for(&job.media_type, &job.media_id).unwrap_or_default()
                });
        }
        let mut artwork_identities = Vec::new();
        let retained: HashSet<String> = self
            .shared
            .lock()
            .unwrap()
            .page_artwork
            .retained_keys(super::page_artwork::Page::Downloads);
        let mut counts = vec![0i32; 4];
        let mut bytes = 0u64;
        let mut series = HashSet::new();
        let mut groups: BTreeMap<String, Vec<DownloadJob>> = BTreeMap::new();
        let mut transfers = Vec::new();
        for job in jobs {
            let p = progress.get(&progress_map_key(&job.media_id, &job.request_id));
            let (view, _) = viewing_state(p);
            let videos = &metadata[&(job.media_type.clone(), job.media_id.clone())];
            let video = videos.iter().find(|video| video.id == job.request_id);
            let numbers = video
                .and_then(|v| v.season.zip(v.episode.or(v.number)))
                .or_else(|| episode_numbers(&job.request_id));
            let protected = protected_jobs.contains(&job.id);
            let title = if protected {
                spoiler::numbered_download_label(video, &job.request_id)
            } else {
                video
                    .map(Video::label)
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| {
                        if job.media_type == "series" {
                            job.file_name
                                .clone()
                                .unwrap_or_else(|| job.request_id.clone())
                        } else {
                            job.title.clone()
                        }
                    })
            };
            let parent_title = if protected && job.media_type.is_empty() {
                text::tr("Recovered files").to_string()
            } else {
                job.title.clone()
            };
            let searchable = if protected {
                format!("{parent_title} {title}")
            } else {
                format!("{} {} {} {}", job.title, title, job.request_id, job.display)
            };
            let matches = nova_ui::settings_search_matches(&query, &searchable);
            if job.phase != DownloadPhase::Completed {
                if matches {
                    transfers.push(DownloadEpisode {
                        id: job.id.clone().into(),
                        title: format!("{parent_title} · {title}").into(),
                        badge: "↓".into(),
                        details: if protected && job.phase == DownloadPhase::Failed {
                            text::tr("Download failed").into()
                        } else {
                            DownloadCoordinator::status_text(&job)
                        }
                        .into(),
                        state: 1,
                        progress: DownloadCoordinator::progress_fraction(&job),
                        action_label: DownloadCoordinator::action_label(&job).into(),
                        action_kind: DownloadCoordinator::action_kind(&job),
                    });
                }
                continue;
            }
            counts[0] += 1;
            counts[match view {
                0 => 1,
                1 => 2,
                _ => 3,
            }] += 1;
            bytes = bytes.saturating_add(job.total_bytes.unwrap_or(job.bytes_downloaded));
            if job.media_type == "series" {
                series.insert(job.media_id.clone());
            }
            if !matches
                || (filter != 0
                    && filter
                        != match view {
                            0 => 1,
                            1 => 2,
                            _ => 3,
                        })
            {
                continue;
            }
            let key = format!(
                "{}\u{1}{}\u{1}{}",
                job.media_type,
                job.media_id,
                if job.media_type == "series" {
                    numbers.map_or(String::new(), |(s, _)| s.to_string())
                } else {
                    String::new()
                }
            );
            groups.entry(key).or_default().push(job.clone());
            // Build the display projection after grouping, below. The viewing
            // state is always derived from persisted playback history.
        }
        let mut grouped: Vec<_> = groups.into_iter().collect();
        grouped.sort_by(|(_, a), (_, b)| {
            if recent {
                b.iter()
                    .map(|j| j.created_at)
                    .max()
                    .cmp(&a.iter().map(|j| j.created_at).max())
            } else {
                a[0].title
                    .to_lowercase()
                    .cmp(&b[0].title.to_lowercase())
                    .then_with(|| {
                        cached_episode_numbers(
                            &a[0].request_id,
                            &metadata[&(a[0].media_type.clone(), a[0].media_id.clone())],
                        )
                        .cmp(&cached_episode_numbers(
                            &b[0].request_id,
                            &metadata[&(b[0].media_type.clone(), b[0].media_id.clone())],
                        ))
                    })
            }
        });
        let rows: Vec<DownloadGroup> = grouped
            .into_iter()
            .map(|(id, mut jobs)| {
                jobs.sort_by(|a, b| {
                    cached_episode_numbers(
                        &a.request_id,
                        &metadata[&(a.media_type.clone(), a.media_id.clone())],
                    )
                    .cmp(&cached_episode_numbers(
                        &b.request_id,
                        &metadata[&(b.media_type.clone(), b.media_id.clone())],
                    ))
                    .then_with(|| a.request_id.cmp(&b.request_id))
                    .then_with(|| a.id.cmp(&b.id))
                });
                let first = &jobs[0];
                let season = cached_episode_numbers(
                    &first.request_id,
                    &metadata[&(first.media_type.clone(), first.media_id.clone())],
                )
                .map(|(s, _)| s);
                let size = jobs.iter().fold(0u64, |sum, job| {
                    sum.saturating_add(job.total_bytes.unwrap_or(job.bytes_downloaded))
                });
                let header = if first.media_type == "series" {
                    season.map_or_else(
                        || text::tr("Episodes").to_string(),
                        |s| format!("{} {s}", text::tr("Season")),
                    )
                } else if first.media_type.is_empty() {
                    text::tr("Recovered files").into()
                } else {
                    text::tr("Movie").into()
                };
                let subtitle = format!(
                    "{header} · {} · {}",
                    text::downloads_item_count(jobs.len(), first.media_type == "series"),
                    format_bytes(size)
                );
                let artwork_url = entries
                    .iter()
                    .find(|entry| entry.id == first.media_id && entry.type_ == first.media_type)
                    .map(|entry| {
                        if entry.background_url.is_empty() {
                            entry.poster_url.clone()
                        } else {
                            entry.background_url.clone()
                        }
                    })
                    .unwrap_or_default();
                let identity = super::page_artwork::Identity::new(
                    artwork_url,
                    ImageUse::Fixed(Some(DETAIL_BACKDROP_SIDE)),
                );
                let artwork = if retained.contains(&id) {
                    decoded_cache_get(&identity.cache_key)
                        .map(Image::from_rgba8)
                        .unwrap_or_default()
                } else {
                    Image::default()
                };
                let artwork_identity = identity.cache_key.clone().into();
                artwork_identities.push((id.clone(), identity));
                let title = if first.title.is_empty()
                    || protected_jobs.contains(&first.id) && first.media_type.is_empty()
                {
                    text::tr("Recovered files").into()
                } else {
                    first.title.clone()
                };
                let episodes = jobs
                    .iter()
                    .map(|job| {
                        let videos = &metadata[&(job.media_type.clone(), job.media_id.clone())];
                        let video = videos.iter().find(|v| v.id == job.request_id);
                        let numbers = video
                            .and_then(|v| v.season.zip(v.episode.or(v.number)))
                            .or_else(|| episode_numbers(&job.request_id));
                        let (view, fraction) = viewing_state(
                            progress.get(&progress_map_key(&job.media_id, &job.request_id)),
                        );
                        DownloadEpisode {
                            id: job.id.clone().into(),
                            title: if protected_jobs.contains(&job.id) {
                                spoiler::numbered_download_label(video, &job.request_id)
                            } else {
                                video
                                    .map(Video::label)
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| {
                                        job.file_name.clone().unwrap_or_else(|| job.title.clone())
                                    })
                            }
                            .into(),
                            badge: numbers
                                .map_or_else(
                                    || {
                                        if job.media_type == "movie" {
                                            text::tr("Movie").into()
                                        } else {
                                            "—".into()
                                        }
                                    },
                                    |(s, e)| format!("S{s} E{e}"),
                                )
                                .into(),
                            details: format_bytes(job.total_bytes.unwrap_or(job.bytes_downloaded))
                                .into(),
                            state: view,
                            progress: fraction,
                            action_label: text::tr(if view == 1 { "Resume" } else { "Play" })
                                .into(),
                            action_kind: 4,
                        }
                    })
                    .collect::<Vec<_>>();
                DownloadGroup {
                    id: id.clone().into(),
                    title: title.into(),
                    subtitle: subtitle.into(),
                    artwork,
                    artwork_identity,
                    collapsed: collapsed.contains(&id),
                    failed_count: episodes
                        .iter()
                        .filter(|episode| episode.action_kind == 3)
                        .count() as i32,
                    episodes: Rc::new(VecModel::from(episodes)).into(),
                }
            })
            .collect();
        self.shared.lock().unwrap().page_artwork.reconcile(
            super::page_artwork::Page::Downloads,
            format!("{query}:{filter}:{recent}"),
            artwork_identities,
            app.get_page_artwork_allowed(),
        );
        app.set_downloads_page_summary(
            text::downloads_summary(counts[0] as usize, series.len(), &format_bytes(bytes)).into(),
        );
        app.set_downloads_page_counts(Rc::new(VecModel::from(counts)).into());
        app.set_downloads_page_groups(update_group_model(
            &app.get_downloads_page_groups(),
            rows,
            &retained,
        ));
        app.set_downloads_page_transfers(update_episode_model(
            &app.get_downloads_page_transfers(),
            transfers,
        ));
        app.set_downloads_page_query(query.into());
        app.set_downloads_page_filter(filter);
        app.set_downloads_page_sort_recent(recent);
        self.schedule_page_artwork(super::page_artwork::Page::Downloads);
        if app.get_show_downloads() {
            app.recover_back_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoiler_download_models_queries_and_accessible_rows_follow_live_profile_policy() {
        const ROOT: &str = "NOVA_SPOILER_DOWNLOADS_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-spoiler-downloads-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::downloads_page::tests::spoiler_download_models_queries_and_accessible_rows_follow_live_profile_policy", "--nocapture"]).output().unwrap();
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
        storage::init_at(&root.join("store"));
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let mut jobs = ["cached", "show:1:2", "OpaqueSecret"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let artifact = root.join(format!("downloads/http/{index}.mkv"));
                fs::create_dir_all(artifact.parent().unwrap()).unwrap();
                fs::write(&artifact, b"video").unwrap();
                DownloadJob {
                    id: id.into(),
                    media_type: "series".into(),
                    media_id: "show".into(),
                    request_id: id.into(),
                    title: "Safe show".into(),
                    phase: DownloadPhase::Completed,
                    artifact_path: Some(artifact),
                    file_name: Some("FinaleSecret.mkv".into()),
                    display: "SourceSecret".into(),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        jobs.push(DownloadJob {
            id: "transfer".into(),
            media_type: "series".into(),
            media_id: "show".into(),
            request_id: "transfer-id".into(),
            title: "Safe show".into(),
            phase: DownloadPhase::Failed,
            file_name: Some("FinaleSecret.mkv".into()),
            display: "SourceSecret".into(),
            error: Some("ErrorSecret file unavailable".into()),
            ..Default::default()
        });
        storage::try_set_str(
            "downloads:v1",
            &crate::download::DownloadManifest::new(jobs)
                .to_json()
                .unwrap(),
        )
        .unwrap();
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        let app = AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(390, 1000));
        app.show().unwrap();
        let player = crate::player::Player::setup(&app);
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
        write_episodes_cache_for(
            "series",
            "show",
            &[Video {
                id: "cached".into(),
                name: "Betrayal title".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }],
        );
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .spoiler_protection = true;
        bridge.show_downloads_page();
        bridge.refresh_spoiler_presentation();
        let titles = || {
            app.get_downloads_page_groups()
                .iter()
                .flat_map(|group| {
                    group
                        .episodes
                        .iter()
                        .map(|row| row.title.to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let protected = titles();
        assert_eq!(protected.len(), 3);
        for label in ["S1 E1", "S1 E2", "Episode"] {
            assert!(
                protected.iter().any(|title| title == label),
                "missing {label}: {protected:?}"
            );
        }
        let transfers = app.get_downloads_page_transfers();
        assert_eq!(transfers.row_count(), 1);
        assert_eq!(
            transfers.row_data(0).unwrap().details.as_str(),
            "Download failed"
        );
        assert!(!transfers.row_data(0).unwrap().title.contains("Secret"));
        assert!(
            app.get_downloads_rows()
                .iter()
                .all(|row| !format!("{}{}", row.subtitle, row.details).contains("Secret"))
        );
        for query in [
            "Betrayal",
            "FinaleSecret",
            "SourceSecret",
            "OpaqueSecret",
            "ErrorSecret",
        ] {
            bridge.downloads_page_query_changed(query);
            assert_eq!(
                app.get_downloads_page_groups().row_count(),
                0,
                "hidden term matched {query}"
            );
            assert_eq!(
                app.get_downloads_page_transfers().row_count(),
                0,
                "hidden transfer term matched {query}"
            );
        }
        bridge.downloads_page_query_changed("S1 E2");
        assert_eq!(titles(), ["S1 E2"]);
        bridge.downloads_page_query_changed("");
        for _ in 0..5 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
        }
        assert!(
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(
                &app,
                "DownloadEpisodeRow"
            )
            .count()
                >= 4,
            "the protected completed episodes and transfer must be mounted for the accessibility regression"
        );
        let labels = i_slint_backend_testing::ElementQuery::from_root(&app)
            .match_predicate(|_| true)
            .find_all()
            .into_iter()
            .filter_map(|item| item.accessible_label().map(|label| label.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !labels.contains("Secret") && !labels.contains("Betrayal"),
            "protected metadata leaked through accessibility: {labels}"
        );
        Bridge::set_episode_watched_locked(
            &mut bridge.shared.lock().unwrap().progress,
            "show",
            "cached",
            true,
        );
        bridge.refresh_spoiler_presentation();
        assert!(titles().iter().any(|title| title == "Betrayal title"));
        bridge.downloads_page_query_changed("Betrayal");
        assert_eq!(titles(), ["Betrayal title"]);
        bridge.downloads_page_query_changed("");
        Bridge::set_episode_watched_locked(
            &mut bridge.shared.lock().unwrap().progress,
            "show",
            "cached",
            false,
        );
        bridge.refresh_spoiler_presentation();
        assert!(!titles().iter().any(|title| title.contains("Betrayal")));
        spoiler::reveal("series", "show", "cached");
        bridge.refresh_spoiler_presentation();
        assert!(titles().iter().any(|title| title == "Betrayal title"));
        assert!(
            !bridge.shared.lock().unwrap().progress[&progress_map_key("show", "cached")].watched
        );
        assert!(titles().iter().any(|title| title == "Episode"));
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .spoiler_protection = false;
        bridge.refresh_spoiler_presentation();
        assert!(
            titles().iter().any(|title| title == "FinaleSecret.mkv"),
            "disabled preference preserves legacy labels"
        );
    }

    #[test]
    fn idle_page_skips_projection_and_refreshes_after_data_or_navigation_changes() {
        // Slint and storage are process globals. Keep this fixture independent
        // of other tests' profiles, DB initialization and event loops.
        const TEST_DIR: &str = "NOVA_DOWNLOADS_IDLE_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-downloads-idle-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args([
                    "--exact",
                    "app::downloads_page::tests::idle_page_skips_projection_and_refreshes_after_data_or_navigation_changes",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            fs::remove_dir_all(root).unwrap();
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
        storage::init_at(&root.join("store"));
        let artifact = root.join("downloads/http/completed/episode.mkv");
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, b"offline episode").unwrap();
        let job = DownloadJob {
            id: "completed".into(),
            media_type: "series".into(),
            media_id: "show".into(),
            request_id: "opaque-episode".into(),
            title: "Saved show".into(),
            phase: DownloadPhase::Completed,
            artifact_path: Some(artifact),
            ..Default::default()
        };
        storage::try_set_str(
            "downloads:v1",
            &crate::download::DownloadManifest::new(vec![job])
                .to_json()
                .unwrap(),
        )
        .unwrap();
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        let app = AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(390, 844));
        app.show().unwrap();
        let player = crate::player::Player::setup(&app);
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
        let mut videos = vec![Video {
            id: "opaque-episode".into(),
            name: "Original episode".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        }];
        write_episodes_cache_for("series", "show", &videos);
        bridge.show_downloads_page();
        let projections = || {
            bridge
                .shared
                .lock()
                .unwrap()
                .downloads_page_refresh
                .projections
        };
        let episode = || {
            app.get_downloads_page_groups()
                .row_data(0)
                .unwrap()
                .episodes
                .row_data(0)
                .unwrap()
        };
        assert_eq!(episode().title, "Original episode");
        let initial = projections();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            bridge.refresh_downloads_page();
        }
        eprintln!("100 idle Downloads refreshes: {:?}", started.elapsed());
        assert_eq!(projections(), initial, "idle ticks must not rebuild data");

        // Metadata changes do not bump the device-local download revision.
        // Exercise the real cache writer and its shared invalidation hook.
        let download_revision = bridge.downloads.revision();
        videos[0].name = "Refreshed episode".into();
        write_episodes_cache_for("series", "show", &videos);
        bridge.refresh_downloads_page();
        assert_eq!(bridge.downloads.revision(), download_revision);
        assert_eq!(episode().title, "Refreshed episode");
        assert_eq!(projections(), initial + 1);

        let english_subtitle = app
            .get_downloads_page_groups()
            .row_data(0)
            .unwrap()
            .subtitle;
        let settings = {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.language = Language::Croatian;
            state.cache_settings.clone()
        };
        text::with_language(Language::Croatian, || {
            write_json("settings", &settings);
            bridge.refresh_downloads_page();
            let subtitle = app
                .get_downloads_page_groups()
                .row_data(0)
                .unwrap()
                .subtitle;
            assert_ne!(subtitle, english_subtitle);
            assert!(subtitle.starts_with(text::tr("Season")));
        });
        let settings = {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.language = Language::English;
            state.cache_settings.clone()
        };
        write_json("settings", &settings);
        bridge.refresh_downloads_page();
        assert_eq!(
            app.get_downloads_page_groups()
                .row_data(0)
                .unwrap()
                .subtitle,
            english_subtitle
        );

        // Remote application retains its ordinary write path and must also
        // invalidate the page when the download itself has not changed.
        {
            let _applying = sync::ApplyingGuard::new();
            let mut state = bridge.shared.lock().unwrap();
            state.progress.insert(
                progress_map_key("show", "opaque-episode"),
                EpisodeProgress {
                    series_id: "show".into(),
                    episode_id: "opaque-episode".into(),
                    watched: true,
                    ..Default::default()
                },
            );
            let progress = state.progress.clone();
            drop(state);
            write_progress_map(&progress);
        }
        bridge.refresh_downloads_page();
        assert_eq!(episode().state, 2);
        bridge.downloads_page_filter_changed(3);
        assert_eq!(app.get_downloads_page_groups().row_count(), 1);
        bridge.downloads_page_query_changed("no matching download");
        assert_eq!(app.get_downloads_page_groups().row_count(), 0);
        bridge.downloads_page_query_changed("");
        let group = app.get_downloads_page_groups().row_data(0).unwrap();
        bridge.downloads_page_group_toggled(&group.id);
        assert!(
            app.get_downloads_page_groups()
                .row_data(0)
                .unwrap()
                .collapsed
        );

        let before_reopen = projections();
        app.set_show_downloads(false);
        bridge.show_downloads_page();
        assert_eq!(projections(), before_reopen + 1);
        bridge.refresh_downloads_page();
        assert_eq!(projections(), before_reopen + 1);
        let before_context = projections();
        profiles::with_context(profiles::activation_context("another"), || {
            bridge.refresh_downloads_page();
            assert_eq!(projections(), before_context + 1);
            bridge.refresh_downloads_page();
            assert_eq!(projections(), before_context + 1);
        });
        bridge.refresh_downloads_page();
        assert_eq!(projections(), before_context + 2);

        bridge.downloads.remove("completed");
        bridge.refresh_downloads_page();
        assert_eq!(app.get_downloads_page_groups().row_count(), 0);
    }

    #[test]
    fn timer_updates_retain_episode_model_identity() {
        let first = DownloadEpisode {
            id: "stable".into(),
            progress: 0.2,
            ..Default::default()
        };
        let model: slint::ModelRc<DownloadEpisode> =
            Rc::new(VecModel::from(vec![first.clone()])).into();
        let updated = update_episode_model(
            &model,
            vec![DownloadEpisode {
                progress: 0.4,
                ..first
            }],
        );
        assert_eq!(model.row_data(0).unwrap().progress, 0.4);
        assert_eq!(updated.row_data(0).unwrap().progress, 0.4);
        let replacement = update_episode_model(
            &model,
            vec![DownloadEpisode {
                id: "replacement".into(),
                ..Default::default()
            }],
        );
        assert_eq!(model.row_data(0).unwrap().id.as_str(), "stable");
        assert_eq!(replacement.row_data(0).unwrap().id.as_str(), "replacement");
    }

    #[test]
    fn cached_replacement_artwork_replaces_nonempty_old_image_by_identity() {
        let old_image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(1, 1));
        let new_image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(2, 2));
        let first = DownloadGroup {
            id: "stable-group".into(),
            artwork: old_image,
            artwork_identity: "old-url-and-recipe".into(),
            ..Default::default()
        };
        let model: slint::ModelRc<DownloadGroup> =
            Rc::new(VecModel::from(vec![first.clone()])).into();
        let retained = HashSet::from(["stable-group".to_owned()]);
        update_group_model(
            &model,
            vec![DownloadGroup {
                artwork: new_image,
                artwork_identity: "new-url-and-recipe".into(),
                ..first.clone()
            }],
            &retained,
        );
        assert_eq!(model.row_data(0).unwrap().artwork.size().width, 2);
        assert_eq!(
            model.row_data(0).unwrap().artwork_identity,
            "new-url-and-recipe"
        );
        // Directly delivered pixels survive decoded cache eviction for a retained
        // card, then are released when it leaves the retention window.
        let current = model.row_data(0).unwrap();
        update_group_model(
            &model,
            vec![DownloadGroup {
                artwork: Image::default(),
                ..current.clone()
            }],
            &retained,
        );
        assert_eq!(model.row_data(0).unwrap().artwork.size().width, 2);
        update_group_model(
            &model,
            vec![DownloadGroup {
                artwork: Image::default(),
                ..current
            }],
            &HashSet::new(),
        );
        assert_eq!(model.row_data(0).unwrap().artwork.size().width, 0);
    }

    #[test]
    fn group_reordering_keeps_visible_direct_artwork_after_cache_eviction() {
        let first = DownloadGroup {
            id: "first".into(),
            artwork_identity: "first-recipe".into(),
            artwork: Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(7, 3)),
            ..Default::default()
        };
        let model: slint::ModelRc<DownloadGroup> =
            Rc::new(VecModel::from(vec![first.clone()])).into();
        let updated = update_group_model(
            &model,
            vec![
                DownloadGroup {
                    id: "new".into(),
                    ..Default::default()
                },
                DownloadGroup {
                    artwork: Image::default(),
                    ..first.clone()
                },
            ],
            &HashSet::from(["first".to_owned()]),
        );
        assert_eq!(updated.row_data(1).unwrap().artwork.size().width, 7);
        let mut rows: Vec<_> = updated.iter().collect();
        rows.swap(0, 1);
        rows[0].artwork = Image::default();
        rows[0].artwork_identity = "replacement-recipe".into();
        let replaced = update_group_model(&updated, rows, &HashSet::from(["first".to_owned()]));
        assert_eq!(replaced.row_data(0).unwrap().artwork.size().width, 0);
    }

    #[test]
    fn request_tail_keeps_native_episode_numbers() {
        assert_eq!(episode_numbers("provider:show:12:108"), Some((12, 108)));
        assert_eq!(episode_numbers("tt123"), None);
        assert_eq!(episode_numbers("native:unknown"), None);
    }
    #[test]
    fn opaque_ids_sort_by_cached_episode_numbers() {
        let video = |id: &str, episode| Video {
            id: id.into(),
            season: Some(2),
            episode: Some(episode),
            ..Default::default()
        };
        let videos = vec![
            video("anime-z", 1),
            video("anime-a", 12),
            video("anime-m", 3),
        ];
        let mut ids = vec!["anime-a", "anime-m", "anime-z"];
        ids.sort_by_key(|id| cached_episode_numbers(id, &videos));
        assert_eq!(ids, ["anime-z", "anime-m", "anime-a"]);
        assert_eq!(
            cached_episode_numbers("fallback:4:8", &videos),
            Some((4, 8))
        );
    }

    #[test]
    fn viewing_filters_distinguish_resume_from_watched() {
        assert_eq!(viewing_state(None), (0, 0.0));
        let mut p = EpisodeProgress {
            position_secs: 45.0,
            duration_secs: 100.0,
            ..Default::default()
        };
        assert_eq!(viewing_state(Some(&p)), (1, 0.45));
        p.watched = true;
        assert_eq!(viewing_state(Some(&p)), (2, 1.0));
        p.watched = false;
        p.duration_secs = 0.0;
        assert_eq!(viewing_state(Some(&p)), (1, 0.0));
        p.position_secs = f64::NAN;
        assert_eq!(viewing_state(Some(&p)), (0, 0.0));
    }
}

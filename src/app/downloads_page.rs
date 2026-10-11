//! Device-local downloaded library projection. All mutations stay in the
//! download coordinator, which owns artifact lifetime and shared torrents.
use super::*;
use crate::download::{DownloadPhase, format_bytes};
use crate::{DownloadEpisode, DownloadGroup};
use std::collections::BTreeMap;

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
        let Some(job) = self.downloads.job(id) else {
            return;
        };
        match action {
            1 => self.remove_download(id),
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
            return;
        }
        let jobs = self.downloads.jobs();
        let (query, filter, recent, collapsed, progress, entries) = {
            let state = self.shared.lock().unwrap();
            (
                state.downloads_page_query.clone(),
                state.downloads_page_filter,
                state.downloads_page_sort_recent,
                state.downloads_page_collapsed.clone(),
                state.progress.clone(),
                state.entries.clone(),
            )
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
            let title = video
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
                });
            let matches = nova_ui::settings_search_matches(
                &query,
                &format!("{} {} {} {}", job.title, title, job.request_id, job.display),
            );
            if job.phase != DownloadPhase::Completed {
                if matches {
                    transfers.push(DownloadEpisode {
                        id: job.id.clone().into(),
                        title: format!("{} · {title}", job.title).into(),
                        badge: "↓".into(),
                        details: DownloadCoordinator::status_text(&job).into(),
                        state: 1,
                        progress: job
                            .total_bytes
                            .filter(|total| *total > 0)
                            .map_or(0.0, |total| {
                                (job.bytes_downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
                            }),
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
                let title = if first.title.is_empty() {
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
                            title: video
                                .map(Video::label)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| {
                                    job.file_name.clone().unwrap_or_else(|| job.title.clone())
                                })
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

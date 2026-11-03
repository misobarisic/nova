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
    data: Option<(u64, Arc<Vec<DownloadJob>>)>,
    progress_revision: u64,
    latest_transfers: HashMap<String, TransferUpdate>,
    transfer_rows: HashMap<String, (usize, DownloadPhase)>,
    metadata_stamp: Option<library::DisplayProofStamp>,
    metadata: HashMap<(String, String), Arc<EpisodeIndex>>,
    policy: Option<(
        PolicyReceipt,
        Arc<Result<profile_policy::TitlePolicySnapshot, String>>,
    )>,
    streams: Option<StreamSignature>,
    #[cfg(test)]
    projections: usize,
    #[cfg(test)]
    metadata_reads: usize,
    #[cfg(test)]
    completed_clones: usize,
    #[cfg(test)]
    transfer_notifications: usize,
    #[cfg(test)]
    stream_rebuilds: usize,
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
    metadata: library::DisplayProofStamp,
    reveals: u64,
}

struct RefreshSignature {
    revisions: RefreshRevisions,
    query: String,
    filter: i32,
    recent: bool,
    collapsed: HashSet<String>,
}

#[derive(PartialEq)]
struct PolicyReceipt {
    projection: u64,
    profile: (String, u64),
    scoped: bool,
}
#[derive(PartialEq)]
struct StreamSignature {
    revisions: RefreshRevisions,
    progress: u64,
}
struct EpisodeFacts {
    numbers: Option<(u32, u32)>,
    label: String,
    numbered_label: String,
}
#[derive(Default)]
struct EpisodeIndex {
    by_id: HashMap<String, EpisodeFacts>,
}
impl EpisodeIndex {
    fn new(videos: Vec<Video>) -> Self {
        let mut by_id = HashMap::new();
        for video in videos {
            // Keep the baseline's first matching native ID, including a first
            // match that has missing numbers or an empty label.
            by_id
                .entry(video.id.clone())
                .or_insert_with(|| EpisodeFacts {
                    numbers: video.season.zip(video.episode.or(video.number)),
                    label: video.label(),
                    numbered_label: episode_se_label(&video),
                });
        }
        Self { by_id }
    }
    fn numbers(&self, request: &str) -> Option<(u32, u32)> {
        self.by_id
            .get(request)
            .and_then(|facts| facts.numbers)
            .or_else(|| episode_numbers(request))
    }
}

struct PreparedEpisode<'a> {
    metadata: Option<&'a EpisodeFacts>,
    numbers: Option<(u32, u32)>,
}
struct GroupDraft<'a> {
    id: String,
    jobs: Vec<&'a DownloadJob>,
    folded_title: String,
    numbers: Option<(u32, u32)>,
    newest: u64,
}
fn protected_episode_label(metadata: Option<&EpisodeFacts>, request: &str) -> String {
    metadata
        .map(|facts| facts.numbered_label.clone())
        .unwrap_or_else(|| spoiler::numbered_download_label(None, request))
}
fn episode_key(job: &DownloadJob) -> (&str, &str, &str) {
    (&job.media_type, &job.media_id, &job.request_id)
}

fn episodic_media_type(media_type: &str) -> bool {
    matches!(media_type, "series" | "anime")
}

fn episode_numbers(request: &str) -> Option<(u32, u32)> {
    let mut parts = request.rsplit(':');
    let episode = parts.next()?.parse().ok()?;
    let season = parts.next()?.parse().ok()?;
    Some((season, episode))
}

#[cfg(test)]
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

    pub(super) fn refresh_stream_transfer_progress(
        &self,
        snapshot: &DownloadProjectionSnapshot,
        progress_changed: bool,
    ) {
        let Some(app) = self.app() else {
            return;
        };
        let context = profiles::capture();
        let revisions = {
            let state = self.shared.lock().unwrap();
            RefreshRevisions {
                projection: progress_projection::revision(),
                downloads: snapshot.revisions.structure,
                images: image_settings_revision(),
                profile: (context.id.clone(), context.epoch),
                language: state.cache_settings.language,
                day: today_days(),
                artwork_allowed: false,
                spoiler_protection: state.cache_settings.spoiler_protection,
                metadata: library::display_proof_stamp(),
                reveals: spoiler::reveal_revision(),
            }
        };
        let rebuild = {
            let state = self.shared.lock().unwrap();
            match &state.downloads_page_refresh.streams {
                Some(previous) => {
                    previous.revisions != revisions
                        || (!progress_changed && previous.progress != snapshot.revisions.progress)
                }
                None => progress_changed || !state.streams.is_empty(),
            }
        };
        let coherent = self.downloads.semantic_revision() == snapshot.revisions.structure
            && profiles::is_current(&context);
        if !coherent {
            self.shared.lock().unwrap().downloads_page_refresh.streams = None;
            return;
        }
        let mut rebuild = rebuild;
        if !rebuild && progress_changed {
            let mut state = self.shared.lock().unwrap();
            rebuild = state
                .streams
                .iter()
                .filter_map(|row| row.download.as_ref())
                .any(|job| {
                    snapshot
                        .transfers
                        .iter()
                        .any(|update| update.id == job.id && update.phase != job.phase)
                });
            if !rebuild {
                // The coordinator snapshot is owned plain data; no coordinator
                // borrow survives while cached display rows are patched.
                for row in &mut state.streams {
                    if let Some(job) = &mut row.download
                        && let Some(update) =
                            snapshot.transfers.iter().find(|update| update.id == job.id)
                    {
                        update.apply(job);
                    }
                }
            }
        }
        if rebuild {
            self.apply_stream_filter();
            #[cfg(test)]
            {
                self.shared
                    .lock()
                    .unwrap()
                    .downloads_page_refresh
                    .stream_rebuilds += 1;
            }
        } else if progress_changed {
            let visible: HashMap<_, _> = {
                let state = self.shared.lock().unwrap();
                state
                    .streams
                    .iter()
                    .filter_map(|row| {
                        row.download.as_ref().and_then(|job| {
                            snapshot
                                .transfers
                                .iter()
                                .find(|update| update.id == job.id)
                                .map(|update| (row.id.clone(), update))
                        })
                    })
                    .collect()
            };
            let model = app.get_streams();
            for index in 0..model.row_count() {
                let Some(mut row) = model.row_data(index) else {
                    continue;
                };
                if row.external_link {
                    continue;
                }
                if let Some(update) = visible.get(row.id.as_str()) {
                    let details =
                        (update.phase == DownloadPhase::Downloading).then(|| update.status_text());
                    let fraction = update.progress_fraction();
                    if details
                        .as_ref()
                        .is_some_and(|details| row.details != details.as_str())
                        || row.download_progress != fraction
                    {
                        if let Some(details) = details {
                            row.details = details.into();
                        }
                        row.download_progress = fraction;
                        row.lines = Self::estimate_stream_lines(&row.text)
                            + i32::from(!row.details.is_empty());
                        model.set_row_data(index, row);
                    }
                }
            }
        }
        if profiles::is_current(&context)
            && self.downloads.semantic_revision() == snapshot.revisions.structure
        {
            self.shared.lock().unwrap().downloads_page_refresh.streams = Some(StreamSignature {
                revisions,
                progress: snapshot.revisions.progress,
            });
        } else {
            self.shared.lock().unwrap().downloads_page_refresh.streams = None;
        }
    }

    fn update_visible_transfer_progress(
        &self,
        app: &AppWindow,
        updates: &[TransferUpdate],
        structure: u64,
    ) -> bool {
        if self.downloads.semantic_revision() != structure {
            return false;
        }
        let targets = {
            let state = self.shared.lock().unwrap();
            updates
                .iter()
                .filter_map(|update| {
                    state
                        .downloads_page_refresh
                        .transfer_rows
                        .get(&update.id)
                        .map(|(row, phase)| (*row, *phase, update))
                })
                .collect::<Vec<_>>()
        };
        let model = app.get_downloads_page_transfers();
        if targets.iter().any(|(row, phase, update)| {
            *phase != update.phase || model.row_data(*row).is_none_or(|row| row.id != update.id)
        }) {
            return false;
        }
        for (index, _, update) in targets {
            let mut row = model.row_data(index).unwrap();
            let details =
                (update.phase == DownloadPhase::Downloading).then(|| update.status_text());
            let fraction = update.progress_fraction();
            if details
                .as_ref()
                .is_some_and(|details| row.details != details.as_str())
                || row.progress != fraction
            {
                if let Some(details) = details {
                    row.details = details.into();
                }
                row.progress = fraction;
                model.set_row_data(index, row);
                #[cfg(test)]
                {
                    self.shared
                        .lock()
                        .unwrap()
                        .downloads_page_refresh
                        .transfer_notifications += 1;
                }
            }
        }
        true
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
        let context = profiles::capture();
        let version = progress_projection::revision();
        let owner = profiles::ProfileStore::capture();
        let (structure, seen_progress) = {
            let state = self.shared.lock().unwrap();
            let refresh = &state.downloads_page_refresh;
            (
                refresh.data.as_ref().map(|(revision, _)| *revision),
                refresh.progress_revision,
            )
        };
        let snapshot = self.downloads.projection_snapshot(structure, seen_progress);
        let (query, filter, recent, collapsed, progress, entries, jobs, latest, policy) = {
            let mut state = self.shared.lock().unwrap();
            let revisions = RefreshRevisions {
                projection: version,
                downloads: snapshot.revisions.structure,
                images: image_settings_revision(),
                profile: (context.id.clone(), context.epoch),
                language: state.cache_settings.language,
                day: today_days(),
                artwork_allowed: app.get_page_artwork_allowed(),
                spoiler_protection: state.cache_settings.spoiler_protection,
                metadata: library::display_proof_stamp(),
                reveals: spoiler::reveal_revision(),
            };
            if let Some(jobs) = snapshot.jobs {
                #[cfg(test)]
                {
                    state.downloads_page_refresh.completed_clones += jobs
                        .iter()
                        .filter(|job| job.phase == DownloadPhase::Completed)
                        .count();
                }
                state.downloads_page_refresh.data =
                    Some((snapshot.revisions.structure, Arc::new(jobs)));
                state.downloads_page_refresh.latest_transfers.clear();
            }
            state.downloads_page_refresh.progress_revision = snapshot.revisions.progress;
            for update in &snapshot.transfers {
                state
                    .downloads_page_refresh
                    .latest_transfers
                    .insert(update.id.clone(), update.clone());
            }
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
                if !self.update_visible_transfer_progress(
                    &app,
                    &snapshot.transfers,
                    snapshot.revisions.structure,
                ) {
                    let mut state = self.shared.lock().unwrap();
                    state.downloads_page_refresh.invalidate();
                    state.downloads_page_refresh.data = None;
                }
                self.schedule_page_artwork(super::page_artwork::Page::Downloads);
                return;
            }
            let policy_receipt = PolicyReceipt {
                projection: version,
                profile: (context.id.clone(), context.epoch),
                scoped: owner.is_scoped(),
            };
            let policy = state
                .downloads_page_refresh
                .policy
                .as_ref()
                .filter(|(receipt, policy)| *receipt == policy_receipt && policy.is_ok())
                .map(|(_, policy)| policy.clone());
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
                state
                    .downloads_page_refresh
                    .data
                    .as_ref()
                    .unwrap()
                    .1
                    .clone(),
                state.downloads_page_refresh.latest_transfers.clone(),
                policy,
            )
        };
        let policy = policy.unwrap_or_else(|| {
            let policy = Arc::new(profile_policy::TitlePolicySnapshot::capture(&owner));
            self.shared.lock().unwrap().downloads_page_refresh.policy = Some((
                PolicyReceipt {
                    projection: version,
                    profile: (context.id.clone(), context.epoch),
                    scoped: owner.is_scoped(),
                },
                policy.clone(),
            ));
            policy
        });
        let jobs = jobs
            .iter()
            .filter(|job| {
                policy.as_ref().as_ref().is_ok_and(|policy| {
                    policy.allows(&job.media_type, &job.media_id)
                        && policy.allows_addon(if job.offline.source_owner.is_empty() {
                            "local"
                        } else {
                            &job.offline.source_owner
                        })
                })
            })
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
        let metadata_stamp = library::display_proof_stamp();
        let needed: HashSet<_> = jobs
            .iter()
            .map(|job| (job.media_type.clone(), job.media_id.clone()))
            .collect();
        let mut metadata = {
            let mut state = self.shared.lock().unwrap();
            let refresh = &mut state.downloads_page_refresh;
            if refresh.metadata_stamp != Some(metadata_stamp) {
                refresh.metadata.clear();
                refresh.metadata_stamp = Some(metadata_stamp);
            }
            refresh.metadata.retain(|key, _| needed.contains(key));
            refresh.metadata.clone()
        };
        for job in &jobs {
            metadata
                .entry((job.media_type.clone(), job.media_id.clone()))
                .or_insert_with(|| {
                    #[cfg(test)]
                    {
                        self.shared
                            .lock()
                            .unwrap()
                            .downloads_page_refresh
                            .metadata_reads += 1;
                    }
                    Arc::new(EpisodeIndex::new(
                        read_episodes_cache_for(&job.media_type, &job.media_id).unwrap_or_default(),
                    ))
                });
        }
        self.shared.lock().unwrap().downloads_page_refresh.metadata = metadata.clone();
        let facts: HashMap<_, _> = jobs
            .iter()
            .map(|job| {
                let index = &metadata[&(job.media_type.clone(), job.media_id.clone())];
                (
                    episode_key(job),
                    PreparedEpisode {
                        metadata: index.by_id.get(&job.request_id),
                        numbers: index.numbers(&job.request_id),
                    },
                )
            })
            .collect();
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
        let mut groups: BTreeMap<String, Vec<&DownloadJob>> = BTreeMap::new();
        let mut transfers = Vec::new();
        let mut transfer_rows = HashMap::new();
        for job in jobs {
            let p = progress.get(&progress_map_key(&job.media_id, &job.request_id));
            let (view, _) = viewing_state(p);
            let fact = &facts[&episode_key(job)];
            let video = fact.metadata;
            let numbers = fact.numbers;
            let protected = protected_jobs.contains(&job.id);
            let title = if protected {
                protected_episode_label(video, &job.request_id)
            } else {
                video
                    .map(|facts| facts.label.clone())
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| {
                        if episodic_media_type(&job.media_type) {
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
                    transfer_rows.insert(job.id.clone(), (transfers.len(), job.phase));
                    transfers.push(DownloadEpisode {
                        id: job.id.clone().into(),
                        title: format!("{parent_title} · {title}").into(),
                        badge: "↓".into(),
                        details: if protected && job.phase == DownloadPhase::Failed {
                            text::tr("Download failed").into()
                        } else {
                            latest
                                .get(&job.id)
                                .filter(|update| update.phase == DownloadPhase::Downloading)
                                .map(TransferUpdate::status_text)
                                .unwrap_or_else(|| DownloadCoordinator::status_text(job))
                        }
                        .into(),
                        state: 1,
                        progress: latest
                            .get(&job.id)
                            .map(TransferUpdate::progress_fraction)
                            .unwrap_or_else(|| DownloadCoordinator::progress_fraction(job)),
                        action_label: DownloadCoordinator::action_label(job).into(),
                        action_kind: DownloadCoordinator::action_kind(job),
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
            if episodic_media_type(&job.media_type) {
                series.insert((job.media_type.clone(), job.media_id.clone()));
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
                if episodic_media_type(&job.media_type) {
                    numbers.map_or(String::new(), |(s, _)| s.to_string())
                } else {
                    String::new()
                }
            );
            groups.entry(key).or_default().push(job);
            // Build the display projection after grouping, below. The viewing
            // state is always derived from persisted playback history.
        }
        let mut grouped: Vec<_> = groups
            .into_iter()
            .map(|(id, jobs)| {
                let first = jobs[0];
                let folded_title = first.title.to_lowercase();
                let numbers = facts[&episode_key(first)].numbers;
                let newest = jobs.iter().map(|job| job.created_at).max().unwrap_or(0);
                GroupDraft {
                    id,
                    jobs,
                    folded_title,
                    numbers,
                    newest,
                }
            })
            .collect();
        grouped.sort_by(|a, b| {
            if recent {
                b.newest.cmp(&a.newest)
            } else {
                a.folded_title
                    .cmp(&b.folded_title)
                    .then_with(|| a.numbers.cmp(&b.numbers))
            }
        });
        let rows: Vec<DownloadGroup> = grouped
            .into_iter()
            .map(|group| {
                let id = group.id;
                let mut jobs = group.jobs;
                jobs.sort_by(|a, b| {
                    facts[&episode_key(a)]
                        .numbers
                        .cmp(&facts[&episode_key(b)].numbers)
                        .then_with(|| a.request_id.cmp(&b.request_id))
                        .then_with(|| a.id.cmp(&b.id))
                });
                let first = jobs[0];
                let season = facts[&episode_key(first)].numbers.map(|(season, _)| season);
                let size = jobs.iter().fold(0u64, |sum, job| {
                    sum.saturating_add(job.total_bytes.unwrap_or(job.bytes_downloaded))
                });
                let header = if episodic_media_type(&first.media_type) {
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
                    text::downloads_item_count(jobs.len(), episodic_media_type(&first.media_type)),
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
                        let fact = &facts[&episode_key(job)];
                        let video = fact.metadata;
                        let numbers = fact.numbers;
                        let (view, fraction) = viewing_state(
                            progress.get(&progress_map_key(&job.media_id, &job.request_id)),
                        );
                        DownloadEpisode {
                            id: job.id.clone().into(),
                            title: if protected_jobs.contains(&job.id) {
                                protected_episode_label(video, &job.request_id)
                            } else {
                                video
                                    .map(|facts| facts.label.clone())
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
        if !profiles::is_current(&context)
            || progress_projection::revision() != version
            || self.downloads.semantic_revision() != snapshot.revisions.structure
            || library::display_proof_stamp() != metadata_stamp
        {
            self.shared
                .lock()
                .unwrap()
                .downloads_page_refresh
                .invalidate();
            return;
        }
        self.shared.lock().unwrap().page_artwork.reconcile(
            super::page_artwork::Page::Downloads,
            format!("{query}:{filter}:{recent}"),
            artwork_identities,
            app.get_page_artwork_allowed(),
        );
        self.shared
            .lock()
            .unwrap()
            .downloads_page_refresh
            .transfer_rows = transfer_rows;
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

    #[derive(Debug, PartialEq)]
    struct EpisodeOracle {
        id: String,
        title: String,
        badge: String,
        details: String,
        state: i32,
        progress: f32,
        action: String,
        kind: i32,
    }
    #[derive(Debug, PartialEq)]
    struct GroupOracle {
        id: String,
        title: String,
        subtitle: String,
        collapsed: bool,
        episodes: Vec<EpisodeOracle>,
    }
    fn actual_groups(model: slint::ModelRc<DownloadGroup>) -> Vec<GroupOracle> {
        model
            .iter()
            .map(|group| GroupOracle {
                id: group.id.to_string(),
                title: group.title.to_string(),
                subtitle: group.subtitle.to_string(),
                collapsed: group.collapsed,
                episodes: group
                    .episodes
                    .iter()
                    .map(|row| EpisodeOracle {
                        id: row.id.to_string(),
                        title: row.title.to_string(),
                        badge: row.badge.to_string(),
                        details: row.details.to_string(),
                        state: row.state,
                        progress: row.progress,
                        action: row.action_label.to_string(),
                        kind: row.action_kind,
                    })
                    .collect(),
            })
            .collect()
    }
    // Independent pre-UI09 linear inventory joins/comparator behavior. This
    // oracle intentionally does not use EpisodeIndex or prepared numeric keys.
    fn legacy_groups(
        jobs: &[DownloadJob],
        inventories: &HashMap<(String, String), Vec<Video>>,
        query: &str,
        recent: bool,
        collapsed: &HashSet<String>,
    ) -> Vec<GroupOracle> {
        let mut groups: BTreeMap<String, Vec<&DownloadJob>> = BTreeMap::new();
        for job in jobs
            .iter()
            .filter(|job| job.phase == DownloadPhase::Completed)
        {
            let videos = inventories
                .get(&(job.media_type.clone(), job.media_id.clone()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let video = videos.iter().find(|video| video.id == job.request_id);
            let title = video
                .map(Video::label)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| {
                    if episodic_media_type(&job.media_type) {
                        job.file_name
                            .clone()
                            .unwrap_or_else(|| job.request_id.clone())
                    } else {
                        job.title.clone()
                    }
                });
            if !nova_ui::settings_search_matches(
                query,
                &format!("{} {} {} {}", job.title, title, job.request_id, job.display),
            ) {
                continue;
            }
            let numbers = cached_episode_numbers(&job.request_id, videos);
            let key = format!(
                "{}\u{1}{}\u{1}{}",
                job.media_type,
                job.media_id,
                if episodic_media_type(&job.media_type) {
                    numbers.map_or(String::new(), |(season, _)| season.to_string())
                } else {
                    String::new()
                }
            );
            groups.entry(key).or_default().push(job);
        }
        let numbers = |job: &DownloadJob| {
            cached_episode_numbers(
                &job.request_id,
                inventories
                    .get(&(job.media_type.clone(), job.media_id.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        };
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|(_, a), (_, b)| {
            if recent {
                b.iter()
                    .map(|job| job.created_at)
                    .max()
                    .cmp(&a.iter().map(|job| job.created_at).max())
            } else {
                a[0].title
                    .to_lowercase()
                    .cmp(&b[0].title.to_lowercase())
                    .then_with(|| numbers(a[0]).cmp(&numbers(b[0])))
            }
        });
        groups
            .into_iter()
            .map(|(id, mut jobs)| {
                jobs.sort_by(|a, b| {
                    numbers(a)
                        .cmp(&numbers(b))
                        .then_with(|| a.request_id.cmp(&b.request_id))
                        .then_with(|| a.id.cmp(&b.id))
                });
                let first = jobs[0];
                let size = jobs.iter().fold(0u64, |sum, job| {
                    sum.saturating_add(job.total_bytes.unwrap_or(job.bytes_downloaded))
                });
                let header = if episodic_media_type(&first.media_type) {
                    numbers(first).map(|(season, _)| season).map_or_else(
                        || text::tr("Episodes").into(),
                        |season| format!("{} {season}", text::tr("Season")),
                    )
                } else if first.media_type.is_empty() {
                    text::tr("Recovered files").into()
                } else {
                    text::tr("Movie").into()
                };
                let subtitle = format!(
                    "{header} · {} · {}",
                    text::downloads_item_count(jobs.len(), episodic_media_type(&first.media_type)),
                    format_bytes(size)
                );
                GroupOracle {
                    id: id.clone(),
                    title: if first.title.is_empty() {
                        text::tr("Recovered files").into()
                    } else {
                        first.title.clone()
                    },
                    subtitle,
                    collapsed: collapsed.contains(&id),
                    episodes: jobs
                        .into_iter()
                        .map(|job| {
                            let video = inventories
                                .get(&(job.media_type.clone(), job.media_id.clone()))
                                .and_then(|videos| {
                                    videos.iter().find(|video| video.id == job.request_id)
                                });
                            EpisodeOracle {
                                id: job.id.clone(),
                                title: video
                                    .map(Video::label)
                                    .filter(|label| !label.is_empty())
                                    .unwrap_or_else(|| {
                                        job.file_name.clone().unwrap_or_else(|| job.title.clone())
                                    }),
                                badge: numbers(job).map_or_else(
                                    || {
                                        if job.media_type == "movie" {
                                            text::tr("Movie").into()
                                        } else {
                                            "—".into()
                                        }
                                    },
                                    |(season, episode)| format!("S{season} E{episode}"),
                                ),
                                details: format_bytes(
                                    job.total_bytes.unwrap_or(job.bytes_downloaded),
                                ),
                                state: 0,
                                progress: 0.0,
                                action: text::tr("Play").into(),
                                kind: 4,
                            }
                        })
                        .collect(),
                }
            })
            .collect()
    }

    #[test]
    fn ui09_scale_byte_ticks_preserve_rows_models_and_skip_completed_work() {
        const ROOT: &str = "NOVA_UI09_SCALE_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-ui09-scale-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root)
                .args(["--exact","app::downloads_page::tests::ui09_scale_byte_ticks_preserve_rows_models_and_skip_completed_work","--nocapture"]).output().unwrap();
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
        for titles in [10, 100, 1000] {
            for per_title in [1, 10, 100] {
                let case = root.join(format!("{titles}-{per_title}"));
                let artifact = case.join("downloads/http/artifact.bin");
                fs::create_dir_all(artifact.parent().unwrap()).unwrap();
                fs::write(&artifact, vec![b'x'; 1024]).unwrap();
                let mut jobs = vec![DownloadJob {
                    id: "active".into(),
                    media_type: "series".into(),
                    media_id: "active-title".into(),
                    request_id: "active-native".into(),
                    title: "Active title".into(),
                    file_name: Some("active.mkv".into()),
                    phase: DownloadPhase::Paused,
                    ..Default::default()
                }];
                let mut inventories = HashMap::new();
                for title in 0..titles {
                    let kind = if title % 2 == 0 { "anime" } else { "series" };
                    let parent = format!("parent-{title:04}");
                    let mut videos = Vec::new();
                    for episode in 1..=per_title {
                        let native = format!("opaque-{episode:04}");
                        jobs.push(DownloadJob {
                            id: format!("job-{title:04}-{episode:04}"),
                            media_type: kind.into(),
                            media_id: parent.clone(),
                            request_id: native.clone(),
                            title: format!("Saved {title:04}"),
                            display: "source".into(),
                            file_name: Some(format!("file-{episode}.mkv")),
                            phase: DownloadPhase::Completed,
                            artifact_path: Some(artifact.clone()),
                            bytes_downloaded: 1024,
                            total_bytes: Some(1024),
                            created_at: (title % 3) as u64,
                            ..Default::default()
                        });
                        if episode % 5 != 0 {
                            videos.push(Video {
                                id: native,
                                name: format!("Episode {episode:04}"),
                                season: Some(1),
                                episode: Some(episode as u32),
                                ..Default::default()
                            });
                        }
                    }
                    videos.reverse();
                    if let Some(first) = videos.first() {
                        videos.push(Video {
                            id: first.id.clone(),
                            name: "duplicate must lose".into(),
                            season: Some(99),
                            episode: Some(99),
                            ..Default::default()
                        });
                    }
                    write_episodes_cache_for(kind, &parent, &videos);
                    inventories.insert((kind.into(), parent), videos);
                }
                storage::try_set_str(
                    "downloads:v1",
                    &crate::download::DownloadManifest::new(jobs)
                        .to_json()
                        .unwrap(),
                )
                .unwrap();
                let downloads = DownloadCoordinator::new(case.join("downloads"));
                downloads.ui_progress_for_test(
                    "active",
                    crate::download::DownloadProgress::new(0, None, 4096).with_resources(0, 600),
                );
                let app = AppWindow::new().unwrap();
                let player = crate::player::Player::setup(&app);
                #[cfg(feature = "desktop")]
                let bridge = Bridge::new(
                    app.as_weak(),
                    PosterTx::closed(),
                    Arc::new(AtomicU64::new(0)),
                    player,
                    downloads,
                );
                #[cfg(not(feature = "desktop"))]
                let bridge = Bridge::new(
                    app.as_weak(),
                    Arc::new(AtomicU64::new(0)),
                    player,
                    downloads,
                );
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .cache_settings
                    .spoiler_protection = false;
                // Suspend artwork through its public overlay input; the
                // derived artwork policy is intentionally read-only.
                app.set_modal_visible(true);
                assert!(!app.get_page_artwork_allowed());
                bridge.show_downloads_page();
                let oracle_jobs = bridge.downloads.jobs();
                assert_eq!(
                    actual_groups(app.get_downloads_page_groups()),
                    legacy_groups(&oracle_jobs, &inventories, "", false, &HashSet::new())
                );
                let initial = {
                    let state = bridge.shared.lock().unwrap();
                    let refresh = &state.downloads_page_refresh;
                    (
                        refresh.projections,
                        refresh.metadata_reads,
                        refresh.completed_clones,
                        refresh.transfer_notifications,
                    )
                };
                assert_eq!(initial.1, titles + 1);
                assert_eq!(initial.2, titles * per_title);
                let groups = app.get_downloads_page_groups();
                let transfers = app.get_downloads_page_transfers();
                let episodes = groups.row_data(0).unwrap().episodes;
                for _ in 0..1200 {
                    bridge.refresh_downloads_page();
                }
                let revision = bridge.downloads.revision();
                bridge.refresh_stream_transfer_progress(
                    &bridge.downloads.transfer_snapshot(revision),
                    false,
                );
                let active = bridge.downloads.job("active").unwrap();
                bridge.shared.lock().unwrap().streams = vec![StreamUi {
                    id: "active-source".into(),
                    choice_id: None,
                    owner_url: String::new(),
                    display: "Release".into(),
                    card: StreamCardText::default(),
                    source: StreamSource::Url("http://127.0.0.1/unused".into()),
                    addon: String::new(),
                    download: Some(active),
                }];
                app.set_streams(
                    Rc::new(VecModel::from(vec![crate::StreamRow {
                        id: "active-source".into(),
                        text: "Release".into(),
                        is_download: true,
                        ..Default::default()
                    }]))
                    .into(),
                );
                let stream_model = app.get_streams();
                bridge.downloads_seen.store(revision, Ordering::Release);
                let mut event = 0;
                let mut maximum = Duration::ZERO;
                let reads = detail::trace_episode_reads(|| {
                    for tick in 1..=240 {
                        let next = tick * 600 / 240;
                        while event < next {
                            event += 1;
                            bridge.downloads.ui_progress_for_test(
                                "active",
                                crate::download::DownloadProgress::new(
                                    event * 1024,
                                    None,
                                    4096 + event,
                                )
                                .with_resources(event, 600),
                            );
                        }
                        let started = std::time::Instant::now();
                        bridge.refresh_download_rows();
                        bridge.refresh_downloads_page();
                        maximum = maximum.max(started.elapsed());
                        let expected = bridge.downloads.job("active").unwrap();
                        let row = app.get_downloads_page_transfers().row_data(0).unwrap();
                        assert_eq!(
                            row.progress,
                            DownloadCoordinator::progress_fraction(&expected)
                        );
                        assert_eq!(row.details, DownloadCoordinator::status_text(&expected));
                        let stream = app.get_streams().row_data(0).unwrap();
                        assert_eq!(stream.download_progress, row.progress);
                        assert_eq!(stream.details, row.details);
                    }
                });
                assert!(reads.is_empty());
                {
                    let state = bridge.shared.lock().unwrap();
                    let refresh = &state.downloads_page_refresh;
                    assert_eq!(
                        (
                            refresh.projections,
                            refresh.metadata_reads,
                            refresh.completed_clones
                        ),
                        (initial.0, initial.1, initial.2)
                    );
                    assert_eq!(refresh.transfer_notifications - initial.3, 240);
                    assert_eq!(refresh.stream_rebuilds, 0);
                }
                assert!(std::ptr::eq(
                    groups.as_any(),
                    app.get_downloads_page_groups().as_any()
                ));
                assert!(std::ptr::eq(
                    episodes.as_any(),
                    app.get_downloads_page_groups()
                        .row_data(0)
                        .unwrap()
                        .episodes
                        .as_any()
                ));
                assert!(std::ptr::eq(
                    transfers.as_any(),
                    app.get_downloads_page_transfers().as_any()
                ));
                assert!(std::ptr::eq(
                    stream_model.as_any(),
                    app.get_streams().as_any()
                ));
                for (query, recent) in [
                    ("Episode 0001", true),
                    ("Saved", false),
                    ("", true),
                    ("", false),
                ] {
                    bridge.downloads_page_query_changed(query);
                    bridge.downloads_page_sort_changed(recent);
                    assert_eq!(
                        actual_groups(app.get_downloads_page_groups()),
                        legacy_groups(&oracle_jobs, &inventories, query, recent, &HashSet::new())
                    );
                }
                let id = app
                    .get_downloads_page_groups()
                    .row_data(0)
                    .unwrap()
                    .id
                    .to_string();
                bridge.downloads_page_group_toggled(&id);
                assert_eq!(
                    actual_groups(app.get_downloads_page_groups()),
                    legacy_groups(
                        &oracle_jobs,
                        &inventories,
                        "",
                        false,
                        &HashSet::from([id.clone()])
                    )
                );
                bridge.downloads_page_group_toggled(&id);
                assert_eq!(
                    app.get_downloads_page_transfers()
                        .row_data(0)
                        .unwrap()
                        .progress,
                    1.0,
                    "view changes retain the latest resource snapshot"
                );
                let stale = bridge.downloads.transfer_snapshot(0);
                bridge.downloads.pause("active");
                assert!(!bridge.update_visible_transfer_progress(
                    &app,
                    &stale.transfers,
                    stale.revisions.structure
                ));
                bridge.refresh_downloads_page();
                assert_eq!(
                    app.get_downloads_page_transfers()
                        .row_data(0)
                        .unwrap()
                        .action_kind,
                    2
                );
                bridge.downloads.ui_progress_for_test(
                    "active",
                    crate::download::DownloadProgress::new(600 * 1024, None, 4696)
                        .with_resources(600, 600),
                );
                bridge.refresh_downloads_page();
                let before = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .downloads_page_refresh
                    .projections;
                bridge
                    .downloads
                    .ui_finish_for_test("active", artifact.clone(), 1024);
                bridge.refresh_download_rows();
                bridge.refresh_downloads_page();
                assert_eq!(app.get_downloads_page_transfers().row_count(), 0);
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .downloads_page_refresh
                        .projections,
                    before + 1
                );
                for _ in 0..10 {
                    bridge.refresh_downloads_page();
                }
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .downloads_page_refresh
                        .projections,
                    before + 1
                );
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .downloads_page_refresh
                        .metadata_reads,
                    initial.1,
                    "view/byte/phase changes do not reread unchanged metadata"
                );
                eprintln!(
                    "UI09 titles={titles} jobs/title={per_title} completed={} logical_idle_ticks=1200 events=600 active_ticks=240 warm_completed_clones=0 warm_metadata_reads=0 warm_group_rebuilds=0 transfer_notifications=240 max_callback_pair={maximum:?}",
                    titles * per_title
                );
            }
        }
    }

    #[test]
    fn ui09_episode_index_matches_first_native_inventory_fact_and_number_fallback() {
        for seed in 0..32 {
            let mut videos = (0..40)
                .map(|episode| Video {
                    id: format!("opaque-{episode}"),
                    name: format!("Label {seed}-{episode}"),
                    season: (episode % 5 != 0).then_some(episode % 3),
                    episode: (episode % 7 != 0).then_some(episode),
                    number: Some(episode + 1),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            videos.push(Video {
                id: "opaque-1".into(),
                name: "duplicate must lose".into(),
                season: Some(99),
                episode: Some(99),
                ..Default::default()
            });
            let index = EpisodeIndex::new(videos.clone());
            for id in videos
                .iter()
                .map(|video| video.id.as_str())
                .chain(["missing", "provider:3:9"])
            {
                assert_eq!(index.numbers(id), cached_episode_numbers(id, &videos));
                let old = videos.iter().find(|video| video.id == id);
                assert_eq!(
                    protected_episode_label(index.by_id.get(id), id),
                    spoiler::numbered_download_label(old, id)
                );
                assert_eq!(
                    index.by_id.get(id).map(|facts| facts.label.as_str()),
                    old.map(|video| if video.name.is_empty() {
                        video.title.as_str()
                    } else {
                        video.name.as_str()
                    })
                );
            }
        }
    }

    #[test]
    fn native_download_types_are_episodic_without_normalizing_identifiers() {
        for kind in ["series", "anime"] {
            assert!(episodic_media_type(kind));
        }
        for kind in ["movie", "", "show", "ANIME", "anime-movie"] {
            assert!(!episodic_media_type(kind));
        }
    }

    #[test]
    fn episodic_download_nouns_follow_english_and_croatian() {
        text::with_language(Language::English, || {
            assert_eq!(
                text::downloads_item_count(1, episodic_media_type("anime")),
                "1 episode"
            );
            assert_eq!(
                text::downloads_item_count(2, episodic_media_type("anime")),
                "2 episodes"
            );
            assert_eq!(
                text::downloads_item_count(2, episodic_media_type("movie")),
                "2 files"
            );
        });
        text::with_language(Language::Croatian, || {
            assert_eq!(
                text::downloads_item_count(1, episodic_media_type("anime")),
                "1 epizoda"
            );
            assert_eq!(
                text::downloads_item_count(2, episodic_media_type("anime")),
                "2 epizode"
            );
            assert_eq!(
                text::downloads_item_count(2, episodic_media_type("")),
                "2 datoteke"
            );
        });
    }

    #[test]
    fn native_anime_downloads_keep_seasons_typed_counts_nouns_and_job_ids() {
        const ROOT: &str = "NOVA_ANIME_DOWNLOADS_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-anime-downloads-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::downloads_page::tests::native_anime_downloads_keep_seasons_typed_counts_nouns_and_job_ids", "--nocapture"])
                .output().unwrap();
            let completed = root.join("completed").exists();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success() && completed,
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root.join("store"));
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let mut jobs = Vec::new();
        for (id, kind, parent, native) in [
            ("anime-s1", "anime", "same", "shared-native"),
            ("anime-s1-b", "anime", "same", "second-native"),
            ("anime-s2", "anime", "same", "third-native"),
            ("anime-unknown", "anime", "same", "opaque-unmapped"),
            ("series-s1", "series", "same", "shared-native"),
            ("movie", "movie", "same", "same"),
            ("recovered", "", "recovered", "opaque-file"),
        ] {
            let artifact = root.join(format!("downloads/http/{id}.mkv"));
            fs::create_dir_all(artifact.parent().unwrap()).unwrap();
            fs::write(&artifact, b"offline fixture").unwrap();
            jobs.push(DownloadJob {
                id: id.into(),
                media_type: kind.into(),
                media_id: parent.into(),
                request_id: native.into(),
                title: format!("Saved {kind}"),
                file_name: Some(format!("{id}-fallback.mkv")),
                phase: DownloadPhase::Completed,
                artifact_path: Some(artifact),
                ..Default::default()
            });
        }
        jobs.push(DownloadJob {
            id: "anime-transfer".into(),
            media_type: "anime".into(),
            media_id: "same".into(),
            request_id: "opaque-transfer".into(),
            title: "Anime transfer".into(),
            file_name: Some("native-anime-filename.mkv".into()),
            phase: DownloadPhase::Failed,
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
        let player = crate::player::Player::setup(&app);
        #[cfg(feature = "desktop")]
        let bridge = {
            Bridge::new(
                app.as_weak(),
                PosterTx::closed(),
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
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .spoiler_protection = false;
        write_episodes_cache_for(
            "anime",
            "same",
            &[
                Video {
                    id: "shared-native".into(),
                    name: "Anime cached episode".into(),
                    season: Some(1),
                    episode: Some(1),
                    ..Default::default()
                },
                Video {
                    id: "second-native".into(),
                    name: "Anime second episode".into(),
                    season: Some(1),
                    episode: Some(2),
                    ..Default::default()
                },
                Video {
                    id: "third-native".into(),
                    name: "Anime season two".into(),
                    season: Some(2),
                    episode: Some(1),
                    ..Default::default()
                },
            ],
        );
        write_episodes_cache_for(
            "series",
            "same",
            &[Video {
                id: "shared-native".into(),
                name: "Series cached episode".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }],
        );
        let group_key =
            |kind: &str, parent: &str, season: &str| format!("{kind}\u{1}{parent}\u{1}{season}");
        for (language, season, episodes, files, summary) in [
            (
                Language::English,
                "Season",
                "2 episodes",
                "1 file",
                "7 downloads · 2 series ·",
            ),
            (
                Language::Croatian,
                "Sezona",
                "2 epizode",
                "1 datoteka",
                "7 preuzimanja · 2 serije ·",
            ),
        ] {
            bridge.shared.lock().unwrap().cache_settings.language = language;
            text::with_language(language, || {
                bridge.show_downloads_page();
                let groups: HashMap<_, _> = app
                    .get_downloads_page_groups()
                    .iter()
                    .map(|g| (g.id.to_string(), g))
                    .collect();
                assert_eq!(groups.len(), 6);
                let anime = &groups[&group_key("anime", "same", "1")];
                assert!(
                    anime
                        .subtitle
                        .starts_with(&format!("{season} 1 · {episodes}"))
                );
                assert!(
                    groups[&group_key("anime", "same", "2")]
                        .subtitle
                        .starts_with(&format!("{season} 2 ·"))
                );
                assert_eq!(anime.episodes.row_count(), 2);
                let episode = anime.episodes.iter().find(|e| e.id == "anime-s1").unwrap();
                assert_eq!(episode.title, "Anime cached episode");
                assert_eq!(episode.badge, "S1 E1");
                let series = &groups[&group_key("series", "same", "1")];
                assert_eq!(
                    series.episodes.row_data(0).unwrap().title,
                    "Series cached episode"
                );
                let movie = &groups[&group_key("movie", "same", "")];
                assert!(movie.subtitle.contains(files));
                assert!(movie.subtitle.starts_with(text::tr("Movie")));
                let recovered = &groups[&group_key("", "recovered", "")];
                assert!(recovered.subtitle.starts_with(text::tr("Recovered files")));
                assert!(recovered.subtitle.contains(files));
                let unknown = &groups[&group_key("anime", "same", "")];
                assert!(unknown.subtitle.starts_with(text::tr("Episodes")));
                assert_eq!(
                    unknown.episodes.row_data(0).unwrap().title,
                    "anime-unknown-fallback.mkv"
                );
                assert!(app.get_downloads_page_summary().starts_with(summary));
                let expected: HashSet<_> = [
                    "anime-s1",
                    "anime-s1-b",
                    "anime-s2",
                    "anime-unknown",
                    "series-s1",
                    "movie",
                    "recovered",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                let actual: HashSet<_> = groups
                    .values()
                    .flat_map(|g| g.episodes.iter())
                    .map(|e| {
                        assert_eq!(e.action_kind, 4);
                        e.id.to_string()
                    })
                    .collect();
                assert_eq!(actual, expected);
                let transfer = app.get_downloads_page_transfers().row_data(0).unwrap();
                assert_eq!(transfer.id, "anime-transfer");
                assert_eq!(transfer.title, "Anime transfer · native-anime-filename.mkv");
            });
        }
        // Deletion consumes the stable job ID, even when native title/episode
        // IDs collide with another media type.
        bridge.downloads_page_action("anime-s1", 3);
        assert!(bridge.downloads.job("anime-s1").is_none());
        for retained in ["anime-s1-b", "anime-s2", "series-s1", "movie", "recovered"] {
            assert!(bridge.downloads.job(retained).is_some());
        }
        fs::write(root.join("completed"), "ok").unwrap();
    }

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
            Bridge::new(
                app.as_weak(),
                PosterTx::closed(),
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
            Bridge::new(
                app.as_weak(),
                PosterTx::closed(),
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

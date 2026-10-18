use super::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{
    CancellationToken, DownloadIntroTimings, DownloadJob, DownloadManifest, DownloadOutcome,
    DownloadPhase, DownloadProgress, DownloadSource as JobSource, artifact_filename, format_bytes,
    format_percent, part_path, sanitize_filename,
};
use crate::torrent::TorrentStats;
use nova_config::DownloadSettings;

const RESOLVER_DOWNLOAD_PREFIX: &str = "nova-resolver:";
const RESOLVER_RESELECT: &str =
    "Reload streams and select Download again to refresh this download.";

#[derive(Clone)]
struct ResolverDownload {
    provider_url: String,
    token: String,
    /// Only provider configuration changes invalidate an already queued
    /// download. Navigation may continue while the captured episode transfers.
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
}

fn is_resolver_download(job: &DownloadJob) -> bool {
    job.source
        .url()
        .is_some_and(|url| url.starts_with(RESOLVER_DOWNLOAD_PREFIX))
}

/// UI identities carry a request generation to reject stale clicks. Download
/// identities must survive a reload; the provider's hash already incorporates
/// its owner, request and result. Accept only that internal identity format.
fn resolver_download_source(stream_id: &str) -> Option<JobSource> {
    let (generation, identity) = stream_id.split_once(':')?;
    generation.parse::<u64>().ok()?;
    let hash = identity.strip_prefix("stream-")?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(JobSource::http(format!(
        "{RESOLVER_DOWNLOAD_PREFIX}{identity}"
    )))
}

const DOWNLOADS_KEY: &str = "downloads:v1";
const DOWNLOAD_SETTINGS_KEY: &str = "download_settings";
const DOWNLOAD_CORRUPT_PREFIX: &str = "downloads:v1.corrupt.";
/// A torrent that reports neither progress nor a non-zero rate for this long
/// is treated as stalled and failed, so it cannot pin the single download
/// slot (and every queued job behind it) forever.
const TORRENT_STALL_SECS: u64 = 10 * 60;

#[derive(Clone)]
pub(crate) struct DownloadCoordinator {
    inner: Arc<DownloadInner>,
}

struct NewDownload {
    media_type: String,
    media_id: String,
    request_id: String,
    title: String,
    year: String,
    display: String,
    addon: String,
    source: JobSource,
}

struct DownloadInner {
    root: PathBuf,
    state: Mutex<DownloadState>,
    next_id: AtomicU64,
    revision: AtomicU64,
}

impl DownloadInner {
    /// Lock the state, ignoring poisoning. A panic in one worker must not
    /// wedge every later download operation behind a poisoned mutex.
    fn state(&self) -> std::sync::MutexGuard<'_, DownloadState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct DownloadState {
    manifest: DownloadManifest,
    active: Option<ActiveDownload>,
    last_persist: Instant,
    resolvers: HashMap<String, ResolverDownload>,
}

#[derive(Clone)]
struct ActiveDownload {
    id: String,
    torrent_engine: Option<Arc<crate::torrent::TorrentEngine>>,
    cancel: CancellationToken,
    torrent_cancel: Arc<AtomicBool>,
    remove: Arc<AtomicBool>,
    /// Set by the worker's drop guard when its thread ends (normal return or
    /// panic unwind). If it is set while `state.active` still names the same
    /// job, the worker died without finalizing: the tick reaps it so the
    /// queue cannot stay stuck behind a dead transfer.
    finished: Arc<AtomicBool>,
}

/// Drop guard marking a worker as ended, including on panic unwind.
struct WorkerDone(Arc<AtomicBool>);

impl Drop for WorkerDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn same_download_identity(job: &DownloadJob, snapshot: &DownloadJob) -> bool {
    job.id == snapshot.id
        && job.created_at == snapshot.created_at
        && job.source == snapshot.source
        && job.intro_request == snapshot.intro_request
        && job.artifact_path == snapshot.artifact_path
        && job.media_type == snapshot.media_type
        && job.media_id == snapshot.media_id
        && job.request_id == snapshot.request_id
}

fn completed_snapshot_matches(job: &DownloadJob, snapshot: &DownloadJob) -> bool {
    job.phase == DownloadPhase::Completed
        && snapshot.phase == DownloadPhase::Completed
        && same_download_identity(job, snapshot)
}

impl DownloadCoordinator {
    pub(crate) fn new(root: PathBuf) -> Self {
        if let Err(error) = std::fs::create_dir_all(&root) {
            eprintln!("nova downloads: cannot create {}: {error}", root.display());
        }
        let manifest = load_manifest(&root);
        log_corrupt_manifest_backups();
        let coordinator = Self {
            inner: Arc::new(DownloadInner {
                root,
                state: Mutex::new(DownloadState {
                    manifest,
                    active: None,
                    last_persist: Instant::now(),
                    resolvers: HashMap::new(),
                }),
                next_id: AtomicU64::new(1),
                revision: AtomicU64::new(0),
            }),
        };
        coordinator.persist();
        coordinator
    }

    pub(crate) fn jobs(&self) -> Vec<DownloadJob> {
        self.inner.state().manifest.jobs.clone()
    }

    /// A playback worker can reuse only the completed artifact it inspected.
    pub(crate) fn cached_intro(&self, snapshot: &DownloadJob) -> Option<DownloadIntroTimings> {
        let timings = {
            let state = self.inner.state();
            let job = state
                .manifest
                .jobs
                .iter()
                .find(|job| completed_snapshot_matches(job, snapshot))?;
            job.intro_timings.clone()?
        };
        timings
            .matches_path(snapshot.artifact_path.as_deref()?)
            .then_some(timings)
    }

    /// Online playback may upgrade an older download, but neither removal nor
    /// ID reuse may be undone by the response arriving afterward.
    pub(crate) fn retain_intro(
        &self,
        snapshot: &DownloadJob,
        timings: DownloadIntroTimings,
    ) -> bool {
        let Some(path) = snapshot.artifact_path.as_deref() else {
            return false;
        };
        if !timings.matches_path(path) {
            return false;
        }
        let mut state = self.inner.state();
        let Some(job) = state
            .manifest
            .jobs
            .iter_mut()
            .find(|job| completed_snapshot_matches(job, snapshot))
        else {
            return false;
        };
        if !timings.matches_path(path) {
            return false;
        }
        if job.intro_timings.as_ref() == Some(&timings) {
            return true;
        }
        job.intro_timings = Some(timings);
        job.updated_at = crate::download::unix_timestamp();
        persist_manifest(&state.manifest);
        self.bump_revision();
        true
    }

    pub(crate) fn torrent_owners(&self) -> Vec<crate::torrent::RetainedDownload> {
        torrent_owners(&self.inner.state().manifest, &self.inner.root)
    }

    pub(crate) fn start(&self) {
        self.schedule_next();
    }

    fn cancel_stale_resolvers(&self) {
        let pending: Vec<_> = {
            let state = self.inner.state();
            state
                .manifest
                .jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.phase,
                        DownloadPhase::Queued
                            | DownloadPhase::Resolving
                            | DownloadPhase::Downloading
                    )
                })
                .filter_map(|job| {
                    state
                        .resolvers
                        .get(&job.id)
                        .map(|resolver| (job.id.clone(), resolver.clone()))
                })
                .collect()
        };
        // Owner guards read the app state; never hold the coordinator lock
        // while checking them, since UI projections read these locks too.
        for (id, resolver) in pending {
            if (resolver.is_current)() {
                continue;
            }
            let mut state = self.inner.state();
            if let Some(active) = state.active.as_ref().filter(|active| active.id == id) {
                active.cancel.cancel();
            }
            if let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) {
                job.phase = DownloadPhase::Failed;
                job.error = Some(RESOLVER_RESELECT.into());
                job.bytes_per_second = 0;
                persist_manifest(&state.manifest);
                self.bump_revision();
            }
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    pub(crate) fn jobs_for_request(&self, request_id: &str) -> Vec<DownloadJob> {
        let mut jobs: Vec<_> = self
            .inner
            .state()
            .manifest
            .jobs
            .iter()
            .filter(|job| job.request_id == request_id)
            .cloned()
            .collect();
        jobs.sort_by(|a, b| {
            let rank = |job: &DownloadJob| match job.phase {
                DownloadPhase::Resolving | DownloadPhase::Downloading => 0,
                DownloadPhase::Queued => 1,
                DownloadPhase::Paused => 2,
                DownloadPhase::Failed => 3,
                DownloadPhase::Completed => 4,
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        jobs
    }

    pub(crate) fn job(&self, id: &str) -> Option<DownloadJob> {
        self.inner
            .state()
            .manifest
            .jobs
            .iter()
            .find(|job| job.id == id)
            .cloned()
    }

    /// Every completed episode download, sorted by series title then
    /// season/episode, for Settings → Downloads → Downloaded episodes.
    /// Re-adopted artifacts (empty `media_type`, recovered from disk when the
    /// manifest was lost) are included so they stay reachable.
    pub(crate) fn completed_episode_jobs(&self) -> Vec<DownloadJob> {
        let mut jobs: Vec<DownloadJob> = self
            .inner
            .state()
            .manifest
            .jobs
            .iter()
            .filter(|job| {
                job.phase == DownloadPhase::Completed
                    && (job.media_type == "series" || job.media_type.is_empty())
            })
            .cloned()
            .collect();
        jobs.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| episode_sort_key(&a.request_id).cmp(&episode_sort_key(&b.request_id)))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        jobs
    }

    fn enqueue(&self, request: NewDownload) -> String {
        self.enqueue_with_resolver(request, None)
    }

    fn enqueue_with_resolver(
        &self,
        request: NewDownload,
        resolver: Option<ResolverDownload>,
    ) -> String {
        let NewDownload {
            media_type,
            media_id,
            request_id,
            title,
            year,
            display,
            addon,
            source,
        } = request;
        let intro_request = intro_skip::download_request(&media_type, &media_id, &request_id);
        let id = {
            let mut state = self.inner.state();
            if let Some(index) = state
                .manifest
                .jobs
                .iter()
                .position(|job| job.request_id == request_id && job.source == source)
            {
                if let Some(resolver) = resolver {
                    let id = state.manifest.jobs[index].id.clone();
                    state.resolvers.insert(id, resolver);
                }
                // Already have a job for this exact stream. Re-tapping
                // "Download" on a failed job re-arms the same row (no
                // duplicate pinned entry, and an HTTP `.part` resumes) so a
                // stream that timed out can be retried instead of silently
                // returning a stuck id. Live/paused jobs keep their own
                // pinned-row actions.
                if state.manifest.jobs[index].phase == DownloadPhase::Failed {
                    let job = &mut state.manifest.jobs[index];
                    job.phase = DownloadPhase::Queued;
                    job.error = None;
                    job.updated_at = crate::download::unix_timestamp();
                    if matches!(job.source, JobSource::Torrent { .. }) {
                        job.bytes_downloaded = 0;
                        job.total_bytes = None;
                        job.bytes_per_second = 0;
                    }
                    let id = job.id.clone();
                    persist_manifest(&state.manifest);
                    self.bump_revision();
                    drop(state);
                    self.schedule_next();
                    return id;
                }
                // Even an existing job may have been left `Queued` with no
                // active worker (e.g. a dead worker was reaped). Kick the
                // queue so re-tapping Download always means "start it".
                let id = state.manifest.jobs[index].id.clone();
                drop(state);
                self.schedule_next();
                return id;
            }
            let id = loop {
                let candidate = format!(
                    "download-{:016x}",
                    self.inner.next_id.fetch_add(1, Ordering::Relaxed)
                );
                if !state.manifest.jobs.iter().any(|job| job.id == candidate) {
                    break candidate;
                }
            };
            let mut job = DownloadJob::new(id.clone(), source);
            job.intro_request = intro_request;
            job.media_type = media_type;
            job.media_id = media_id;
            job.request_id = request_id;
            job.title = title;
            job.year = (!year.is_empty()).then_some(year);
            job.display = display;
            job.addon = addon;
            job.file_name = Some(job_file_name(&job));
            if job.source.is_http() {
                // Preserve the readable label separately from the disk path.
                // Persist the chosen path before the first transfer so restart
                // never re-derives it from a subsequently refreshed UI title.
                job.artifact_path = Some(self.inner.root.join("http").join(&job.id).join(
                    artifact_filename(&job.id, job.file_name.as_deref().unwrap_or_default()),
                ));
            }
            if let Some(resolver) = resolver {
                state.resolvers.insert(id.clone(), resolver);
            }
            state.manifest.jobs.push(job);
            persist_manifest(&state.manifest);
            self.bump_revision();
            id
        };
        self.schedule_next();
        id
    }

    pub(crate) fn pause(&self, id: &str) {
        let mut state = self.inner.state();
        let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        if matches!(job.phase, DownloadPhase::Completed | DownloadPhase::Failed) {
            return;
        }
        job.phase = DownloadPhase::Paused;
        job.error = None;
        job.updated_at = crate::download::unix_timestamp();
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            let active = state.active.as_ref().unwrap();
            active.cancel.cancel();
            active.torrent_cancel.store(true, Ordering::Release);
        }
        persist_manifest(&state.manifest);
        self.bump_revision();
    }

    pub(crate) fn resume(&self, id: &str) {
        {
            let mut state = self.inner.state();
            let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) else {
                return;
            };
            if job.phase != DownloadPhase::Paused && job.phase != DownloadPhase::Failed {
                return;
            }
            job.phase = DownloadPhase::Queued;
            job.error = None;
            job.updated_at = crate::download::unix_timestamp();
            if matches!(job.source, JobSource::Torrent { .. }) {
                job.bytes_downloaded = 0;
                job.total_bytes = None;
                job.bytes_per_second = 0;
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
        }
        self.schedule_next();
    }

    pub(crate) fn remove(&self, id: &str) {
        let (removed, transfer_engine) = {
            let mut state = self.inner.state();
            let Some(index) = state.manifest.jobs.iter().position(|job| job.id == id) else {
                return;
            };
            let mut job = state.manifest.jobs.remove(index);
            // Removal also drops metadata if filesystem cleanup must restore
            // the row with an error. A late lookup cannot reattach it.
            job.intro_request = None;
            job.intro_timings = None;
            state.resolvers.remove(id);
            if let Some(active) = state.active.as_ref().filter(|active| active.id == id) {
                active.remove.store(true, Ordering::Release);
                active.cancel.cancel();
                active.torrent_cancel.store(true, Ordering::Release);
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
            let engine = state
                .active
                .as_ref()
                .filter(|active| active.id == job.id)
                .and_then(|active| active.torrent_engine.clone());
            (job, engine)
        };
        if matches!(removed.source, JobSource::Torrent { .. }) {
            let remaining = self.torrent_owners();
            let result = if let Some(engine) = transfer_engine.or_else(crate::torrent::engine) {
                for owner in &remaining {
                    engine.retain_download(owner.clone());
                }
                engine.remove_retained(&removed.id).map(|_| ())
            } else if let Some(owner) = torrent_owner(&removed, &self.inner.root) {
                crate::torrent::remove_download_files(&owner, &remaining)
            } else {
                Ok(())
            };
            if let Err(error) = result {
                let mut state = self.inner.state();
                let mut restored = removed;
                restored.error = Some(error);
                if !matches!(restored.phase, DownloadPhase::Completed) {
                    restored.phase = DownloadPhase::Failed;
                }
                state.manifest.jobs.push(restored);
                persist_manifest(&state.manifest);
                self.bump_revision();
                return;
            }
            self.schedule_next();
            return;
        }
        if !self
            .inner
            .state()
            .active
            .as_ref()
            .is_some_and(|active| active.id == removed.id)
        {
            self.remove_artifacts(&removed);
        }
        self.schedule_next();
    }

    pub(crate) fn remove_for_watched_episode(
        &self,
        series_id: &str,
        episode_id: &str,
        playing_episode: bool,
    ) -> bool {
        let ids = {
            self.inner
                .state()
                .manifest
                .jobs
                .iter()
                .filter(|job| job.media_id == series_id && job.request_id == episode_id)
                // Unlike a single open MP4, the HLS demuxer opens more files
                // as playback advances. Keep its bundle until player-close
                // finalization calls automatic cleanup again.
                .filter(|job| {
                    !playing_episode
                        || !job.artifact_path.as_ref().is_some_and(|path| {
                            path.extension()
                                .is_some_and(|extension| extension == "m3u8")
                        })
                })
                .map(|job| job.id.clone())
                .collect::<Vec<_>>()
        };
        let removed = !ids.is_empty();
        for id in ids {
            self.remove(&id);
        }
        removed
    }

    fn schedule_next(&self) {
        let (job, active) = {
            let mut state = self.inner.state();
            if let Some(active) = state.active.as_ref() {
                // Not spammy: schedule_next runs on user actions, not the tick.
                let phase = state
                    .manifest
                    .jobs
                    .iter()
                    .find(|job| job.id == active.id)
                    .map(|job| job.phase);
                eprintln!(
                    "nova downloads: schedule_next blocked by active {} ({phase:?})",
                    active.id
                );
                return;
            }
            let Some(job) = state
                .manifest
                .jobs
                .iter()
                .find(|job| job.phase == DownloadPhase::Queued)
                .cloned()
            else {
                return;
            };
            if matches!(job.source, JobSource::Torrent { .. })
                && crate::torrent::engine().is_none()
                && active_torrent_settings().enabled
            {
                return;
            }
            let active = ActiveDownload {
                id: job.id.clone(),
                torrent_engine: crate::torrent::engine(),
                cancel: CancellationToken::new(),
                torrent_cancel: Arc::new(AtomicBool::new(false)),
                remove: Arc::new(AtomicBool::new(false)),
                finished: Arc::new(AtomicBool::new(false)),
            };
            if let Some(queued) = state
                .manifest
                .jobs
                .iter_mut()
                .find(|queued| queued.id == job.id)
            {
                queued.phase = DownloadPhase::Resolving;
                queued.error = None;
                queued.updated_at = crate::download::unix_timestamp();
            }
            state.active = Some(active.clone());
            persist_manifest(&state.manifest);
            self.bump_revision();
            (job, active)
        };

        let coordinator = self.clone();
        let job_id = job.id.clone();
        let worker_job_id = job_id.clone();
        let source = job.source.clone();
        let done = active.finished.clone();
        let name = match &source {
            JobSource::Http { .. } => "download-http",
            JobSource::Torrent { .. } => "download-torrent",
        };
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            // Marks the worker ended even if the transfer panics; the tick
            // reaps a slot whose worker died before `finish` cleared it.
            let _done = WorkerDone(done);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match source {
                JobSource::Http { .. } => coordinator.run_http(job, active),
                JobSource::Torrent {
                    info_hash,
                    file_idx,
                } => coordinator.run_torrent(job, info_hash, file_idx, active),
            }));
            if outcome.is_err() {
                eprintln!("nova downloads: worker for {worker_job_id} panicked; recovering");
                coordinator.recover_failed_worker(&worker_job_id);
            }
        });
        if let Err(error) = spawned {
            self.finish(
                job_id,
                Err(format!("could not start download worker: {error}")),
            );
        }
    }

    fn run_http(&self, mut job: DownloadJob, active: ActiveDownload) {
        let private_source = is_resolver_download(&job);
        let resolver = self.inner.state().resolvers.get(&job.id).cloned();
        if private_source {
            let resolver = resolver.clone();
            let Some(resolver) = resolver else {
                self.finish(job.id, Err(RESOLVER_RESELECT.into()));
                return;
            };
            let cancel = active.cancel.clone();
            let is_current = resolver.is_current.clone();
            let cancelled: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                if !is_current() {
                    cancel.cancel();
                }
                cancel.is_cancelled()
            });
            if cancelled() {
                let error = if (resolver.is_current)() {
                    crate::download::DownloadError::Cancelled.to_string()
                } else {
                    RESOLVER_RESELECT.into()
                };
                self.finish(job.id, Err(error));
                return;
            }
            let resolved = nova_providers::resolve_provider_stream(
                &resolver.provider_url,
                &resolver.token,
                Duration::from_secs(30),
                cancelled.clone(),
            );
            if cancelled() {
                let error = if (resolver.is_current)() {
                    crate::download::DownloadError::Cancelled.to_string()
                } else {
                    RESOLVER_RESELECT.into()
                };
                self.finish(job.id, Err(error));
                return;
            }
            match resolved.map(|stream| stream_source(&stream)) {
                Ok(StreamSource::Url(url)) => job.source = JobSource::http(url),
                Ok(StreamSource::UrlWithOptions { url, headers, .. }) => {
                    job.source = JobSource::http_with_headers(url, headers);
                }
                Err(error) => {
                    self.finish(job.id, Err(error.message));
                    return;
                }
                _ => {
                    self.finish(job.id, Err("Could not resolve this stream. Try another result or check the addon configuration.".into()));
                    return;
                }
            }
        }
        let destination = self.http_destination(&job);
        let result = match migrate_legacy_http_partial(&job, &destination)
            .map_err(crate::download::DownloadError::from)
            .and_then(|()| crate::download::new_http_client())
        {
            Ok(client) => {
                let id = job.id.clone();
                let coordinator = self.clone();
                let mut request = job.http_request(destination).expect("HTTP source");
                request.private_source = private_source;
                let resolver = resolver.clone();
                let cancel = active.cancel.clone();
                crate::download::download_http_with_request_blocking(
                    &client,
                    request,
                    active.cancel.clone(),
                    move |progress| {
                        if resolver
                            .as_ref()
                            .is_some_and(|resolver| !(resolver.is_current)())
                        {
                            cancel.cancel();
                            return;
                        }
                        coordinator.update_progress(&id, progress);
                    },
                )
            }
            Err(error) => Err(error),
        };
        let result = result
            .map(|mut outcome| {
                if private_source {
                    outcome.etag = None;
                    outcome.last_modified = None;
                }
                // The transfer reports its on-disk filename. Keep the source label
                // readable in the UI; HLS may change the extension to a playlist.
                let label = job.file_name.clone().unwrap_or_else(|| job_file_name(&job));
                outcome.file_name = if outcome
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "m3u8")
                {
                    Path::new(&label)
                        .with_extension("m3u8")
                        .to_string_lossy()
                        .into_owned()
                } else {
                    label
                };
                outcome
            })
            .map_err(|error| {
                if private_source && !error.is_cancelled() {
                    "Could not download this stream. Retry or check the addon configuration.".into()
                } else {
                    error.to_string()
                }
            });
        let result = if resolver
            .as_ref()
            .is_some_and(|resolver| !(resolver.is_current)())
        {
            // Do not publish a completed artifact for an invalidated owner.
            if let Ok(outcome) = &result {
                let _ = std::fs::remove_file(&outcome.path);
            }
            Err(RESOLVER_RESELECT.into())
        } else {
            result
        };
        self.finish(job.id, result);
    }

    fn run_torrent(
        &self,
        job: DownloadJob,
        info_hash: String,
        file_idx: Option<u32>,
        active: ActiveDownload,
    ) {
        let Some(engine) = active.torrent_engine.clone() else {
            self.finish(job.id, Err("torrent engine unavailable".into()));
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let id = job.id.clone();
        let coordinator = self.clone();
        // Stall watchdog: a torrent can legitimately run for hours, but one
        // that never finds metadata/peers (or stops making progress) must not
        // pin the one download slot forever. Track the last time *real*
        // progress arrived, not merely that the engine ticked.
        let last_activity = Arc::new(AtomicU64::new(crate::download::unix_timestamp()));
        let best_progress = Arc::new(AtomicU64::new(0f64.to_bits()));
        let progress_activity = last_activity.clone();
        let progress_best = best_progress.clone();
        engine.start_download(
            crate::torrent::TorrentDownloadRequest {
                owner_id: job.id.clone(),
                info_hash,
                file_idx,
                requested_name: job.display.clone(),
                output_directory: self.inner.root.join("torrents"),
                cancel: active.torrent_cancel.clone(),
            },
            {
                let coordinator = self.clone();
                let id = job.id.clone();
                move |location| {
                    let mut state = coordinator.inner.state();
                    if let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) {
                        job.torrent_directory = Some(location.directory);
                        job.torrent_file_idx = Some(location.file_idx);
                        persist_manifest(&state.manifest);
                    }
                }
            },
            move |stats| {
                let bits = (stats.progress as f64).max(0.0).to_bits();
                if bits > progress_best.load(Ordering::Relaxed) {
                    progress_best.store(bits, Ordering::Relaxed);
                    progress_activity.store(crate::download::unix_timestamp(), Ordering::Release);
                } else if stats.down_bps > 0 {
                    progress_activity.store(crate::download::unix_timestamp(), Ordering::Release);
                }
                coordinator.update_torrent_progress(&id, stats);
            },
            move |result| {
                let _ = sender.send(result);
            },
        );
        let result = loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(ready)) => {
                    break Ok(DownloadOutcome {
                        path: ready.path,
                        part_path: PathBuf::new(),
                        file_name: ready.file_name,
                        bytes_downloaded: ready.file_len,
                        total_bytes: Some(ready.file_len),
                        bytes_per_second: 0,
                        etag: None,
                        last_modified: None,
                    });
                }
                Ok(Err(error)) => break Err(error),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("torrent worker stopped".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let idle = crate::download::unix_timestamp()
                        .saturating_sub(last_activity.load(Ordering::Acquire));
                    if idle >= TORRENT_STALL_SECS {
                        active.torrent_cancel.store(true, Ordering::Release);
                        break Err(format!(
                            "torrent stalled: no progress for {} min",
                            TORRENT_STALL_SECS / 60
                        ));
                    }
                }
            }
        };
        self.finish(job.id, result);
    }

    fn update_progress(&self, id: &str, progress: DownloadProgress) {
        let mut state = self.inner.state();
        if !state
            .active
            .as_ref()
            .is_some_and(|active| active.id == id && !active.cancel.is_cancelled())
        {
            return;
        }
        let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.apply_progress(&progress);
        if state.last_persist.elapsed() >= Duration::from_secs(2) {
            persist_manifest(&state.manifest);
            state.last_persist = Instant::now();
        }
        self.bump_revision();
    }

    fn update_torrent_progress(&self, id: &str, stats: TorrentStats) {
        let mut state = self.inner.state();
        if !state
            .active
            .as_ref()
            .is_some_and(|active| active.id == id && !active.cancel.is_cancelled())
        {
            return;
        }
        let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.phase = DownloadPhase::Downloading;
        job.bytes_per_second = stats.down_bps;
        if let Some(total) = job.total_bytes {
            job.bytes_downloaded = (total as f64 * stats.progress as f64).round() as u64;
        }
        job.updated_at = crate::download::unix_timestamp();
        if state.last_persist.elapsed() >= Duration::from_secs(2) {
            persist_manifest(&state.manifest);
            state.last_persist = Instant::now();
        }
        self.bump_revision();
    }

    fn finish(&self, id: String, result: Result<DownloadOutcome, String>) {
        // Keep the transfer's active slot/foreground-service lease throughout
        // bounded timing preparation; network/probing work never holds state.
        let (active_snapshot, job_snapshot) = {
            let state = self.inner.state();
            let Some(active) = state.active.as_ref().filter(|active| active.id == id) else {
                return;
            };
            (active.clone(), self.job_unlocked(&state, &id))
        };
        let intro_timings = result.as_ref().ok().and_then(|outcome| {
            let job = job_snapshot.as_ref()?;
            if active_snapshot.cancel.is_cancelled()
                || active_snapshot.remove.load(Ordering::Acquire)
                || !matches!(
                    job.phase,
                    DownloadPhase::Resolving | DownloadPhase::Downloading
                )
            {
                return None;
            }
            let request = job.intro_request.as_ref()?;
            intro_skip::prepare_download(request, &outcome.path, &active_snapshot.cancel)
                .filter(|timings| timings.matches_path(&outcome.path))
        });
        let (remove, job) = {
            let mut state = self.inner.state();
            let Some(active) = state.active.take() else {
                return;
            };
            if active.id != id || !Arc::ptr_eq(&active.finished, &active_snapshot.finished) {
                state.active = Some(active);
                return;
            }
            let job = state.manifest.jobs.iter_mut().find(|job| {
                job.id == id
                    && job_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| same_download_identity(job, snapshot))
            });
            if let Some(job) = job {
                let unchanged_phase = job_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| job.phase == snapshot.phase);
                let result = if result.is_ok() && (active.cancel.is_cancelled() || !unchanged_phase)
                {
                    Err("cancelled".to_owned())
                } else {
                    result
                };
                if active.remove.load(Ordering::Acquire) {
                    state.manifest.jobs.retain(|job| job.id != id);
                } else if unchanged_phase || active.cancel.is_cancelled() {
                    match result {
                        Ok(outcome) => {
                            job.apply_outcome(&outcome);
                            job.phase = DownloadPhase::Completed;
                            job.intro_timings = intro_timings.filter(|timings| {
                                !active.cancel.is_cancelled()
                                    && !active.remove.load(Ordering::Acquire)
                                    && timings.matches_path(&outcome.path)
                            });
                        }
                        Err(error) => {
                            if error == RESOLVER_RESELECT {
                                // A fresh explicit selection may already have
                                // rearmed this row while the old worker exits.
                                if job.phase != DownloadPhase::Queued {
                                    job.phase = DownloadPhase::Failed;
                                    job.error = Some(error);
                                }
                            } else if active.cancel.is_cancelled() {
                                // Resume can queue the job while the cancelled
                                // worker is still unwinding. Preserve that intent.
                                if job.phase != DownloadPhase::Queued {
                                    job.phase = DownloadPhase::Paused;
                                }
                                job.error = None;
                            } else {
                                job.phase = DownloadPhase::Failed;
                                job.error = Some(error);
                            }
                        }
                    }
                    job.bytes_per_second = 0;
                    job.updated_at = crate::download::unix_timestamp();
                }
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
            // A replacement may reuse the removed ID while this worker
            // finishes. Its files belong to the new row, not this cleanup.
            let remove = active.remove.load(Ordering::Acquire)
                && !state.manifest.jobs.iter().any(|job| job.id == id);
            (remove, job_snapshot)
        };
        if remove {
            if let Some(job) = job {
                self.remove_artifacts(&job);
            }
            self.remove_artifacts_for_id(&id);
        }
        self.schedule_next();
    }

    /// Recover a job whose worker ended without finalizing (a panic mid-
    /// transfer or mid-`finish`): release the active slot when it still names
    /// the job, mark a still-running job `Failed`, and advance the queue.
    /// Idempotent, so both the panic path and the tick reaper can call it.
    fn recover_failed_worker(&self, id: &str) {
        let removed = {
            let mut state = self.inner.state();
            if state.active.as_ref().is_some_and(|active| active.id == id) {
                state.active = None;
            }
            let still_present = state.manifest.jobs.iter().any(|job| job.id == id);
            if still_present
                && let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id)
                && matches!(
                    job.phase,
                    DownloadPhase::Resolving | DownloadPhase::Downloading
                )
            {
                job.phase = DownloadPhase::Failed;
                job.error = Some("Download worker stopped unexpectedly.".into());
                job.bytes_per_second = 0;
                job.updated_at = crate::download::unix_timestamp();
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
            !still_present
        };
        // A job removed while active had its artifact cleanup deferred to the
        // worker's exit; that has now happened (panic unwind or finished flag).
        if removed {
            self.remove_artifacts_for_id(id);
        }
        self.schedule_next();
    }

    /// 250 ms UI tick: if the active worker has ended (drop guard set) but the
    /// slot is still held, the worker died without finalizing — recover it so
    /// queued jobs cannot wait forever behind a dead transfer. Also self-heals
    /// a `Queued` job that has no active worker (a scheduling call that was
    /// somehow missed). Returns true when it changed the queue.
    pub(crate) fn reap_finished_worker(&self) -> bool {
        let dead = {
            let state = self.inner.state();
            state
                .active
                .as_ref()
                .filter(|active| active.finished.load(Ordering::Acquire))
                .map(|active| active.id.clone())
        };
        if let Some(id) = dead {
            eprintln!("nova downloads: worker for {id} ended without finalizing; recovering");
            self.recover_failed_worker(&id);
            return true;
        }
        // Self-heal: a queued job with no active worker should always be
        // running. Only kick when genuinely idle, so this cannot log-spam.
        let idle_with_queued = {
            let state = self.inner.state();
            state.active.is_none()
                && state
                    .manifest
                    .jobs
                    .iter()
                    .any(|job| job.phase == DownloadPhase::Queued)
        };
        if idle_with_queued {
            self.schedule_next();
            return true;
        }
        false
    }

    fn job_unlocked(&self, state: &DownloadState, id: &str) -> Option<DownloadJob> {
        state.manifest.jobs.iter().find(|job| job.id == id).cloned()
    }

    fn http_destination(&self, job: &DownloadJob) -> PathBuf {
        http_job_destination(&self.inner.root, job)
    }

    fn remove_artifacts(&self, job: &DownloadJob) {
        if matches!(job.source, JobSource::Torrent { .. }) {
            return;
        }
        let path = job
            .artifact_path
            .clone()
            .unwrap_or_else(|| self.http_destination(job));
        self.remove_owned_path(&path);
    }

    fn remove_artifacts_for_id(&self, id: &str) {
        self.remove_owned_dir(&self.inner.root.join("http").join(id));
    }

    fn remove_owned_path(&self, path: &Path) {
        let root = canonical_for_root(&self.inner.root);
        let parent = path.parent().unwrap_or(path).to_path_buf();
        for target in [path.to_path_buf(), part_path(path), parent] {
            if !canonical_for_root(&target).starts_with(&root) {
                continue;
            }
            if target.is_dir() {
                let _ = std::fs::remove_dir_all(target);
            } else {
                let _ = std::fs::remove_file(target);
            }
        }
    }

    fn remove_owned_dir(&self, path: &Path) {
        let root = canonical_for_root(&self.inner.root);
        if canonical_for_root(path).starts_with(&root) {
            let _ = std::fs::remove_dir_all(path);
        }
    }

    fn bump_revision(&self) {
        self.inner.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn persist(&self) {
        let state = self.inner.state();
        persist_manifest(&state.manifest);
    }
}

/// Canonical form of `path` for the downloads-root containment check.
///
/// Android exposes the same directory as `/data/user/0/<pkg>/files` and
/// `/data/data/<pkg>/files`; a lexical `starts_with` across those spellings
/// silently skipped deletion, leaving the file on disk after the row was gone.
/// The file may already be absent (a `.part` sibling, or a retried delete), so
/// fall back to canonicalizing the parent and rejoining the file name.
fn canonical_for_root(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = std::fs::canonicalize(parent)
    {
        return parent.join(name);
    }
    path.to_path_buf()
}

// A 240-byte display filename fits alone but its 17-byte resume suffix does
// not. Keep legacy destinations only when every sibling can fit too.
fn legacy_http_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
                )
        })
        && !name.ends_with([' ', '.'])
        && !crate::download::is_reserved_filename(name)
}
fn http_job_destination(root: &Path, job: &DownloadJob) -> PathBuf {
    let directory = root.join("http").join(&job.id);
    if let Some(path) = &job.artifact_path
        && path
            .parent()
            .is_some_and(|parent| canonical_for_root(parent) == canonical_for_root(&directory))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                legacy_http_filename(name)
                    && (name.len() <= 238
                        || (job.phase == DownloadPhase::Completed && path.is_file()))
            })
    {
        return path.clone();
    }
    if let Some(name) = job.file_name.as_deref()
        && legacy_http_filename(name)
        && name.len() <= 238
    {
        return directory.join(name);
    }
    directory.join(artifact_filename(
        &job.id,
        job.file_name.as_deref().unwrap_or(&job.display),
    ))
}
fn resume_sidecar(path: &Path) -> PathBuf {
    let mut name = part_path(path).into_os_string();
    name.push(".resume.json");
    PathBuf::from(name)
}
/// Create a sibling without replacing an existing transfer. Some Android
/// external filesystems cannot hard-link; copy into a create_new file there.
fn copy_file_no_replace(from: &Path, to: &Path) -> std::io::Result<bool> {
    let mut source = std::fs::File::open(from)?;
    let mut target = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = std::io::copy(&mut source, &mut target).and_then(|_| target.sync_all()) {
        drop(target);
        let _ = std::fs::remove_file(to);
        return Err(error);
    }
    Ok(true)
}
fn link_or_copy_no_replace(from: &Path, to: &Path) -> std::io::Result<bool> {
    match std::fs::hard_link(from, to) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(_) => copy_file_no_replace(from, to),
    }
}
/// Move an oversized legacy partial into the new short name on its worker.
/// Keep the original body and validator until both siblings are ready. If a
/// competing transfer already owns either new path, preserve its whole pair.
fn migrate_legacy_http_partial(job: &DownloadJob, destination: &Path) -> std::io::Result<()> {
    let Some(directory) = destination.parent() else {
        return Ok(());
    };
    let legacy = job
        .artifact_path
        .as_ref()
        .filter(|path| {
            path.parent()
                .is_some_and(|parent| canonical_for_root(parent) == canonical_for_root(directory))
        })
        .cloned()
        .or_else(|| {
            job.file_name
                .as_deref()
                .filter(|name| legacy_http_filename(name))
                .map(|name| directory.join(name))
        });
    let Some(legacy) = legacy.filter(|legacy| legacy != destination) else {
        return Ok(());
    };
    let old_part = part_path(&legacy);
    let new_part = part_path(destination);
    let old_sidecar = resume_sidecar(&legacy);
    let new_sidecar = resume_sidecar(destination);
    let regular_file = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
    };
    if !regular_file(&old_part) || new_part.exists() || new_sidecar.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(directory)?;
    if !link_or_copy_no_replace(&old_part, &new_part)? {
        return Ok(());
    }
    if regular_file(&old_sidecar) {
        match link_or_copy_no_replace(&old_sidecar, &new_sidecar) {
            Ok(true) => {}
            Ok(false) => {
                let _ = std::fs::remove_file(&new_part);
                return Ok(());
            }
            Err(error) => {
                let _ = std::fs::remove_file(&new_part);
                return Err(error);
            }
        }
        std::fs::remove_file(old_sidecar)?;
    }
    std::fs::remove_file(old_part)?;
    Ok(())
}

fn job_file_name(job: &DownloadJob) -> String {
    let candidate = sanitize_filename(&job.display);
    if candidate == "download" {
        format!("{}.bin", job.id)
    } else {
        candidate
    }
}

fn torrent_owner(job: &DownloadJob, root: &Path) -> Option<crate::torrent::RetainedDownload> {
    let JobSource::Torrent {
        info_hash,
        file_idx,
    } = &job.source
    else {
        return None;
    };
    Some(crate::torrent::RetainedDownload {
        owner_id: job.id.clone(),
        info_hash: info_hash.clone(),
        requested_file_idx: *file_idx,
        directory: crate::torrent::download_directory(
            info_hash,
            &root.join("torrents"),
            job.torrent_directory.as_deref(),
        ),
        file_idx: job.torrent_file_idx,
        complete: job.phase == DownloadPhase::Completed,
    })
}

fn torrent_owners(
    manifest: &DownloadManifest,
    root: &Path,
) -> Vec<crate::torrent::RetainedDownload> {
    manifest
        .jobs
        .iter()
        .filter_map(|job| torrent_owner(job, root))
        .collect()
}

pub(super) fn persisted_torrent_owners() -> Vec<crate::torrent::RetainedDownload> {
    torrent_owners(&load_manifest_raw(), &app_data_dir().join("downloads"))
}

fn load_manifest(root: &Path) -> DownloadManifest {
    let mut manifest = load_manifest_raw();
    adopt_orphaned_artifacts(root, &mut manifest);
    normalize_manifest(&mut manifest, root);
    manifest
}

/// Parse the persisted manifest. A corrupt blob is quarantined (like
/// `nova-sync`'s store) instead of silently wiped, so a bad write cannot destroy
/// the completed-download list without a recoverable backup.
fn load_manifest_raw() -> DownloadManifest {
    let Some(raw) = crate::storage::get_str(DOWNLOADS_KEY) else {
        return DownloadManifest::default();
    };
    match DownloadManifest::from_json(&raw) {
        Ok(manifest) if manifest.version == 1 => manifest,
        Ok(manifest) => {
            eprintln!(
                "nova downloads: unsupported manifest version {}",
                manifest.version
            );
            DownloadManifest::default()
        }
        Err(error) => {
            let key = format!(
                "{DOWNLOAD_CORRUPT_PREFIX}{}",
                crate::nova_config::now_secs()
            );
            crate::storage::set_str(&key, &raw);
            crate::storage::remove(DOWNLOADS_KEY);
            eprintln!("nova downloads: quarantined corrupt manifest: {error}");
            DownloadManifest::default()
        }
    }
}

/// Warn when a quarantined manifest backup exists: the live list was reset at
/// some point and can be recovered from the `<key>.corrupt.<ts>` row.
fn log_corrupt_manifest_backups() {
    let backups = crate::storage::scan_prefix(DOWNLOAD_CORRUPT_PREFIX);
    if !backups.is_empty() {
        eprintln!(
            "nova downloads: {} quarantined manifest backup(s) found; \
             completed downloads may need recovery",
            backups.len()
        );
    }
}

/// Bring a loaded/adopted manifest back to a runnable state: interrupted
/// transfers return to `Queued`, and `Completed` jobs whose artifact is gone
/// (or moved) are demoted so the UI does not offer a dead "Play" action.
fn normalize_manifest(manifest: &mut DownloadManifest, root: &Path) {
    for job in &mut manifest.jobs {
        // Selection tokens and signed links are deliberately runtime-only.
        // A restart cannot silently re-resolve an old account's request.
        if is_resolver_download(job) && job.phase != DownloadPhase::Completed {
            job.phase = DownloadPhase::Failed;
            job.error = Some(RESOLVER_RESELECT.into());
        }
        if matches!(
            job.phase,
            DownloadPhase::Resolving | DownloadPhase::Downloading
        ) {
            job.phase = DownloadPhase::Queued;
            job.error = None;
        }
        if job.phase == DownloadPhase::Completed && !artifact_is_valid(job, root) {
            eprintln!(
                "nova downloads: completed job {} has no artifact under {}; marking failed",
                job.id,
                root.display()
            );
            job.phase = DownloadPhase::Failed;
            job.error = Some("Downloaded file is missing.".into());
        }
    }
}

/// Whether a completed job's stored artifact still exists under the downloads
/// root. Both sides are canonicalized first: Android can report the same
/// directory as `/data/user/0/<pkg>/files` and `/data/data/<pkg>/files`, and a
/// lexical `starts_with` would wrongly demote every completed job (files
/// intact, Settings list emptied).
fn artifact_is_valid(job: &DownloadJob, root: &Path) -> bool {
    let Some(path) = job.artifact_path.as_deref() else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    if let Some(owner) = torrent_owner(job, root) {
        return crate::torrent::path_is_within(path, &owner.directory);
    }
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical_path.starts_with(&canonical_root)
}

/// Re-adopt completed artifacts found on disk that the manifest does not
/// reference. Safety net for a lost/reset `downloads:v1` row while the media
/// files survive: the files come back into the list as best-effort entries
/// instead of becoming invisible. Metadata is derived from the path, so
/// titles may be generic (the real label lives only in the manifest).
///
/// Only HTTP jobs are re-adopted: they live in a per-job directory
/// (`http/<job id>/<file>`) that maps back to a job id, and only the finished
/// file is present (in-progress transfers are `.part`). Torrent artifacts are
/// deliberately *not* re-adopted: librqbit writes the final filename from the
/// start (sparse), so a partial torrent is indistinguishable from a complete
/// one on disk.
fn adopt_orphaned_artifacts(root: &Path, manifest: &mut DownloadManifest) {
    let known: HashSet<PathBuf> = manifest
        .jobs
        .iter()
        .filter_map(|job| job.artifact_path.as_deref())
        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        .collect();
    let known_ids: HashSet<String> = manifest.jobs.iter().map(|job| job.id.clone()).collect();
    let mut adopted = 0usize;

    if let Ok(entries) = std::fs::read_dir(root.join("http")) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some((path, len)) = directory_artifact(&entry.path()) else {
                continue;
            };
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if known.contains(&canonical) || known_ids.contains(id.as_str()) {
                continue;
            }
            manifest
                .jobs
                .push(adopted_job(id, JobSource::http(String::new()), &path, len));
            adopted += 1;
        }
    }

    if adopted > 0 {
        eprintln!("nova downloads: re-adopted {adopted} completed artifact(s) from disk");
    }
}

/// Largest completed artifact directly inside `dir`, if any. Partial bodies
/// and their resume metadata are never adopted as completed media.
fn directory_artifact(dir: &Path) -> Option<(PathBuf, u64)> {
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "part")
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".part.resume.json"))
        {
            continue;
        }
        let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        if best.as_ref().is_none_or(|(_, best_len)| len > *best_len) {
            best = Some((path, len));
        }
    }
    best
}

/// Best-effort `Completed` job for a re-adopted artifact. `media_type` is left
/// empty so the row is distinguishable from a real series episode; the
/// Settings list includes these so recovered files are still reachable.
fn adopted_job(id: String, source: JobSource, path: &Path, len: u64) -> DownloadJob {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&file_name)
        .to_string();
    let mut job = DownloadJob::new(id, source);
    job.phase = DownloadPhase::Completed;
    job.display = file_name.clone();
    job.title = title;
    job.file_name = Some(file_name);
    job.artifact_path = Some(path.to_path_buf());
    job.bytes_downloaded = len;
    job.total_bytes = Some(len);
    job
}

fn persist_manifest(manifest: &DownloadManifest) {
    match manifest.to_json() {
        Ok(raw) => crate::storage::set_str(DOWNLOADS_KEY, &raw),
        Err(error) => eprintln!("nova downloads: could not serialize manifest: {error}"),
    }
}

/// Download manifests from older builds may contain one of these fixed errors
/// already rendered in Croatian. Keep the persisted diagnostic language-free
/// going forward, and normalize legacy values when building the UI.
fn localized_download_error(error: &str) -> String {
    match error {
        "Download worker stopped unexpectedly."
        | "Radna dretva za preuzimanje neočekivano se zaustavila." => {
            text::tr("Download worker stopped unexpectedly.").to_string()
        }
        RESOLVER_RESELECT => text::tr(RESOLVER_RESELECT).to_string(),
        "Could not download this stream. Retry or check the addon configuration." => {
            text::tr("Could not download this stream. Retry or check the addon configuration.")
                .to_string()
        }
        "Downloaded file is missing." | "Preuzeta datoteka nedostaje." => {
            text::tr("Downloaded file is missing.").to_string()
        }
        "HTTP 429: download remained rate limited after retries"
        | "HTTP 429: server cooldown exceeds the download retry budget; try again later" => {
            text::tr(error).to_string()
        }
        _ => error.to_string(),
    }
}

pub(crate) fn read_download_settings() -> DownloadSettings {
    read_json(DOWNLOAD_SETTINGS_KEY).unwrap_or_default()
}

pub(crate) fn write_download_settings(settings: &DownloadSettings) {
    write_json(DOWNLOAD_SETTINGS_KEY, settings);
}

impl DownloadCoordinator {
    pub(crate) fn status_text(job: &DownloadJob) -> String {
        match job.phase {
            DownloadPhase::Queued => text::tr("Queued").into(),
            DownloadPhase::Resolving => text::tr("Preparing…").into(),
            DownloadPhase::Downloading => {
                let amount = match job.total_bytes {
                    Some(total) => format!(
                        " · {} / {} ({})",
                        format_bytes(job.bytes_downloaded),
                        format_bytes(total),
                        format_percent(job.bytes_downloaded, Some(total))
                    ),
                    None => match job.resource_progress {
                        Some((completed, total)) => format!(
                            " · {} ({})",
                            format_bytes(job.bytes_downloaded),
                            format_percent(completed, Some(total))
                        ),
                        None => format!(" · {}", format_bytes(job.bytes_downloaded)),
                    },
                };
                let speed = if job.bytes_per_second > 0 {
                    format!(" · {}", crate::download::format_speed(job.bytes_per_second))
                } else {
                    String::new()
                };
                format!("{}{amount}{speed}", text::tr("Downloading"))
            }
            DownloadPhase::Paused => text::tr("Paused").into(),
            // The file name is noise in the stream list (the row already
            // names the release); "Downloaded" is the whole story.
            DownloadPhase::Completed => text::tr("Downloaded").into(),
            DownloadPhase::Failed => job
                .error
                .as_deref()
                .map(localized_download_error)
                .unwrap_or_else(|| text::tr("Download failed").into()),
        }
    }

    pub(crate) fn action_kind(job: &DownloadJob) -> i32 {
        match job.phase {
            DownloadPhase::Queued | DownloadPhase::Resolving | DownloadPhase::Downloading => 1,
            DownloadPhase::Paused => 2,
            DownloadPhase::Failed => 3,
            DownloadPhase::Completed => 4,
        }
    }

    pub(crate) fn action_label(job: &DownloadJob) -> &'static str {
        match job.phase {
            DownloadPhase::Queued | DownloadPhase::Resolving | DownloadPhase::Downloading => {
                text::tr("Pause download")
            }
            DownloadPhase::Paused => text::tr("Resume download"),
            DownloadPhase::Failed => text::tr("Retry download"),
            DownloadPhase::Completed => text::tr("Play downloaded file"),
        }
    }

    /// Whether a download is queued or actively transferring. Drives the
    /// Android foreground service: active work keeps the process alive.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(crate) fn has_active_work(&self) -> bool {
        let state = self.inner.state();
        state.active.is_some()
            || state.manifest.jobs.iter().any(|job| {
                matches!(
                    job.phase,
                    DownloadPhase::Queued | DownloadPhase::Resolving | DownloadPhase::Downloading
                )
            })
    }

    /// `(title, text)` for the background notification: how many downloads are
    /// pending/active and the overall progress of the active one.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(crate) fn background_status(&self) -> (String, String) {
        let state = self.inner.state();
        let active = state
            .manifest
            .jobs
            .iter()
            .find(|job| job.phase == DownloadPhase::Downloading)
            .or_else(|| {
                state
                    .manifest
                    .jobs
                    .iter()
                    .find(|job| job.phase == DownloadPhase::Resolving)
            });
        let pending = state
            .manifest
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.phase,
                    DownloadPhase::Queued | DownloadPhase::Resolving | DownloadPhase::Downloading
                )
            })
            .count();
        let title = text::downloading_files(pending);
        let text = match active {
            Some(job) => {
                let name = first_line(&job.display);
                match job
                    .resource_progress
                    .or(job.total_bytes.map(|total| (job.bytes_downloaded, total)))
                {
                    Some((completed, total)) if total > 0 => {
                        format!("{name} · {}", format_percent(completed, Some(total)))
                    }
                    _ => name,
                }
            }
            None => text::tr("Preparing…").into(),
        };
        (title, text)
    }

    pub(crate) fn progress_fraction(job: &DownloadJob) -> f32 {
        job.resource_progress
            .or(job.total_bytes.map(|total| (job.bytes_downloaded, total)))
            .map(|(completed, total)| {
                if total == 0 {
                    1.0
                } else {
                    (completed as f64 / total as f64).clamp(0.0, 1.0) as f32
                }
            })
            .unwrap_or(0.0)
    }
}

impl Bridge {
    pub(super) fn open_stream_action(&self, stream_id: &str) {
        let Some(app) = self.app() else { return };
        let stream = self
            .shared
            .lock()
            .unwrap()
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .cloned();
        let Some(stream) = stream else { return };
        let (title, items) = match &stream.download {
            Some(job) => {
                let mut items = vec![SheetItem {
                    label: SharedString::from(DownloadCoordinator::action_label(job)),
                    enabled: true,
                }];
                items.push(SheetItem {
                    label: SharedString::from(text::tr("Remove download")),
                    enabled: true,
                });
                (
                    SharedString::from(format!("{} · {}", first_line(&job.display), job.addon)),
                    items,
                )
            }
            None => {
                let downloadable = match &stream.source {
                    StreamSource::Url(url) => crate::download::validate_download_url(url).is_ok(),
                    StreamSource::UrlWithOptions { url, .. } => {
                        crate::download::validate_download_url(url).is_ok()
                    }
                    StreamSource::Torrent { .. } | StreamSource::Resolver { .. } => true,
                    StreamSource::Unsupported | StreamSource::Downloaded { .. } => false,
                };
                let mut items = vec![SheetItem {
                    label: SharedString::from(text::tr("Play stream")),
                    enabled: true,
                }];
                if downloadable {
                    items.push(SheetItem {
                        label: SharedString::from(text::tr("Download")),
                        enabled: true,
                    });
                }
                (SharedString::from(first_line(&stream.display)), items)
            }
        };
        app.set_stream_action_id(SharedString::from(stream_id));
        app.set_stream_action_title(title);
        app.set_stream_action_items(Rc::new(VecModel::from(items)).into());
        app.set_stream_action_open(true);
    }

    pub(super) fn stream_action_selected(&self, stream_id: &str, action: i32) {
        let stream = self
            .shared
            .lock()
            .unwrap()
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .cloned();
        let Some(stream) = stream else { return };
        if let Some(job) = stream.download {
            match action {
                0 => match job.phase {
                    DownloadPhase::Completed => {
                        self.play_download(&job.id);
                    }
                    DownloadPhase::Paused | DownloadPhase::Failed => self.downloads.resume(&job.id),
                    _ => self.downloads.pause(&job.id),
                },
                1 => self.downloads.remove(&job.id),
                _ => {}
            }
            self.apply_stream_filter();
            return;
        }
        if action == 0 {
            if let Some(index) = self
                .shared
                .lock()
                .unwrap()
                .streams
                .iter()
                .position(|candidate| candidate.id == stream_id)
            {
                self.stream_picked(index);
            }
        } else if action == 1 {
            self.download_stream(&stream);
        }
    }

    pub(super) fn download_stream(&self, stream: &StreamUi) {
        if let StreamSource::Unsupported = stream.source {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "This stream cannot be downloaded here.",
            )));
            return;
        }
        let url = match &stream.source {
            StreamSource::Url(url) | StreamSource::UrlWithOptions { url, .. } => Some(url),
            _ => None,
        };
        if url.is_some_and(|url| crate::download::validate_download_url(url).is_err()) {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "This stream cannot be downloaded here.",
            )));
            return;
        }
        if matches!(stream.source, StreamSource::Torrent { .. })
            && (!active_torrent_settings().enabled || crate::torrent::engine().is_none())
        {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "P2P downloads are disabled or unavailable.",
            )));
            return;
        }
        let (media_type, media_id, request_id, title, year) = {
            let state = self.shared.lock().unwrap();
            let modal = state.modal_item.as_ref();
            (
                modal.map(|m| m.type_.clone()).unwrap_or_default(),
                modal.map(|m| m.id.clone()).unwrap_or_default(),
                modal.map(|m| m.request_id.clone()).unwrap_or_default(),
                modal.map(|m| m.name.clone()).unwrap_or_default(),
                modal.map(|m| m.year.clone()).unwrap_or_default(),
            )
        };
        let resolver = match &stream.source {
            StreamSource::Resolver {
                provider_url,
                token,
                settings_generation,
                addon_generation,
            } => {
                let shared = Arc::downgrade(&self.shared);
                let owner = provider_url.clone();
                let settings_generation = *settings_generation;
                let addon_generation = *addon_generation;
                let is_current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                    nova_providers::plugin_settings::settings_generation() == settings_generation
                        && shared.upgrade().is_some_and(|shared| {
                            shared.lock().unwrap().installed.iter().any(|addon| {
                                addon.url == owner
                                    && addon.enabled
                                    && addon.available
                                    && addon.generation == addon_generation
                            })
                        })
                });
                if !is_current() {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This stream selection is out of date. Reload streams.",
                    )));
                    return;
                }
                Some(ResolverDownload {
                    provider_url: provider_url.clone(),
                    token: token.clone(),
                    is_current,
                })
            }
            _ => None,
        };
        let source = match &stream.source {
            StreamSource::Url(url) => JobSource::http(url.clone()),
            StreamSource::UrlWithOptions { url, headers, .. } => {
                JobSource::http_with_headers(url.clone(), headers.clone())
            }
            StreamSource::Torrent {
                info_hash,
                file_idx,
            } => JobSource::torrent(info_hash.clone(), *file_idx),
            StreamSource::Resolver { .. } => {
                let Some(source) = resolver_download_source(&stream.id) else {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This stream selection is out of date. Reload streams.",
                    )));
                    return;
                };
                source
            }
            StreamSource::Unsupported => return,
            StreamSource::Downloaded { .. } => return,
        };
        let request = NewDownload {
            media_type,
            media_id,
            request_id,
            title,
            year,
            display: stream.display.clone(),
            addon: stream.addon.clone(),
            source,
        };
        if let Some(resolver) = resolver {
            self.downloads
                .enqueue_with_resolver(request, Some(resolver));
        } else {
            self.downloads.enqueue(request);
        }
        self.apply_stream_filter();
    }

    pub(super) fn download_settings_to_ui(&self) {
        let settings = self.shared.lock().unwrap().download_settings.clone();
        if let Some(app) = self.app() {
            app.set_download_auto_delete_watched(settings.auto_delete_watched);
        }
    }

    /// Rebuild the Settings → Downloads → Downloaded episodes list from the
    /// manifest. Completed episode jobs only; movies and partial transfers
    /// are not listed here.
    pub(super) fn downloads_list_to_ui(&self) {
        let rows: Vec<DownloadRow> = self
            .downloads
            .completed_episode_jobs()
            .into_iter()
            .map(|job| {
                // Prefer the real episode label from the cached episode list;
                // fall back to parsing the request id's season/episode tail.
                let label = read_episodes_cache_for(&job.media_type, &job.media_id)
                    .and_then(|videos| episode_context_label_for(&job.request_id, &videos))
                    .unwrap_or_else(|| fallback_episode_label(&job.request_id));
                let display = if job.display.trim().is_empty() {
                    String::new()
                } else {
                    first_line(&job.display)
                };
                let subtitle = if display.is_empty() {
                    label
                } else {
                    format!("{label} · {display}")
                };
                let size = format_bytes(job.total_bytes.unwrap_or(job.bytes_downloaded));
                let details = match job.file_name.as_deref() {
                    Some(name) if !name.is_empty() => format!("{name} · {size}"),
                    _ => size,
                };
                DownloadRow {
                    id: SharedString::from(&job.id),
                    title: SharedString::from(&job.title),
                    subtitle: SharedString::from(&subtitle),
                    details: SharedString::from(&details),
                }
            })
            .collect();
        if let Some(app) = self.app() {
            app.set_downloads_rows(Rc::new(VecModel::from(rows)).into());
        }
    }

    /// Remove one downloaded episode (file + manifest entry) from the list.
    pub(super) fn remove_download(&self, id: &str) {
        self.downloads.remove(id);
        self.apply_stream_filter();
        self.downloads_list_to_ui();
    }

    pub(super) fn set_download_auto_delete_watched(&self, enabled: bool) {
        let settings = DownloadSettings {
            auto_delete_watched: enabled,
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.download_settings = settings.clone();
        }
        write_download_settings(&settings);
        self.download_settings_to_ui();
    }

    pub(super) fn auto_delete_watched_downloads(&self, series_id: &str, episode_ids: &[String]) {
        let (enabled, playing_episode) = {
            let state = self.shared.lock().unwrap();
            (
                state.download_settings.auto_delete_watched,
                state
                    .playback
                    .as_ref()
                    .filter(|target| target.series_id == series_id)
                    .map(|target| target.episode_id.clone()),
            )
        };
        if !enabled {
            return;
        }
        let mut removed = false;
        for episode_id in episode_ids {
            removed |= self.downloads.remove_for_watched_episode(
                series_id,
                episode_id,
                playing_episode.as_ref() == Some(episode_id),
            );
        }
        if removed {
            self.apply_stream_filter();
        }
    }

    /// Re-render the Android foreground notification after a language change.
    pub(super) fn refresh_download_background_status(&self) {
        #[cfg(target_os = "android")]
        {
            let busy = self.downloads.has_active_work();
            let (title, text) = self.downloads.background_status();
            crate::app::android_bg::set_download_service(busy, &title, &text);
        }
    }

    pub(super) fn refresh_download_rows(&self) {
        // Reap a transfer thread that died without finalizing before reading
        // the revision: recovery bumps it, so the UI picks up the Failed row
        // and the next queued job on this same tick.
        self.downloads.cancel_stale_resolvers();
        self.downloads.reap_finished_worker();
        // Keep the Android foreground service in step with the queue. This is
        // deliberately before the revision early-return (the service must stop
        // even when a completion did not change the row set) and is throttled
        // internally to state changes + ~1 Hz.
        self.refresh_download_background_status();
        let revision = self.downloads.revision();
        if revision == self.downloads_seen.load(Ordering::Acquire) {
            return;
        }
        self.downloads_seen.store(revision, Ordering::Release);
        self.apply_stream_filter();
        // Only rebuild the settings list while it could be on screen; this
        // runs on the 250 ms tick and active-transfer progress bumps the
        // revision often.
        if self.app().is_some_and(|app| app.get_show_settings()) {
            self.downloads_list_to_ui();
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Stream")
        .to_string()
}

/// `(season, episode)` parsed from the tail of an episode request id
/// (`<item>:<season>:<episode>`). Unparseable ids sort last.
fn episode_sort_key(request_id: &str) -> (u32, u32) {
    let mut parts = request_id.rsplitn(3, ':');
    let episode = parts.next().and_then(|s| s.parse::<u32>().ok());
    let season = parts.next().and_then(|s| s.parse::<u32>().ok());
    match (season, episode) {
        (Some(s), Some(e)) => (s, e),
        _ => (u32::MAX, u32::MAX),
    }
}

/// Best-effort "S1 E2" label when the episode list is not cached.
fn fallback_episode_label(request_id: &str) -> String {
    match episode_sort_key(request_id) {
        (u32::MAX, _) => String::new(),
        (season, episode) => format!("S{season} E{episode}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_intro_fixture(
        name: &str,
    ) -> (
        DownloadCoordinator,
        DownloadJob,
        DownloadIntroTimings,
        PathBuf,
    ) {
        let root = temp_root(name);
        let coordinator = DownloadCoordinator::new(root.clone());
        let path = root.join("http/intro/episode.mkv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"downloaded episode").unwrap();
        let mut job = DownloadJob::new("intro", JobSource::http("https://example.test/episode"));
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(path.clone());
        let timings = DownloadIntroTimings::capture(
            &path,
            1_440_000,
            vec![crate::download::DownloadIntroSegment {
                kind: 0,
                start_ms: 10_000,
                end_ms: 100_000,
            }],
        )
        .unwrap();
        coordinator.inner.state().manifest.jobs = vec![job.clone()];
        (coordinator, job, timings, root)
    }

    #[test]
    fn downloaded_intro_upgrades_cache_and_removal_rejects_late_publication() {
        let (coordinator, snapshot, timings, root) = completed_intro_fixture("intro-removal");
        assert!(coordinator.cached_intro(&snapshot).is_none());
        assert!(coordinator.retain_intro(&snapshot, timings.clone()));
        assert_eq!(coordinator.cached_intro(&snapshot), Some(timings.clone()));
        let manifest = coordinator.inner.state().manifest.to_json().unwrap();
        assert!(
            DownloadManifest::from_json(&manifest)
                .unwrap()
                .jobs
                .iter()
                .any(|job| job.intro_timings.is_some())
        );
        coordinator.remove(&snapshot.id);
        assert!(!snapshot.artifact_path.as_ref().unwrap().exists());
        assert!(coordinator.cached_intro(&snapshot).is_none());
        assert!(!coordinator.retain_intro(&snapshot, timings));
        assert!(coordinator.job(&snapshot.id).is_none());
        assert!(
            !coordinator
                .inner
                .state()
                .manifest
                .to_json()
                .unwrap()
                .contains("intro_timings")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn downloaded_intro_rejects_reused_identity_phase_and_artifact_changes() {
        let (coordinator, snapshot, timings, root) = completed_intro_fixture("intro-stale");
        for change in 0..4 {
            let mut replacement = snapshot.clone();
            match change {
                0 => replacement.created_at += 1,
                1 => replacement.source = JobSource::http("https://example.test/replacement"),
                2 => replacement.phase = DownloadPhase::Paused,
                _ => replacement.artifact_path = Some(root.join("replacement.mkv")),
            }
            coordinator.inner.state().manifest.jobs = vec![replacement];
            assert!(!coordinator.retain_intro(&snapshot, timings.clone()));
            assert!(coordinator.cached_intro(&snapshot).is_none());
            assert!(
                coordinator
                    .job(&snapshot.id)
                    .unwrap()
                    .intro_timings
                    .is_none()
            );
        }
        coordinator.inner.state().manifest.jobs = vec![snapshot.clone()];
        std::fs::write(
            snapshot.artifact_path.as_ref().unwrap(),
            b"a different encode with a new size",
        )
        .unwrap();
        assert!(!coordinator.retain_intro(&snapshot, timings));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_successful_download_does_not_publish_completed_or_revive_removed_job() {
        let coordinator = DownloadCoordinator::new(temp_root("intro-cancel-finish"));
        coordinator.inner.state().manifest.jobs.clear();
        install_active(&coordinator, "cancelled", DownloadPhase::Downloading, false);
        let path = coordinator.inner.root.join("http/cancelled/episode.mkv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"finished bytes").unwrap();
        let outcome = DownloadOutcome {
            path: path.clone(),
            part_path: part_path(&path),
            file_name: "episode.mkv".into(),
            bytes_downloaded: 14,
            total_bytes: Some(14),
            bytes_per_second: 0,
            etag: None,
            last_modified: None,
        };
        coordinator.pause("cancelled");
        coordinator.finish("cancelled".into(), Ok(outcome.clone()));
        assert_eq!(
            coordinator.job("cancelled").unwrap().phase,
            DownloadPhase::Paused
        );
        install_active(&coordinator, "removed", DownloadPhase::Downloading, false);
        coordinator.remove("removed");
        coordinator.finish("removed".into(), Ok(outcome));
        assert!(coordinator.job("removed").is_none());
        assert!(coordinator.inner.state().active.is_none());
        std::fs::remove_dir_all(&coordinator.inner.root).unwrap();
    }

    #[test]
    fn job_status_exposes_progress_and_action() {
        let mut job = DownloadJob::new("job-1", JobSource::http("https://cdn.example/movie.mp4"));
        job.display = "Addon\nMovie 1080p".into();
        job.phase = DownloadPhase::Downloading;
        job.bytes_downloaded = 512;
        job.total_bytes = Some(1024);
        job.bytes_per_second = 2048;
        let status = DownloadCoordinator::status_text(&job);
        assert!(status.contains("50%"));
        assert!(status.contains("2.0 KiB/s"));
        assert_eq!(DownloadCoordinator::action_kind(&job), 1);
        assert_eq!(DownloadCoordinator::action_label(&job), "Pause download");
        assert!((DownloadCoordinator::progress_fraction(&job) - 0.5).abs() < 0.001);
    }

    #[test]
    fn resolver_download_identity_survives_generations_and_separates_results_and_owners() {
        let stream: addons::Stream = serde_json::from_value(serde_json::json!({
            "url": "https://fixture.example/file.mp4"
        }))
        .unwrap();
        let owner = "nova://supernova";
        let request = "tt123:2:3";
        let stable = nova_providers::stable_stream_identity(owner, request, &stream);
        let first = resolver_download_source(&format!("1:{stable}")).unwrap();
        let reloaded = resolver_download_source(&format!("99:{stable}")).unwrap();
        assert_eq!(first, reloaded);
        let manifest = DownloadManifest::new(vec![DownloadJob::new("existing-job", first.clone())]);
        assert_eq!(
            manifest
                .jobs
                .iter()
                .filter(|job| job.source == reloaded)
                .count(),
            1
        );
        let changed: addons::Stream = serde_json::from_value(serde_json::json!({
            "url": "https://fixture.example/other.mp4"
        }))
        .unwrap();
        for different in [
            nova_providers::stable_stream_identity(owner, request, &changed),
            nova_providers::stable_stream_identity("nova://other-owner", request, &stream),
            nova_providers::stable_stream_identity(owner, "tt123:2:4", &stream),
        ] {
            assert_ne!(
                first,
                resolver_download_source(&format!("2:{different}")).unwrap()
            );
        }
        assert!(resolver_download_source("1:https://private.example/?token=secret").is_none());
        assert!(resolver_download_source("invalid:stream-short").is_none());
    }

    #[test]
    fn resolver_restart_requires_selection_but_keeps_completed_offline_file() {
        let root = temp_root("resolver-restart");
        let artifact = root.join("http/completed/movie.mp4");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"offline").unwrap();
        let mut incomplete = DownloadJob::new(
            "pending",
            JobSource::http(format!("{RESOLVER_DOWNLOAD_PREFIX}stable-id")),
        );
        incomplete.request_id = "tt123:2:3".into();
        incomplete.phase = DownloadPhase::Downloading;
        let mut completed = incomplete.clone();
        completed.id = "completed".into();
        completed.phase = DownloadPhase::Completed;
        completed.artifact_path = Some(artifact);
        let mut manifest = DownloadManifest::new(vec![incomplete, completed]);
        normalize_manifest(&mut manifest, &root);
        assert_eq!(manifest.jobs[0].phase, DownloadPhase::Failed);
        assert_eq!(manifest.jobs[0].error.as_deref(), Some(RESOLVER_RESELECT));
        assert_eq!(manifest.jobs[0].request_id, "tt123:2:3");
        assert_eq!(manifest.jobs[1].phase, DownloadPhase::Completed);
        let json = manifest.to_json().unwrap();
        assert!(!json.contains("https://"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_owner_invalidation_cancels_active_without_persisting_token() {
        let root = temp_root("resolver-owner");
        let coordinator = DownloadCoordinator::new(root.clone());
        install_active(&coordinator, "private", DownloadPhase::Resolving, false);
        let current = Arc::new(AtomicBool::new(true));
        let check = current.clone();
        {
            let mut state = coordinator.inner.state();
            state.manifest.jobs[0].source =
                JobSource::http(format!("{RESOLVER_DOWNLOAD_PREFIX}stream-id"));
            state.resolvers.insert(
                "private".into(),
                ResolverDownload {
                    provider_url: "nova://supernova".into(),
                    token: "runtime-only-private-token".into(),
                    is_current: Arc::new(move || check.load(Ordering::Acquire)),
                },
            );
            assert!(
                !state
                    .manifest
                    .to_json()
                    .unwrap()
                    .contains("runtime-only-private-token")
            );
        }
        coordinator.cancel_stale_resolvers();
        assert!(
            !coordinator
                .inner
                .state()
                .active
                .as_ref()
                .unwrap()
                .cancel
                .is_cancelled()
        );
        current.store(false, Ordering::Release);
        coordinator.cancel_stale_resolvers();
        let state = coordinator.inner.state();
        assert!(state.active.as_ref().unwrap().cancel.is_cancelled());
        assert_eq!(state.manifest.jobs[0].phase, DownloadPhase::Failed);
        assert_eq!(
            state.manifest.jobs[0].error.as_deref(),
            Some(RESOLVER_RESELECT)
        );
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn completed_job_status_keeps_filename() {
        let mut job = DownloadJob::new("job-2", JobSource::torrent("hash", Some(1)));
        job.file_name = Some("movie.mkv".into());
        job.phase = DownloadPhase::Completed;
        assert_eq!(DownloadCoordinator::status_text(&job), "Downloaded");
        assert_eq!(
            DownloadCoordinator::action_label(&job),
            "Play downloaded file"
        );
    }

    fn temp_root(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "nova-downloads-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn long_http_labels_use_stable_hash_paths_with_readable_job_metadata() {
        let root = temp_root("long-http-name");
        let label = format!("{}.mkv", "ć".repeat(300));
        let mut job = DownloadJob::new(
            "download-long",
            JobSource::http("https://cdn.example/video.mkv"),
        );
        job.title = "Readable episode title".into();
        job.display = label.clone();
        job.file_name = Some(job_file_name(&job));
        assert_eq!(job.file_name.as_ref().unwrap().len(), 240);
        let destination = http_job_destination(&root, &job);
        assert!(destination.file_name().unwrap().len() < 100);
        assert_eq!(destination.extension().unwrap(), "mkv");
        assert!(resume_sidecar(&destination).file_name().unwrap().len() <= 255);
        let restored: DownloadJob =
            serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(http_job_destination(&root, &restored), destination);
        assert_eq!(restored.title, "Readable episode title");
        assert_eq!(restored.display, label);
        let directory = destination.parent().unwrap();
        std::fs::create_dir_all(directory).unwrap();
        let old_partial = part_path(directory.join(job.file_name.as_ref().unwrap()));
        std::fs::write(&old_partial, b"downloaded bytes").unwrap();
        migrate_legacy_http_partial(&job, &destination).unwrap();
        assert_eq!(
            std::fs::read(part_path(&destination)).unwrap(),
            b"downloaded bytes"
        );
        assert!(!old_partial.exists());
        migrate_legacy_http_partial(&restored, &destination).unwrap();
        assert_eq!(
            std::fs::read(part_path(&destination)).unwrap(),
            b"downloaded bytes"
        );
        let completed = directory.join(job.file_name.as_ref().unwrap());
        std::fs::write(&completed, b"completed old artifact").unwrap();
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(completed.clone());
        assert_eq!(http_job_destination(&root, &job), completed);
        assert!(artifact_is_valid(&job, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_http_destinations_and_partial_validators_remain_compatible() {
        let root = temp_root("legacy-http-name");
        let directory = root.join("http/download-legacy");
        std::fs::create_dir_all(&directory).unwrap();
        let mut job = DownloadJob::new(
            "download-legacy",
            JobSource::http("https://cdn.example/movie.mp4"),
        );
        job.file_name = Some("movie.mp4".into());
        let legacy = directory.join("movie.mp4");
        assert_eq!(http_job_destination(&root, &job), legacy);
        std::fs::write(&legacy, b"completed legacy file").unwrap();
        job.artifact_path = Some(legacy.clone());
        job.phase = DownloadPhase::Completed;
        assert!(artifact_is_valid(&job, &root));
        assert_eq!(http_job_destination(&root, &job), legacy);
        // Exercise the migration pair independently: both files must retain
        // their contents if a future saved destination needs a new basename.
        std::fs::write(part_path(&legacy), b"partial").unwrap();
        std::fs::write(resume_sidecar(&legacy), b"validator identity").unwrap();
        let hashed = directory.join(artifact_filename(&job.id, "movie.mp4"));
        migrate_legacy_http_partial(&job, &hashed).unwrap();
        assert_eq!(std::fs::read(part_path(&hashed)).unwrap(), b"partial");
        assert_eq!(
            std::fs::read(resume_sidecar(&hashed)).unwrap(),
            b"validator identity"
        );
        assert!(
            legacy.is_file(),
            "completed legacy artifact must remain unchanged"
        );
        assert!(!part_path(&legacy).exists());
        assert!(!resume_sidecar(&legacy).exists());
        // Never replace a body/validator already belonging to a resumed job.
        std::fs::write(part_path(&legacy), b"old partial").unwrap();
        std::fs::write(resume_sidecar(&legacy), b"old validator").unwrap();
        migrate_legacy_http_partial(&job, &hashed).unwrap();
        assert_eq!(std::fs::read(part_path(&hashed)).unwrap(), b"partial");
        assert_eq!(
            std::fs::read(resume_sidecar(&hashed)).unwrap(),
            b"validator identity"
        );
        assert_eq!(std::fs::read(part_path(&legacy)).unwrap(), b"old partial");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_migration_copy_fallback_and_stale_siblings_never_overwrite() {
        let root = temp_root("http-migration-copy");
        let directory = root.join("http/download-copy");
        std::fs::create_dir_all(&directory).unwrap();
        let mut job = DownloadJob::new(
            "download-copy",
            JobSource::http("https://cdn.example/movie.mp4"),
        );
        job.file_name = Some("old.mp4".into());
        let legacy = directory.join("old.mp4");
        let destination = directory.join(artifact_filename(&job.id, "movie.mp4"));
        std::fs::write(part_path(&legacy), b"original body").unwrap();
        std::fs::write(resume_sidecar(&legacy), b"original validator").unwrap();
        std::fs::write(resume_sidecar(&destination), b"existing validator").unwrap();
        migrate_legacy_http_partial(&job, &destination).unwrap();
        assert!(!part_path(&destination).exists());
        assert_eq!(std::fs::read(part_path(&legacy)).unwrap(), b"original body");
        assert_eq!(
            std::fs::read(resume_sidecar(&legacy)).unwrap(),
            b"original validator"
        );
        assert_eq!(
            std::fs::read(resume_sidecar(&destination)).unwrap(),
            b"existing validator"
        );
        // Exercise the fallback directly, regardless of the test filesystem's
        // hard-link support. create_new preserves an already-owned body.
        assert!(copy_file_no_replace(&part_path(&legacy), &part_path(&destination)).unwrap());
        assert!(!copy_file_no_replace(&resume_sidecar(&legacy), &part_path(&destination)).unwrap());
        assert_eq!(
            std::fs::read(part_path(&destination)).unwrap(),
            b"original body"
        );
        assert_eq!(std::fs::read(part_path(&legacy)).unwrap(), b"original body");
        migrate_legacy_http_partial(&job, &destination).unwrap();
        assert_eq!(
            std::fs::read(resume_sidecar(&destination)).unwrap(),
            b"existing validator"
        );
        let rollback = directory.join("rollback.part");
        assert!(copy_file_no_replace(&directory.join("missing-source"), &rollback).is_err());
        assert!(!rollback.exists());
        assert!(copy_file_no_replace(&directory, &rollback).is_err());
        assert!(
            !rollback.exists(),
            "failed stream copy must remove its new file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_hls_episode_leaves_no_bundle_or_partial_data() {
        for completed in [false, true] {
            let root = temp_root("remove-hls");
            let coordinator = DownloadCoordinator::new(root.clone());
            let dir = root.join("http/download-hls");
            let bundle = dir.join("hls");
            std::fs::create_dir_all(&bundle).unwrap();
            for name in [
                "playlist-1.m3u8",
                "asset-0.ts",
                "asset-0.ts.done",
                "asset-1.ts.part",
                "asset-2.key",
                "asset-3.vtt",
            ] {
                std::fs::write(bundle.join(name), b"data").unwrap();
            }
            let mut job = DownloadJob::new(
                "download-hls",
                JobSource::http("https://provider.example/master.m3u8"),
            );
            if completed {
                job.phase = DownloadPhase::Completed;
                let entry = dir.join("episode.m3u8");
                std::fs::write(&entry, b"#EXTM3U\n").unwrap();
                job.artifact_path = Some(entry);
            } else {
                job.phase = DownloadPhase::Paused;
                std::fs::write(dir.join("episode.m3u8.part"), b"partial").unwrap();
            }
            coordinator.inner.state().manifest.jobs.push(job);
            coordinator.remove("download-hls");
            assert!(
                !dir.exists(),
                "all HLS files must be removed, completed={completed}"
            );
            assert!(coordinator.job("download-hls").is_none());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn automatic_hls_cleanup_waits_until_playback_closes() {
        let root = temp_root("auto-remove-hls");
        let coordinator = DownloadCoordinator::new(root.clone());
        let dir = root.join("http/download-hls");
        std::fs::create_dir_all(dir.join("hls")).unwrap();
        let entry = dir.join("episode.m3u8");
        let segment = dir.join("hls/asset-0.ts");
        std::fs::write(&entry, b"#EXTM3U\n").unwrap();
        std::fs::write(&segment, b"video").unwrap();
        let mut job = DownloadJob::new(
            "download-hls",
            JobSource::http("https://provider.example/master.m3u8"),
        );
        job.media_id = "series".into();
        job.request_id = "episode".into();
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(entry.clone());
        coordinator.inner.state().manifest.jobs.push(job);
        assert!(!coordinator.remove_for_watched_episode("series", "episode", true));
        assert!(entry.exists() && segment.exists());
        assert!(coordinator.remove_for_watched_episode("series", "episode", false));
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hls_status_keeps_the_same_amount_format_during_and_between_segments() {
        let mut job = DownloadJob::new("hls", JobSource::http("https://example.com/master.m3u8"));
        for (bytes, completed) in [(99, 50), (100, 50), (124, 65), (125, 65)] {
            job.apply_progress(
                &DownloadProgress::new(bytes * 1024 * 1024, None, 1024)
                    .with_resources(completed, 100),
            );
            let status = DownloadCoordinator::status_text(&job);
            assert!(
                status.contains(&format!("{bytes} MiB ({completed}%)")),
                "{status}"
            );
            assert!(
                !status.contains(" / "),
                "no fabricated byte total: {status}"
            );
            assert!(
                (DownloadCoordinator::progress_fraction(&job) - completed as f32 / 100.0).abs()
                    < 0.001
            );
        }
    }

    /// A completed job whose stored path is the same directory under a
    /// different spelling (Android's `/data/user/0` vs `/data/data`) must not
    /// be demoted: the old lexical `starts_with` check emptied the whole
    /// Settings list while the files sat untouched on disk.
    #[cfg(unix)]
    #[test]
    fn completed_job_validates_through_path_alias() {
        let root = temp_root("alias");
        let real = root.join("real");
        let file = real.join("http/download-abc/movie.mp4");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let mut job = DownloadJob::new("download-abc", JobSource::http("http://x/movie.mp4"));
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(alias.join("http/download-abc/movie.mp4"));
        let mut manifest = DownloadManifest::new(vec![job]);
        normalize_manifest(&mut manifest, &real);
        assert_eq!(manifest.jobs[0].phase, DownloadPhase::Completed);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Deleting a completed job must remove the artifact even when the stored
    /// path uses the other Android spelling of the downloads root. The old
    /// lexical containment check skipped the delete, so the row vanished from
    /// Settings while the file (and its app-storage footprint) stayed.
    #[cfg(unix)]
    #[test]
    fn remove_deletes_artifact_through_path_alias() {
        let root = temp_root("remove-alias");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        // Build the coordinator before the artifact exists, so orphan
        // re-adoption does not add a second job under the same id.
        let coordinator = DownloadCoordinator::new(real.clone());
        let file = real.join("http/download-abc/movie.mp4");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        {
            let mut state = coordinator.inner.state();
            let mut job = DownloadJob::new("download-abc", JobSource::http("http://x/movie.mp4"));
            job.phase = DownloadPhase::Completed;
            // Stored path uses the other spelling of the same directory.
            job.artifact_path = Some(alias.join("http/download-abc/movie.mp4"));
            state.manifest.jobs.push(job);
        }
        coordinator.remove("download-abc");

        assert!(
            !file.exists(),
            "artifact must be deleted through the path alias"
        );
        assert!(coordinator.job("download-abc").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_torrent_job_preserves_sibling_bundle_after_manifest_reload() {
        for reverse in [false, true] {
            let root = temp_root("torrent-siblings");
            let coordinator = DownloadCoordinator::new(root.clone());
            let hash = "abcdef0123456789abcdef0123456789abcdef01";
            let directory = root.join("torrents").join(hash);
            std::fs::create_dir_all(directory.join("season")).unwrap();
            let mut jobs = Vec::new();
            for (index, id) in ["a", "b"].into_iter().enumerate() {
                let path = directory.join("season").join(format!("{id}.mkv"));
                std::fs::write(&path, id).unwrap();
                let mut job = DownloadJob::new(
                    id,
                    JobSource::Torrent {
                        info_hash: hash.into(),
                        file_idx: Some(index as u32),
                    },
                );
                job.phase = DownloadPhase::Completed;
                job.artifact_path = Some(path);
                job.torrent_directory = Some(directory.clone());
                job.torrent_file_idx = Some(index);
                jobs.push(job);
            }
            let json = DownloadManifest::new(jobs).to_json().unwrap();
            let mut manifest = DownloadManifest::from_json(&json).unwrap();
            normalize_manifest(&mut manifest, &root);
            assert!(
                manifest
                    .jobs
                    .iter()
                    .all(|job| job.phase == DownloadPhase::Completed)
            );
            coordinator.inner.state().manifest = manifest;
            let (first, second) = if reverse { ("b", "a") } else { ("a", "b") };
            coordinator.remove(first);
            assert!(directory.join("season/a.mkv").exists());
            assert!(directory.join("season/b.mkv").exists());
            coordinator.remove(second);
            assert!(!directory.exists());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn missing_artifact_marks_completed_job_failed() {
        let root = temp_root("missing");
        let mut job = DownloadJob::new("download-missing", JobSource::http("http://x/movie.mp4"));
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(root.join("http/download-missing/movie.mp4"));
        let mut manifest = DownloadManifest::new(vec![job]);
        normalize_manifest(&mut manifest, &root);
        assert_eq!(manifest.jobs[0].phase, DownloadPhase::Failed);
        assert!(manifest.jobs[0].error.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn orphaned_completed_artifact_is_readopted() {
        let root = temp_root("adopt");
        let file = root.join("http/download-abc/movie.mp4");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        // An incomplete `.part` sibling must not be adopted as completed.
        std::fs::write(file.with_extension("mp4.part"), b"partial").unwrap();
        std::fs::write(
            file.with_extension("mp4.part.resume.json"),
            b"metadata larger than the media",
        )
        .unwrap();

        let mut manifest = DownloadManifest::default();
        adopt_orphaned_artifacts(&root, &mut manifest);
        assert_eq!(manifest.jobs.len(), 1);
        let job = &manifest.jobs[0];
        assert_eq!(job.id, "download-abc");
        assert_eq!(job.phase, DownloadPhase::Completed);
        assert_eq!(job.display, "movie.mp4");
        assert_eq!(job.artifact_path.as_deref(), Some(file.as_path()));
        assert!(job.media_type.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn adoption_skips_artifacts_already_in_manifest() {
        let root = temp_root("dedup");
        let file = root.join("http/download-abc/movie.mp4");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"hello").unwrap();

        let mut job = DownloadJob::new("download-abc", JobSource::http("http://x/movie.mp4"));
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(file.clone());
        let mut manifest = DownloadManifest::new(vec![job]);
        adopt_orphaned_artifacts(&root, &mut manifest);
        assert_eq!(manifest.jobs.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Install a fake active job (and optional queued followers) directly in
    /// the coordinator's state, bypassing the worker spawn.
    fn install_active(
        coordinator: &DownloadCoordinator,
        id: &str,
        phase: DownloadPhase,
        finished: bool,
    ) {
        let mut state = coordinator.inner.state();
        let mut job = DownloadJob::new(id, JobSource::http("http://127.0.0.1:1/never"));
        job.phase = phase;
        state.manifest.jobs.push(job);
        state.active = Some(ActiveDownload {
            id: id.to_string(),
            torrent_engine: None,
            cancel: CancellationToken::new(),
            torrent_cancel: Arc::new(AtomicBool::new(false)),
            remove: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(finished)),
        });
    }

    #[test]
    fn late_progress_preserves_pause_and_resume_intent() {
        let coordinator = DownloadCoordinator::new(temp_root("pause-progress"));
        install_active(
            &coordinator,
            "download-race",
            DownloadPhase::Downloading,
            false,
        );
        coordinator.pause("download-race");
        coordinator.update_progress("download-race", DownloadProgress::new(3, Some(6), 1));
        assert_eq!(
            coordinator.job("download-race").unwrap().phase,
            DownloadPhase::Paused
        );
        coordinator.resume("download-race");
        coordinator.update_torrent_progress("download-race", TorrentStats::default());
        assert_eq!(
            coordinator.job("download-race").unwrap().phase,
            DownloadPhase::Queued
        );
        // Finalization schedules the retry; a refused loopback connection may
        // already have failed by the time we inspect it, but it must not pause.
        coordinator.finish("download-race".into(), Err("cancelled".into()));
        assert_ne!(
            coordinator.job("download-race").unwrap().phase,
            DownloadPhase::Paused
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while coordinator.inner.state().active.is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(coordinator.inner.state().active.is_none());
        let _ = std::fs::remove_dir_all(&coordinator.inner.root);
    }

    #[test]
    fn reaping_a_dead_worker_fails_the_job_and_frees_the_slot() {
        let coordinator = DownloadCoordinator::new(temp_root("reap"));
        install_active(
            &coordinator,
            "download-dead",
            DownloadPhase::Resolving,
            true,
        );

        assert!(coordinator.reap_finished_worker());
        assert!(coordinator.inner.state().active.is_none());
        let job = coordinator.job("download-dead").unwrap();
        assert_eq!(job.phase, DownloadPhase::Failed);
        assert!(job.error.is_some());

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reaping_preserves_a_paused_job() {
        let coordinator = DownloadCoordinator::new(temp_root("reap-paused"));
        install_active(&coordinator, "download-paused", DownloadPhase::Paused, true);

        // The worker died after the user paused: the slot frees, but the
        // user's Paused intent must survive.
        assert!(coordinator.reap_finished_worker());
        assert!(coordinator.inner.state().active.is_none());
        assert_eq!(
            coordinator.job("download-paused").unwrap().phase,
            DownloadPhase::Paused
        );

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn idle_queued_job_is_kicked_by_the_tick_healer() {
        let coordinator = DownloadCoordinator::new(temp_root("heal"));
        {
            let mut state = coordinator.inner.state();
            let mut job = DownloadJob::new(
                "download-queued",
                JobSource::http("http://127.0.0.1:1/never"),
            );
            job.phase = DownloadPhase::Queued;
            state.manifest.jobs.push(job);
        }

        assert!(coordinator.reap_finished_worker());
        // schedule_next moved it out of Queued synchronously (Resolving, or
        // already Failed once the loopback connection was refused).
        assert_ne!(
            coordinator.job("download-queued").unwrap().phase,
            DownloadPhase::Queued
        );

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_live_worker_is_not_reaped() {
        let coordinator = DownloadCoordinator::new(temp_root("reap-live"));
        install_active(
            &coordinator,
            "download-live",
            DownloadPhase::Resolving,
            false,
        );

        assert!(!coordinator.reap_finished_worker());
        let state = coordinator.inner.state();
        assert!(state.active.is_some());
        assert_eq!(state.manifest.jobs[0].phase, DownloadPhase::Resolving);
        drop(state);

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn state_lock_survives_a_poisoned_mutex() {
        let coordinator = DownloadCoordinator::new(temp_root("poison"));
        let poison = coordinator.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison.inner.state();
            panic!("intentional test panic to poison the state mutex");
        })
        .join();

        // The helper must recover the poisoned guard, not cascade-panic.
        assert!(coordinator.inner.state().active.is_none());
        assert!(coordinator.job("missing").is_none());

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn background_policy_tracks_active_queue_work() {
        let coordinator = DownloadCoordinator::new(temp_root("bg"));
        // Idle: no service should be requested.
        assert!(!coordinator.has_active_work());

        install_active(
            &coordinator,
            "download-bg",
            DownloadPhase::Downloading,
            false,
        );
        assert!(coordinator.has_active_work());
        let (title, _text) = coordinator.background_status();
        assert!(title.contains('1'), "{title}");

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }
}

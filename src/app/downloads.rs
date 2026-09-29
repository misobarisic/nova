use super::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{
    CancellationToken, DownloadJob, DownloadManifest, DownloadOutcome, DownloadPhase,
    DownloadProgress, DownloadSource as JobSource, format_bytes, format_percent, part_path,
    sanitize_filename,
};
use crate::torrent::TorrentStats;
use nova_config::DownloadSettings;

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
}

#[derive(Clone)]
struct ActiveDownload {
    id: String,
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
                }),
                next_id: AtomicU64::new(1),
                revision: AtomicU64::new(0),
            }),
        };
        coordinator.persist();
        coordinator
    }

    pub(crate) fn start(&self) {
        self.schedule_next();
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

    pub(crate) fn enqueue(
        &self,
        media_type: String,
        media_id: String,
        request_id: String,
        title: String,
        year: String,
        display: String,
        addon: String,
        source: JobSource,
    ) -> String {
        let id = {
            let mut state = self.inner.state();
            if let Some(index) = state
                .manifest
                .jobs
                .iter()
                .position(|job| job.request_id == request_id && job.source == source)
            {
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
            let id = format!(
                "download-{:016x}",
                self.inner.next_id.fetch_add(1, Ordering::Relaxed)
            );
            let mut job = DownloadJob::new(id.clone(), source);
            job.media_type = media_type;
            job.media_id = media_id;
            job.request_id = request_id;
            job.title = title;
            job.year = (!year.is_empty()).then_some(year);
            job.display = display;
            job.addon = addon;
            job.file_name = Some(job_file_name(&job));
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
        let removed = {
            let mut state = self.inner.state();
            let Some(index) = state.manifest.jobs.iter().position(|job| job.id == id) else {
                return;
            };
            let job = state.manifest.jobs.remove(index);
            if let Some(active) = state.active.as_ref().filter(|active| active.id == id) {
                active.remove.store(true, Ordering::Release);
                active.cancel.cancel();
                active.torrent_cancel.store(true, Ordering::Release);
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
            job
        };
        if let JobSource::Torrent { info_hash, .. } = &removed.source {
            if let Some(engine) = crate::torrent::engine() {
                let _ = engine.remove_retained(info_hash);
            }
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

    pub(crate) fn remove_for_watched_episode(&self, series_id: &str, episode_id: &str) -> bool {
        let ids = {
            self.inner
                .state()
                .manifest
                .jobs
                .iter()
                .filter(|job| job.media_id == series_id && job.request_id == episode_id)
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
            let active = ActiveDownload {
                id: job.id.clone(),
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
        let spawned = thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // Marks the worker ended even if the transfer panics; the tick
                // reaps a slot whose worker died before `finish` cleared it.
                let _done = WorkerDone(done);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match source {
                        JobSource::Http { .. } => coordinator.run_http(job, active),
                        JobSource::Torrent {
                            info_hash,
                            file_idx,
                        } => coordinator.run_torrent(job, info_hash, file_idx, active),
                    }
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

    fn run_http(&self, job: DownloadJob, active: ActiveDownload) {
        let destination = self.http_destination(&job);
        let result = match crate::download::new_http_client() {
            Ok(client) => {
                let id = job.id.clone();
                let coordinator = self.clone();
                crate::download::download_http_for_job_blocking(
                    &client,
                    &job,
                    destination,
                    active.cancel.clone(),
                    move |progress| coordinator.update_progress(&id, progress),
                )
            }
            Err(error) => Err(error),
        };
        let result = result.map_err(|error| error.to_string());
        self.finish(job.id, result);
    }

    fn run_torrent(
        &self,
        job: DownloadJob,
        info_hash: String,
        file_idx: Option<u32>,
        active: ActiveDownload,
    ) {
        let Some(engine) = crate::torrent::engine() else {
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
            info_hash,
            file_idx,
            job.display.clone(),
            self.inner.root.join("torrents"),
            active.torrent_cancel.clone(),
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
        if !state.active.as_ref().is_some_and(|active| active.id == id) {
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
        if !state.active.as_ref().is_some_and(|active| active.id == id) {
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
        let (remove, job) = {
            let mut state = self.inner.state();
            let Some(active) = state.active.take() else {
                return;
            };
            if active.id != id {
                state.active = Some(active);
                return;
            }
            let job = state.manifest.jobs.iter_mut().find(|job| job.id == id);
            if let Some(job) = job {
                if active.remove.load(Ordering::Acquire) {
                    state.manifest.jobs.retain(|job| job.id != id);
                } else {
                    match result {
                        Ok(outcome) => {
                            job.apply_outcome(&outcome);
                            job.phase = DownloadPhase::Completed;
                        }
                        Err(error) => {
                            if active.cancel.is_cancelled() {
                                job.phase = DownloadPhase::Paused;
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
            (
                active.remove.load(Ordering::Acquire),
                self.job_unlocked(&state, &id),
            )
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
            if still_present {
                if let Some(job) = state.manifest.jobs.iter_mut().find(|job| job.id == id)
                    && matches!(
                        job.phase,
                        DownloadPhase::Resolving | DownloadPhase::Downloading
                    )
                {
                    job.phase = DownloadPhase::Failed;
                    job.error = Some(text::tr("Download worker stopped unexpectedly.").into());
                    job.bytes_per_second = 0;
                    job.updated_at = crate::download::unix_timestamp();
                }
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
        let name = job
            .file_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| job_file_name(job));
        self.inner.root.join("http").join(&job.id).join(name)
    }

    fn remove_artifacts(&self, job: &DownloadJob) {
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

fn job_file_name(job: &DownloadJob) -> String {
    let candidate = sanitize_filename(&job.display);
    if candidate == "download" {
        format!("{}.bin", job.id)
    } else {
        candidate
    }
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
            let key = format!("{DOWNLOAD_CORRUPT_PREFIX}{}", crate::nova_config::now_secs());
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
            job.error = Some(text::tr("Downloaded file is missing.").into());
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
            manifest.jobs.push(adopted_job(
                id,
                JobSource::http(String::new()),
                &path,
                len,
            ));
            adopted += 1;
        }
    }

    if adopted > 0 {
        eprintln!("nova downloads: re-adopted {adopted} completed artifact(s) from disk");
    }
}

/// Largest non-`.part` regular file directly inside `dir`, if any. `.part`
/// files are incomplete transfers and are not adopted as completed.
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
        if path.extension().is_some_and(|extension| extension == "part") {
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
                    None => format!(" · {}", format_bytes(job.bytes_downloaded)),
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
                .clone()
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
                match job.total_bytes {
                    Some(total) if total > 0 => format!(
                        "{name} · {}",
                        format_percent(job.bytes_downloaded, Some(total))
                    ),
                    _ => name,
                }
            }
            None => text::tr("Preparing…").into(),
        };
        (title, text)
    }

    pub(crate) fn progress_fraction(job: &DownloadJob) -> f32 {
        job.total_bytes
            .map(|total| {
                if total == 0 {
                    1.0
                } else {
                    (job.bytes_downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
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
                    StreamSource::Url(url) => !crate::download::is_manifest_url(url),
                    StreamSource::Torrent { .. } => true,
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
                        if let Some(path) = job.artifact_path {
                            let _ = self.open_player(path.to_string_lossy().into_owned());
                        }
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
            self.set_streams_hint(text::tr("This stream cannot be downloaded here."));
            return;
        }
        if let StreamSource::Url(url) = &stream.source
            && crate::download::is_manifest_url(url)
        {
            self.set_streams_hint(text::tr(
                "HLS, DASH, and YouTube streams cannot be downloaded here.",
            ));
            return;
        }
        if matches!(stream.source, StreamSource::Torrent { .. })
            && (!active_torrent_settings().enabled || crate::torrent::engine().is_none())
        {
            self.set_streams_hint(text::tr("P2P downloads are disabled or unavailable."));
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
        let source = match &stream.source {
            StreamSource::Url(url) => JobSource::http(url.clone()),
            StreamSource::Torrent {
                info_hash,
                file_idx,
            } => JobSource::torrent(info_hash.clone(), *file_idx),
            StreamSource::Unsupported => return,
            StreamSource::Downloaded { .. } => return,
        };
        self.downloads.enqueue(
            media_type,
            media_id,
            request_id,
            title,
            year,
            stream.display.clone(),
            stream.addon.clone(),
            source,
        );
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
                let display =
                    if job.display.trim().is_empty() { String::new() } else { first_line(&job.display) };
                let subtitle =
                    if display.is_empty() { label } else { format!("{label} · {display}") };
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
        let enabled = self
            .shared
            .lock()
            .unwrap()
            .download_settings
            .auto_delete_watched;
        if !enabled {
            return;
        }
        let mut removed = false;
        for episode_id in episode_ids {
            removed |= self
                .downloads
                .remove_for_watched_episode(series_id, episode_id);
        }
        if removed {
            self.apply_stream_filter();
        }
    }

    pub(super) fn refresh_download_rows(&self) {
        // Reap a transfer thread that died without finalizing before reading
        // the revision: recovery bumps it, so the UI picks up the Failed row
        // and the next queued job on this same tick.
        self.downloads.reap_finished_worker();
        // Keep the Android foreground service in step with the queue. This is
        // deliberately before the revision early-return (the service must stop
        // even when a completion did not change the row set) and is throttled
        // internally to state changes + ~1 Hz.
        #[cfg(target_os = "android")]
        {
            let busy = self.downloads.has_active_work();
            let (title, text) = self.downloads.background_status();
            crate::app::android_bg::set_download_service(busy, &title, &text);
        }
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

    #[test]
    fn job_status_exposes_progress_and_action() {
        let mut job = DownloadJob::new(
            "job-1",
            JobSource::http("https://cdn.example/movie.mp4"),
        );
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
    fn completed_job_status_keeps_filename() {
        let mut job = DownloadJob::new("job-2", JobSource::torrent("hash", Some(1)));
        job.file_name = Some("movie.mkv".into());
        job.phase = DownloadPhase::Completed;
        assert_eq!(DownloadCoordinator::status_text(&job), "Downloaded");
        assert_eq!(DownloadCoordinator::action_label(&job), "Play downloaded file");
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
            cancel: CancellationToken::new(),
            torrent_cancel: Arc::new(AtomicBool::new(false)),
            remove: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(finished)),
        });
    }

    #[test]
    fn reaping_a_dead_worker_fails_the_job_and_frees_the_slot() {
        let coordinator = DownloadCoordinator::new(temp_root("reap"));
        install_active(&coordinator, "download-dead", DownloadPhase::Resolving, true);

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
            let mut job =
                DownloadJob::new("download-queued", JobSource::http("http://127.0.0.1:1/never"));
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
        install_active(&coordinator, "download-live", DownloadPhase::Resolving, false);

        assert!(!coordinator.reap_finished_worker());
        let state = coordinator.inner.state();
        assert!(state.active.is_some());
        assert_eq!(
            state.manifest.jobs[0].phase,
            DownloadPhase::Resolving
        );
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

        install_active(&coordinator, "download-bg", DownloadPhase::Downloading, false);
        assert!(coordinator.has_active_work());
        let (title, _text) = coordinator.background_status();
        assert!(title.contains('1'), "{title}");

        let root = coordinator.inner.root.clone();
        let _ = std::fs::remove_dir_all(&root);
    }
}

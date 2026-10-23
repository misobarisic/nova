use super::*;

#[cfg(test)]
use crate::app::profiles::thread;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
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

type ResolverCancellation = Arc<dyn Fn() -> bool + Send + Sync>;
type ResolveDownload = dyn Fn(
        &str,
        Duration,
        ResolverCancellation,
    ) -> Result<addons::Stream, nova_providers::ProviderResolveError>
    + Send
    + Sync;
#[derive(Clone)]
pub(super) struct ResolverDownload {
    token: String,
    resolve: Arc<ResolveDownload>,
    /// Only provider configuration changes invalidate an already queued
    /// download. Navigation may continue while the captured episode transfers.
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl ResolverDownload {
    pub(super) fn for_offline(
        token: String,
        resolve: impl Fn(
            &str,
            Duration,
            ResolverCancellation,
        ) -> Result<addons::Stream, nova_providers::ProviderResolveError>
        + Send
        + Sync
        + 'static,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            token,
            resolve: Arc::new(resolve),
            is_current,
        }
    }
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

pub(super) struct NewDownload {
    pub(super) media_type: String,
    pub(super) media_id: String,
    pub(super) request_id: String,
    pub(super) title: String,
    pub(super) year: String,
    pub(super) display: String,
    pub(super) addon: String,
    pub(super) source: JobSource,
    pub(super) source_owner: String,
    pub(super) source_revision: String,
}

struct PendingRemoval {
    jobs: Vec<DownloadJob>,
    transfer_engine: Option<(String, Arc<crate::torrent::TorrentEngine>)>,
}

struct DownloadInner {
    root: PathBuf,
    state: Mutex<DownloadState>,
    next_id: AtomicU64,
    revision: AtomicU64,
    foreground: AtomicBool,
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

fn detach_policy_jobs(
    state: &mut DownloadState,
    owner: &crate::download::offline::Owner,
    keep: bool,
    playing: Option<&str>,
) -> HashSet<String> {
    let mut remove = HashSet::new();
    for job in &mut state.manifest.jobs {
        if job.offline.owners.remove(owner)
            && job.offline.owners.is_empty()
            && !job.offline.manual_keep
        {
            if keep && job.phase == DownloadPhase::Completed {
                job.offline.manual_keep = true;
            } else if playing != Some(job.id.as_str()) {
                remove.insert(job.id.clone());
            }
        }
    }
    remove
}

/// Refresh actual physical bytes before admission/restart. Torrent bundles are
/// charged by directory once, including boundary pieces and sibling files.
fn refresh_physical_accounting(jobs: &mut [DownloadJob]) {
    for job in jobs {
        if job.offline.manual_keep {
            continue;
        }
        if let Some(directory) = &job.torrent_directory {
            job.bytes_downloaded = crate::torrent::physical_bytes(directory);
        } else if let Some(path) = &job.artifact_path {
            job.bytes_downloaded = path
                .parent()
                .map(crate::torrent::physical_bytes)
                .unwrap_or(0);
        }
    }
}

impl DownloadCoordinator {
    pub(super) fn available_bytes(&self) -> Option<u64> {
        crate::download::available_bytes(&self.inner.root)
    }
    pub(crate) fn new(root: PathBuf) -> Self {
        Self::new_mode(root, true)
    }
    #[cfg(target_os = "android")]
    pub(super) fn new_headless(root: PathBuf) -> Self {
        Self::new_mode(root, false)
    }
    #[cfg(target_os = "android")]
    pub(super) fn foreground_attached(&self) -> bool {
        self.inner.foreground.load(Ordering::Acquire)
    }
    fn new_mode(root: PathBuf, foreground: bool) -> Self {
        if let Err(error) = std::fs::create_dir_all(&root) {
            eprintln!("nova downloads: cannot create {}: {error}", root.display());
        }
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        static COORDINATORS: std::sync::OnceLock<
            Mutex<HashMap<PathBuf, std::sync::Weak<DownloadInner>>>,
        > = std::sync::OnceLock::new();
        let mut registry = COORDINATORS.get_or_init(Mutex::default).lock().unwrap();
        if let Some(inner) = registry.get(&root).and_then(std::sync::Weak::upgrade) {
            if foreground {
                inner.foreground.store(true, Ordering::Release);
            }
            return Self { inner };
        }

        let manifest = load_manifest(&root);
        log_corrupt_manifest_backups();
        // A removed row can still own asynchronous file/torrent cleanup after
        // leaving the manifest. Continue above every restored generated ID so
        // a new job never takes that cleanup's identity or artifact directory.
        let next_id = manifest
            .jobs
            .iter()
            .filter_map(|job| {
                job.id
                    .strip_prefix("download-")
                    .filter(|suffix| suffix.len() == 16)
                    .and_then(|suffix| u64::from_str_radix(suffix, 16).ok())
            })
            .max()
            .map_or(1, |id| id.saturating_add(1));
        let coordinator = Self {
            inner: Arc::new(DownloadInner {
                root,
                state: Mutex::new(DownloadState {
                    manifest,
                    active: None,
                    last_persist: Instant::now(),
                    resolvers: HashMap::new(),
                }),
                next_id: AtomicU64::new(next_id),
                revision: AtomicU64::new(0),
                foreground: AtomicBool::new(foreground),
            }),
        };
        registry.insert(
            coordinator.inner.root.clone(),
            Arc::downgrade(&coordinator.inner),
        );
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
        // Owner guards inspect retained provider/backend state; never hold the
        // coordinator lock while checking ownership or durable membership.
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

    /// Automatic playback requires the exact title as well as its episode ID;
    /// provider episode identifiers are not globally unique across titles.
    pub(crate) fn jobs_for_media(
        &self,
        media_type: &str,
        title_id: &str,
        request_id: &str,
    ) -> Vec<DownloadJob> {
        let mut jobs: Vec<_> = self
            .inner
            .state()
            .manifest
            .jobs
            .iter()
            .filter(|job| {
                job.phase == DownloadPhase::Completed
                    && job.media_type == media_type
                    && job.media_id == title_id
                    && job.request_id == request_id
            })
            .cloned()
            .collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
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

    pub(super) fn enqueue_with_resolver(
        &self,
        request: NewDownload,
        resolver: Option<ResolverDownload>,
    ) -> String {
        self.enqueue_owned(
            request,
            resolver,
            crate::download::offline::JobOwnership::default(),
        )
    }

    fn enqueue_owned(
        &self,
        request: NewDownload,
        resolver: Option<ResolverDownload>,
        mut ownership: crate::download::offline::JobOwnership,
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
            source_owner,
            source_revision,
        } = request;
        let intro_request = intro_skip::download_request(&media_type, &media_id, &request_id);
        let id = {
            let mut state = self.inner.state();
            if let Some(index) = state.manifest.jobs.iter().position(|job| {
                job.media_type == media_type
                    && job.media_id == media_id
                    && job.request_id == request_id
                    && job.source == source
            }) {
                if let Some(resolver) = resolver {
                    let id = state.manifest.jobs[index].id.clone();
                    state.resolvers.insert(id, resolver);
                }
                if ownership.manual_keep {
                    state.manifest.jobs[index].offline.manual_keep = true;
                    state.manifest.jobs[index].offline.manual_origin = true;
                }
                state.manifest.jobs[index]
                    .offline
                    .owners
                    .append(&mut ownership.owners);
                persist_manifest(&state.manifest);
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
            ownership.source_owner = source_owner;
            ownership.source_revision = source_revision;
            job.offline = ownership;
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

    #[cfg(target_os = "android")]
    pub(super) fn pause_background_smart(&self) {
        let mut state = self.inner.state();
        for job in &mut state.manifest.jobs {
            if job.offline.automatic()
                && matches!(
                    job.phase,
                    DownloadPhase::Queued | DownloadPhase::Resolving | DownloadPhase::Downloading
                )
            {
                job.phase = DownloadPhase::Paused;
                job.offline.pause_reason = Some(crate::download::offline::PauseReason::Background);
            }
        }
        if let Some(active) = &state.active
            && state
                .manifest
                .jobs
                .iter()
                .any(|job| job.id == active.id && job.offline.automatic())
        {
            active.cancel.cancel();
            active.torrent_cancel.store(true, Ordering::Release);
        }
        persist_manifest(&state.manifest);
        self.bump_revision();
    }

    pub(super) fn attach_smart_owner(&self, id: &str, owner: crate::download::offline::Owner) {
        let mut state = self.inner.state();
        if let Some(job) = state.manifest.jobs.iter_mut().find(|j| j.id == id)
            && job.offline.owners.insert(owner)
        {
            persist_manifest(&state.manifest);
            self.bump_revision();
        }
    }
    pub(super) fn keep_removed_policy(
        &self,
        owner: &crate::download::offline::Owner,
        keep: bool,
        playing: Option<&str>,
    ) {
        let mut state = self.inner.state();
        let remove = detach_policy_jobs(&mut state, owner, keep, playing);
        let pending = self.claim_removals(&mut state, &remove);
        persist_manifest(&state.manifest);
        self.bump_revision();
        drop(state);
        if let Some(pending) = pending {
            self.finish_removals(pending);
        }
    }

    pub(super) fn smart_admit(
        &self,
        request: NewDownload,
        resolver: Option<ResolverDownload>,
        owner: crate::download::offline::Owner,
        estimate: Option<u64>,
    ) -> Result<String, crate::download::offline::PauseReason> {
        use crate::download::offline::{JobOwnership, Ledger, PauseReason};
        let _device = smart_offline::device_guard();
        // Revalidate after slow source lookup, then serialize physical admission.
        if !smart_offline::owner_wants(
            &owner,
            &request.media_type,
            &request.media_id,
            &request.request_id,
            &request.source_owner,
        ) || !smart_offline::source_revision_matches(&owner, &request.source_revision)
        {
            return Err(PauseReason::Disabled);
        }
        let device = smart_offline::read_device().map_err(|_| PauseReason::Disabled)?;
        if let Some(reason) = device.environmental_pause(smart_offline::environment()) {
            return Err(reason);
        }
        let mut state = self.inner.state();
        refresh_physical_accounting(&mut state.manifest.jobs);
        let ledger = Ledger::from_jobs(&state.manifest.jobs);
        let estimate_cap = estimate
            .and_then(|n| n.checked_add(n / 20))
            .and_then(|n| n.checked_add(16 * 1024 * 1024));
        if let Some(job) = state.manifest.jobs.iter_mut().find(|job| {
            job.media_type == request.media_type
                && job.media_id == request.media_id
                && job.request_id == request.request_id
        }) {
            let id = job.id.clone();
            if job.phase == DownloadPhase::Failed
                && job.offline.retry_at.is_some_and(|at| at <= now_secs())
            {
                let cap = estimate_cap.ok_or(PauseReason::NeedsSource)?;
                if ledger.automatic_bytes.saturating_add(ledger.outstanding)
                    > device.allowance_bytes
                {
                    return Err(PauseReason::Budget);
                }
                if cap > job.offline.reserved_bytes {
                    let (free, capacity) = crate::download::filesystem_capacity(&self.inner.root)
                        .ok_or(PauseReason::FreeSpace)?;
                    let extra = ledger.admit(
                        Some(cap - job.offline.reserved_bytes),
                        &device,
                        free,
                        capacity,
                    )?;
                    job.offline.reserved_bytes = job.offline.reserved_bytes.saturating_add(extra);
                }
                job.offline.owners.insert(owner);
                job.source = request.source;
                job.offline.source_revision = request.source_revision;
                job.offline.source_owner = request.source_owner;
                job.offline.pause_reason = None;
                job.phase = DownloadPhase::Queued;
                if let Some(resolver) = resolver {
                    state.resolvers.insert(id.clone(), resolver);
                }
            } else {
                job.offline.owners.insert(owner);
            }
            persist_manifest(&state.manifest);
            self.bump_revision();
            drop(state);
            self.schedule_next();
            return Ok(id);
        }
        if state.manifest.jobs.iter().any(|job| {
            job.source == request.source
                && (job.media_type != request.media_type
                    || job.media_id != request.media_id
                    || job.request_id != request.request_id)
        }) {
            return Err(PauseReason::NeedsSource);
        }
        let device = smart_offline::read_device().map_err(|_| PauseReason::Disabled)?;
        if let Some(reason) = device.environmental_pause(smart_offline::environment()) {
            return Err(reason);
        }
        let (free, capacity) =
            crate::download::filesystem_capacity(&self.inner.root).ok_or(PauseReason::FreeSpace)?;
        refresh_physical_accounting(&mut state.manifest.jobs);
        let mut reserved = Ledger::from_jobs(&state.manifest.jobs).admit(
            estimate.map(|n| n.saturating_add(16 * 1024 * 1024)),
            &device,
            free,
            capacity,
        )?;
        let torrent_directory = match &request.source {
            JobSource::Torrent { info_hash, .. } => Some(crate::torrent::download_directory(
                info_hash,
                &self.inner.root.join("torrents"),
                None,
            )),
            _ => None,
        };
        if let Some(directory) = &torrent_directory {
            let previous = state
                .manifest
                .jobs
                .iter()
                .filter(|j| j.torrent_directory.as_ref() == Some(directory))
                .map(|j| j.offline.reserved_bytes)
                .max()
                .unwrap_or(0);
            reserved = reserved.saturating_add(previous);
            for sibling in &mut state.manifest.jobs {
                if sibling.torrent_directory.as_ref() == Some(directory)
                    && !sibling.offline.manual_keep
                {
                    sibling.offline.reserved_bytes = reserved;
                }
            }
        }
        let id = format!(
            "download-{:016x}",
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut job = DownloadJob::new(id.clone(), request.source);
        job.media_type = request.media_type;
        job.media_id = request.media_id;
        job.request_id = request.request_id;
        job.title = request.title;
        job.year = (!request.year.is_empty()).then_some(request.year);
        job.display = request.display;
        job.addon = request.addon;
        job.offline = JobOwnership {
            manual_keep: false,
            manual_origin: false,
            owners: std::collections::BTreeSet::from([owner]),
            reserved_bytes: reserved,
            source_owner: request.source_owner,
            source_revision: request.source_revision,
            ..Default::default()
        };
        job.torrent_directory = torrent_directory;
        job.intro_request =
            intro_skip::download_request(&job.media_type, &job.media_id, &job.request_id);
        job.file_name = Some(job_file_name(&job));
        if job.source.is_http() {
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
        drop(state);
        self.schedule_next();
        Ok(id)
    }

    fn smart_byte_admission(&self, id: &str, next: u64) -> bool {
        use crate::download::offline::{GIB, Ledger, PauseReason};
        let mut state = self.inner.state();
        let Some(index) = state.manifest.jobs.iter().position(|j| j.id == id) else {
            return false;
        };
        refresh_physical_accounting(&mut state.manifest.jobs);
        let job = &state.manifest.jobs[index];
        if job.offline.manual_keep {
            return true;
        }
        let next = next.max(job.bytes_downloaded).saturating_add(8192);
        let mut reason = smart_offline::job_pause(job);
        if next > job.offline.reserved_bytes {
            reason = Some(PauseReason::Budget);
        }
        if let (Ok(device), Some((free, capacity))) = (
            smart_offline::read_device(),
            crate::download::filesystem_capacity(&self.inner.root),
        ) {
            let ledger = Ledger::from_jobs(&state.manifest.jobs);
            if ledger
                .automatic_bytes
                .saturating_sub(job.bytes_downloaded)
                .saturating_add(next)
                > device.allowance_bytes
            {
                reason = Some(PauseReason::Budget);
            }
            if free.saturating_sub(next.saturating_sub(job.bytes_downloaded))
                < GIB.max(capacity / 20)
            {
                reason = Some(PauseReason::FreeSpace);
            }
        } else {
            reason = Some(PauseReason::FreeSpace);
        }
        if let Some(reason) = reason {
            let job = &mut state.manifest.jobs[index];
            job.phase = DownloadPhase::Paused;
            job.offline.pause_reason = Some(reason);
            persist_manifest(&state.manifest);
            self.bump_revision();
            return false;
        }
        true
    }

    pub(super) fn reconcile_smart_owners(
        &self,
        wanted: &HashSet<(crate::download::offline::Owner, String)>,
        playing: Option<&str>,
        cleanup: bool,
    ) {
        use crate::download::offline::PauseReason;
        let mut state = self.inner.state();
        let before = state.manifest.clone();
        let mut remove = HashSet::new();
        for job in &mut state.manifest.jobs {
            if job.phase == DownloadPhase::Completed
                && job
                    .artifact_path
                    .as_ref()
                    .is_none_or(|path| !path.is_file())
            {
                job.phase = DownloadPhase::Failed;
                job.error = Some("Downloaded file is missing.".into());
                if job.offline.automatic() {
                    job.offline.retry_at = Some(now_secs());
                }
            }
            let keep_removed = job
                .offline
                .owners
                .iter()
                .any(smart_offline::removed_rule_keeps_file);
            job.offline
                .owners
                .retain(|owner| wanted.contains(&(owner.clone(), job.request_id.clone())));
            if job.offline.owners.is_empty() && !job.offline.manual_keep {
                if keep_removed && job.phase == DownloadPhase::Completed {
                    job.offline.manual_keep = true;
                    job.offline.pinned = true;
                    continue;
                }
                if job.phase != DownloadPhase::Completed
                    || cleanup && playing != Some(job.id.as_str())
                {
                    remove.insert(job.id.clone());
                }
            } else if job.offline.automatic() {
                let reason = smart_offline::job_pause(job);
                if let Some(reason) = reason {
                    if !matches!(job.phase, DownloadPhase::Completed | DownloadPhase::Failed)
                        && job.offline.pause_reason != Some(PauseReason::User)
                    {
                        job.phase = DownloadPhase::Paused;
                        job.offline.pause_reason = Some(reason);
                    }
                } else if job.phase == DownloadPhase::Paused
                    && job.offline.pause_reason.as_ref().is_some_and(|reason| {
                        !matches!(
                            reason,
                            PauseReason::User
                                | PauseReason::Budget
                                | PauseReason::FreeSpace
                                | PauseReason::NeedsSource
                        )
                    })
                {
                    job.phase = DownloadPhase::Queued;
                    job.offline.pause_reason = None;
                }
            }
        }
        if let Some(active) = &state.active
            && state
                .manifest
                .jobs
                .iter()
                .any(|j| j.id == active.id && j.phase == DownloadPhase::Paused)
        {
            active.cancel.cancel();
            active.torrent_cancel.store(true, Ordering::Release);
        }
        let pending = self.claim_removals(&mut state, &remove);
        if state.manifest != before {
            persist_manifest(&state.manifest);
            self.bump_revision();
        }
        drop(state);
        if let Some(pending) = pending {
            self.finish_removals(pending);
        }
        self.schedule_next();
    }

    fn pin_completed(&self, id: &str, pinned: bool) {
        let mut state = self.inner.state();
        if let Some(job) = state
            .manifest
            .jobs
            .iter_mut()
            .find(|job| job.id == id && job.phase == DownloadPhase::Completed)
        {
            job.offline.pinned = pinned;
            job.offline.manual_keep = job.offline.manual_origin || pinned;
            persist_manifest(&state.manifest);
            self.bump_revision();
        }
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
        job.offline.pause_reason = Some(crate::download::offline::PauseReason::User);
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
            if job.offline.automatic()
                && (job.phase == DownloadPhase::Failed || is_resolver_download(job))
            {
                job.phase = DownloadPhase::Failed;
                job.offline.retry_at = Some(now_secs());
                job.offline.retry_attempt = 0;
                job.offline.pause_reason = None;
                job.error = None;
                persist_manifest(&state.manifest);
                self.bump_revision();
                return;
            }
            job.phase = DownloadPhase::Queued;
            job.offline.pause_reason = None;
            job.offline.retry_attempt = 0;
            job.offline.retry_at = None;
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
        if let Some(job) = self.job(id) {
            smart_offline::suppress_removed_job(&job);
        }
        self.remove_many(&HashSet::from([id.to_owned()]));
    }

    /// Page action 1 cancels an incomplete transfer; action 3 explicitly
    /// confirms deletion of a completed file. Check and claim atomically so
    /// a timer/worker completion cannot change what the user authorized.
    pub(super) fn remove_from_page(&self, id: &str, completed: bool) -> bool {
        let pending = {
            let mut state = self.inner.state();
            let Some(job) = state.manifest.jobs.iter().find(|job| job.id == id) else {
                return false;
            };
            if (job.phase == DownloadPhase::Completed) != completed {
                return false;
            }
            self.claim_removals(&mut state, &HashSet::from([id.to_owned()]))
        };
        let Some(pending) = pending else { return false };
        for job in &pending.jobs {
            smart_offline::suppress_removed_job(job);
        }
        self.finish_removals(pending);
        true
    }

    /// Separate the short in-memory authority claim from persistence/file IO.
    fn claim_removals(
        &self,
        state: &mut DownloadState,
        ids: &HashSet<String>,
    ) -> Option<PendingRemoval> {
        if ids.is_empty() {
            return None;
        }
        let mut removed = Vec::new();
        let jobs = std::mem::take(&mut state.manifest.jobs);
        for mut job in jobs {
            if ids.contains(&job.id) {
                job.intro_request = None;
                job.intro_timings = None;
                state.resolvers.remove(&job.id);
                removed.push(job);
            } else {
                state.manifest.jobs.push(job);
            }
        }
        if removed.is_empty() {
            return None;
        }
        let transfer_engine = state
            .active
            .as_ref()
            .filter(|active| ids.contains(&active.id))
            .and_then(|active| {
                active.remove.store(true, Ordering::Release);
                active.cancel.cancel();
                active.torrent_cancel.store(true, Ordering::Release);
                active
                    .torrent_engine
                    .clone()
                    .map(|engine| (active.id.clone(), engine))
            });
        self.bump_revision();
        Some(PendingRemoval {
            jobs: removed,
            transfer_engine,
        })
    }

    fn remove_many(&self, ids: &HashSet<String>) -> bool {
        let pending = {
            let mut state = self.inner.state();
            self.claim_removals(&mut state, ids)
        };
        let Some(pending) = pending else { return false };
        self.finish_removals(pending);
        true
    }

    fn finish_removals(&self, pending: PendingRemoval) {
        // Serialize the CURRENT manifest under its normal lock. A stale
        // captured snapshot could otherwise overwrite a newer enqueue/progress.
        self.persist();
        let PendingRemoval {
            jobs: removed,
            transfer_engine,
        } = pending;
        for removed in removed {
            if matches!(removed.source, JobSource::Torrent { .. }) {
                let remaining = self.torrent_owners();
                let engine = transfer_engine
                    .as_ref()
                    .filter(|(id, _)| *id == removed.id)
                    .map(|(_, engine)| engine.clone())
                    .or_else(crate::torrent::engine);
                let result = if let Some(engine) = engine {
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
                }
            } else if !self
                .inner
                .state()
                .active
                .as_ref()
                .is_some_and(|active| active.id == removed.id)
            {
                // A live HTTP worker owns deferred cleanup after cancellation.
                self.remove_artifacts(&removed);
            }
        }
        self.schedule_next();
    }

    /// Run only on the UI authority thread. Both locks are nonblocking; no
    /// watched/playback/profile action can interleave between check and claim,
    /// and an IO-owning coordinator worker can never stall the UI here.
    fn try_claim_watched_removals(
        &self,
        shared: &profile_session::SessionState,
        series_id: &str,
        episode_ids: &[String],
    ) -> Result<Option<PendingRemoval>, ()> {
        let mut downloads = match self.inner.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(()),
        };
        // Resolve the captured UI profile at the operation boundary, including
        // its epoch/session identity; never retain a raw old Shared allocation.
        let state = shared.try_lock().map_err(|_| ())?;
        let (episodes, playing) = watched_download_cleanup_targets(&state, series_id, episode_ids);
        let ids =
            watched_download_removal_ids(&downloads, series_id, &episodes, playing.as_deref());
        Ok(self.claim_removals(&mut downloads, &ids))
    }

    #[cfg(test)]
    fn remove_for_watched_episode(
        &self,
        series_id: &str,
        episode_id: &str,
        playing_episode: bool,
    ) -> bool {
        self.remove_for_watched_episodes(
            series_id,
            &HashSet::from([episode_id.to_owned()]),
            playing_episode.then_some(episode_id),
        )
    }

    #[cfg(test)]
    fn remove_for_watched_episodes(
        &self,
        series_id: &str,
        episode_ids: &HashSet<String>,
        playing_episode: Option<&str>,
    ) -> bool {
        let pending = {
            let mut state = self.inner.state();
            let ids = watched_download_removal_ids(&state, series_id, episode_ids, playing_episode);
            self.claim_removals(&mut state, &ids)
        };
        let Some(pending) = pending else { return false };
        self.finish_removals(pending);
        true
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
                .filter(|job| job.phase == DownloadPhase::Queued)
                .filter(|job| {
                    self.inner.foreground.load(Ordering::Acquire) || job.offline.automatic()
                })
                .filter(|job| !job.offline.automatic() || smart_offline::job_pause(job).is_none())
                .min_by_key(|job| (!job.offline.manual_keep, job.created_at))
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

        if job.offline.automatic() {
            let coordinator = self.clone();
            let id = job.id.clone();
            active.cancel.set_byte_admission(Arc::new(move |bytes| {
                coordinator.smart_byte_admission(&id, bytes)
            }));
        }
        let coordinator = self.clone();
        let job_id = job.id.clone();
        let worker_job_id = job_id.clone();
        let source = job.source.clone();
        let done = active.finished.clone();
        let name = match &source {
            JobSource::Http { .. } => "download-http",
            JobSource::Torrent { .. } => "download-torrent",
        };
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // Marks the worker ended even if the transfer panics; the tick
                // reaps a slot whose worker died before `finish` cleared it.
                let _done = WorkerDone(done);
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match source {
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
            let resolved =
                (resolver.resolve)(&resolver.token, Duration::from_secs(30), cancelled.clone());
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
        let _write_guard = job.offline.automatic().then(|| {
            let coordinator = self.clone();
            let id = job.id.clone();
            let cancel = active.cancel.clone();
            let torrent_cancel = active.torrent_cancel.clone();
            let root = crate::torrent::download_directory(
                &info_hash,
                &self.inner.root.join("torrents"),
                job.torrent_directory.as_deref(),
            );
            crate::torrent::register_write_guard(
                &info_hash,
                root,
                Arc::new(move |next| {
                    let allowed =
                        !cancel.is_cancelled() && coordinator.smart_byte_admission(&id, next);
                    if !allowed {
                        cancel.cancel();
                        torrent_cancel.store(true, Ordering::Release);
                    }
                    allowed
                }),
            )
        });
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
        job.total_bytes = (stats.total_bytes > 0).then_some(stats.total_bytes);
        job.bytes_downloaded = stats.downloaded_bytes;
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
                                if job.offline.automatic() {
                                    job.offline.retry(crate::download::unix_timestamp());
                                }
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
        smart_offline::trigger();
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
            if job.offline.automatic() {
                job.offline.retry_at = Some(now_secs());
            }
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
        if job.phase == DownloadPhase::Paused
            && let Some(reason) = job.offline.pause_reason.clone()
        {
            return text::tr(smart_offline::pause_text(reason)).into();
        }
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
        if self.shared.lock().unwrap().season_download.is_some() {
            app.set_stream_action_id(stream.id.clone().into());
            app.set_stream_action_title(text::tr("Select a stream for download").into());
            app.set_stream_action_items(
                Rc::new(VecModel::from(vec![SheetItem {
                    label: text::tr("Select stream").into(),
                    enabled: stream.download.is_none(),
                }]))
                .into(),
            );
            app.set_stream_action_open(true);
            return;
        }
        let (title, items) = match &stream.download {
            Some(job) => {
                let mut items = vec![SheetItem {
                    label: SharedString::from(DownloadCoordinator::action_label(job)),
                    enabled: true,
                }];
                if job.phase == DownloadPhase::Failed {
                    items.push(SheetItem {
                        label: text::tr("Choose another stream").into(),
                        enabled: true,
                    });
                }
                items.push(SheetItem {
                    label: SharedString::from(text::tr("Remove download")),
                    enabled: true,
                });
                if job.phase == DownloadPhase::Completed {
                    items.push(SheetItem {
                        label: text::tr(if job.offline.pinned {
                            "Unpin downloaded file"
                        } else {
                            "Keep downloaded file"
                        })
                        .into(),
                        enabled: true,
                    });
                }
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
                    StreamSource::Unsupported
                    | StreamSource::ExternalUrl(_)
                    | StreamSource::Downloaded { .. } => false,
                };
                let mut items = vec![SheetItem {
                    label: SharedString::from(text::tr(
                        if matches!(stream.source, StreamSource::ExternalUrl(_)) {
                            "Open in browser"
                        } else {
                            "Play stream"
                        },
                    )),
                    enabled: true,
                }];
                if downloadable {
                    items.push(SheetItem {
                        label: SharedString::from(text::tr("Download")),
                        enabled: true,
                    });
                    let owner = profiles::ProfileStore::capture();
                    if self
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .is_some_and(|m| smart_offline::enabled_title(&owner, &m.type_, &m.id))
                    {
                        items.push(SheetItem {
                            label: SharedString::from(text::tr(
                                "Use this source for smart downloads",
                            )),
                            enabled: true,
                        });
                    }
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
        if self.season_download_pick_row(stream.clone()) {
            return;
        }
        if let Some(job) = stream.download {
            match action {
                0 => match job.phase {
                    DownloadPhase::Completed => {
                        self.play_download(&job.id);
                    }
                    DownloadPhase::Paused | DownloadPhase::Failed => self.downloads.resume(&job.id),
                    _ => self.downloads.pause(&job.id),
                },
                1 if job.phase == DownloadPhase::Failed => {
                    self.reselect_failed_download(job.clone())
                }
                2 if job.phase == DownloadPhase::Completed => {
                    if self.require_profile_management() {
                        self.downloads.pin_completed(&job.id, !job.offline.pinned);
                    }
                }
                1 | 2 => self.downloads.remove(&job.id),
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
        } else if action == 2 {
            self.smart_source_selected(&stream);
        }
    }

    pub(super) fn download_stream(&self, stream: &StreamUi) {
        let owner = profiles::ProfileStore::capture();
        if !profile_policy::allows_addon(&owner, &stream.owner_url)
            || self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .is_none_or(|m| !profile_policy::allows_title(&owner, &m.type_, &m.id))
        {
            return;
        }
        if self.season_download_pick_row(stream.clone()) {
            return;
        }
        if matches!(
            stream.source,
            StreamSource::Unsupported | StreamSource::ExternalUrl(_)
        ) {
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
        let Some((source, resolver)) = self.prepare_download_source(stream) else {
            return;
        };
        let request = NewDownload {
            media_type,
            media_id,
            request_id,
            title,
            year,
            display: stream.display.clone(),
            addon: stream.addon.clone(),
            source_owner: stream.owner_url.clone(),
            source_revision: String::new(),
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

    pub(super) fn prepare_download_source(
        &self,
        stream: &StreamUi,
    ) -> Option<(JobSource, Option<ResolverDownload>)> {
        let resolver = match &stream.source {
            StreamSource::Resolver {
                provider_url,
                token,
                settings_generation,
                addon_generation,
            } => {
                let valid_selection = self.shared.lock().unwrap().installed.iter().any(|addon| {
                    addon.url == *provider_url
                        && addon.enabled
                        && addon.available
                        && addon.generation == *addon_generation
                });
                let capability = valid_selection
                    .then(|| {
                        nova_providers::capture_resolver_capability(
                            provider_url,
                            *settings_generation,
                        )
                    })
                    .transpose();
                let Ok(Some(capability)) = capability else {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This stream selection is out of date. Reload streams.",
                    )));
                    return None;
                };
                let authority = capability.clone();
                Some(ResolverDownload {
                    token: token.clone(),
                    is_current: Arc::new(move || authority.is_current()),
                    resolve: Arc::new(move |token, timeout, cancelled| {
                        capability.resolve(token, timeout, cancelled)
                    }),
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
                    return None;
                };
                source
            }
            StreamSource::Unsupported | StreamSource::ExternalUrl(_) => return None,
            StreamSource::Downloaded { .. } => return None,
        };
        Some((source, resolver))
    }

    pub(super) fn enqueue_season_download(
        &self,
        stream: &StreamUi,
        identity: (&str, &str, &str, &str, &str),
        prepared: (JobSource, Option<ResolverDownload>),
    ) -> String {
        let (media_type, media_id, request_id, title, year) = identity;
        self.downloads.enqueue_with_resolver(
            NewDownload {
                media_type: media_type.into(),
                media_id: media_id.into(),
                request_id: request_id.into(),
                title: title.into(),
                year: year.into(),
                display: stream.display.clone(),
                addon: stream.addon.clone(),
                source_owner: stream.owner_url.clone(),
                source_revision: String::new(),
                source: prepared.0,
            },
            prepared.1,
        )
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
        let rows = completed_episode_download_rows(
            self.downloads.completed_episode_jobs(),
            read_episodes_cache_for,
        );
        if let Some(app) = self.app() {
            app.set_downloads_rows(Rc::new(VecModel::from(rows)).into());
        }
    }

    /// Remove one downloaded episode (file + manifest entry) from the list.
    pub(super) fn remove_download(&self, id: &str) {
        if !profile_policy::management_allowed(&profiles::ProfileStore::capture()) {
            return;
        }
        self.downloads.remove(id);
        self.apply_stream_filter();
        self.downloads_list_to_ui();
    }

    pub(super) fn set_download_auto_delete_watched(&self, enabled: bool) {
        if !profile_policy::management_allowed(&profiles::ProfileStore::capture()) {
            return;
        }
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
        if episode_ids.is_empty()
            || !self
                .shared
                .lock()
                .unwrap()
                .download_settings
                .auto_delete_watched
        {
            return;
        }
        let bridge = self.clone();
        let owner = profiles::capture();
        let series_id = series_id.to_owned();
        let episode_ids = episode_ids.to_vec();
        let _ = profiles::invoke_from_event_loop(move || {
            bridge.claim_watched_download_cleanup(owner, series_id, episode_ids);
        });
    }

    fn claim_watched_download_cleanup(
        &self,
        owner: profiles::Context,
        series_id: String,
        episode_ids: Vec<String>,
    ) {
        if !profiles::is_current(&owner) {
            return;
        }
        match self
            .downloads
            .try_claim_watched_removals(&self.shared, &series_id, &episode_ids)
        {
            Err(()) => {
                let bridge = self.clone();
                slint::Timer::single_shot(Duration::from_millis(10), move || {
                    if profiles::is_current(&owner) {
                        profiles::with_context(owner.clone(), || {
                            bridge.claim_watched_download_cleanup(owner, series_id, episode_ids)
                        });
                    }
                });
            }
            Ok(None) => {}
            Ok(Some(pending)) => {
                let bridge = self.clone();
                profiles::thread::Builder::new()
                    .name("watched-download-cleanup".into())
                    .spawn(move || {
                        // The deletion was authorized atomically on UI; no Shared
                        // lock is held while manifest/torrent/filesystem IO runs.
                        bridge.downloads.finish_removals(pending);
                        let _ =
                            profiles::invoke_from_event_loop(move || bridge.apply_stream_filter());
                    })
                    .expect("spawn watched download cleanup");
            }
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

// Keep title metadata transient and typed. Hundreds of downloaded episodes
// from one show share one cache read/parse per publication; missing metadata is
// memoized too, and the next publication observes refreshed cache contents.
fn completed_episode_download_rows(
    jobs: Vec<DownloadJob>,
    mut read_episodes: impl FnMut(&str, &str) -> Option<Vec<Video>>,
) -> Vec<DownloadRow> {
    let mut labels: HashMap<(String, String), HashMap<String, String>> = HashMap::new();
    jobs.into_iter()
        .map(|job| {
            // Prefer the real episode label from the cached episode list;
            // fall back to parsing the request id's season/episode tail.
            let title_labels = labels
                .entry((job.media_type.clone(), job.media_id.clone()))
                .or_insert_with(|| {
                    let mut by_id = HashMap::new();
                    for video in read_episodes(&job.media_type, &job.media_id).unwrap_or_default() {
                        // Match the previous first-matching-video behavior.
                        by_id
                            .entry(video.id.clone())
                            .or_insert_with(|| episode_context_label(&video));
                    }
                    by_id
                });
            let label = title_labels
                .get(&job.request_id)
                .cloned()
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
        .collect()
}

fn watched_download_removal_ids(
    state: &DownloadState,
    series_id: &str,
    episode_ids: &HashSet<String>,
    playing_episode: Option<&str>,
) -> HashSet<String> {
    state
        .manifest
        .jobs
        .iter()
        .filter(|job| job.media_id == series_id && episode_ids.contains(&job.request_id))
        .filter(|job| job.offline.owners.is_empty() && !job.offline.pinned)
        // HLS opens later segments throughout playback, so its playing bundle
        // must survive until player-close cleanup authorizes another claim.
        .filter(|job| {
            playing_episode != Some(job.request_id.as_str())
                || !job.artifact_path.as_ref().is_some_and(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "m3u8")
                })
        })
        .map(|job| job.id.clone())
        .collect()
}

fn watched_download_cleanup_targets(
    state: &Shared,
    series_id: &str,
    episode_ids: &[String],
) -> (HashSet<String>, Option<String>) {
    if !state.download_settings.auto_delete_watched {
        return (HashSet::new(), None);
    }
    let episodes = episode_ids
        .iter()
        .filter(|episode_id| {
            state
                .progress
                .get(&progress_map_key(series_id, episode_id))
                .is_some_and(|progress| progress.watched)
        })
        .cloned()
        .collect();
    let playing = state
        .playback
        .as_ref()
        .filter(|target| target.series_id == series_id)
        .map(|target| target.episode_id.clone());
    (episodes, playing)
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
    fn removing_rule_keeps_only_completed_files_and_never_promotes_live_auto_work() {
        use crate::download::offline::Owner;
        let owner = Owner {
            profile: "alice".into(),
            policy: "show".into(),
        };
        let sibling = Owner {
            profile: "bob".into(),
            policy: "show".into(),
        };
        for phase in [
            DownloadPhase::Queued,
            DownloadPhase::Resolving,
            DownloadPhase::Downloading,
        ] {
            let coordinator = DownloadCoordinator {
                inner: Arc::new(DownloadInner {
                    root: PathBuf::new(),
                    state: Mutex::new(DownloadState {
                        manifest: DownloadManifest::default(),
                        active: None,
                        last_persist: Instant::now(),
                        resolvers: HashMap::new(),
                    }),
                    next_id: AtomicU64::new(1),
                    revision: AtomicU64::new(0),
                    foreground: AtomicBool::new(true),
                }),
            };
            install_active(&coordinator, "auto", phase, false);
            {
                let mut state = coordinator.inner.state();
                let job = &mut state.manifest.jobs[0];
                job.offline.manual_keep = false;
                job.offline.owners.insert(owner.clone());
            }
            let token = coordinator
                .inner
                .state()
                .active
                .as_ref()
                .unwrap()
                .cancel
                .clone();
            let mut state = coordinator.inner.state();
            let mut completed = DownloadJob::default();
            completed.id = "ready".into();
            completed.phase = DownloadPhase::Completed;
            completed.offline.manual_keep = false;
            completed.offline.owners.insert(owner.clone());
            state.manifest.jobs.push(completed);
            let mut shared = DownloadJob::default();
            shared.id = "shared".into();
            shared.offline.manual_keep = false;
            shared
                .offline
                .owners
                .extend([owner.clone(), sibling.clone()]);
            state.manifest.jobs.push(shared);
            let ids = detach_policy_jobs(&mut state, &owner, true, None);
            assert_eq!(ids, HashSet::from(["auto".into()]));
            assert!(!state.manifest.jobs[0].offline.manual_keep);
            assert!(
                state
                    .manifest
                    .jobs
                    .iter()
                    .find(|job| job.id == "ready")
                    .unwrap()
                    .offline
                    .manual_keep
            );
            assert_eq!(
                state
                    .manifest
                    .jobs
                    .iter()
                    .find(|job| job.id == "shared")
                    .unwrap()
                    .offline
                    .owners,
                std::collections::BTreeSet::from([sibling.clone()])
            );
            let _pending = coordinator.claim_removals(&mut state, &ids).unwrap();
            drop(state);
            assert!(token.is_cancelled());
            assert!(
                !coordinator.smart_byte_admission("auto", 1),
                "a removed automatic worker cannot bypass policy as manual"
            );
        }
    }

    #[test]
    fn downloaded_episode_labels_read_each_typed_title_once_per_publication() {
        let jobs = (0..500)
            .map(|index| {
                let mut job = DownloadJob::new(
                    format!("label-{index}"),
                    JobSource::http("https://example.test/video"),
                );
                job.media_id = if index < 498 { "same-id" } else { "missing" }.into();
                job.media_type = if index == 497 { "anime" } else { "series" }.into();
                job.request_id = "episode".into();
                job
            })
            .collect::<Vec<_>>();
        let mut calls = HashMap::new();
        let rows = completed_episode_download_rows(jobs.clone(), |kind, id| {
            *calls.entry((kind.to_owned(), id.to_owned())).or_insert(0) += 1;
            (id != "missing").then(|| {
                vec![Video {
                    id: "episode".into(),
                    name: format!("{kind} cached title"),
                    ..Default::default()
                }]
            })
        });
        assert_eq!(calls.len(), 3);
        assert!(calls.values().all(|count| *count == 1));
        assert_eq!(rows[0].subtitle.as_str(), "series cached title");
        assert_eq!(rows[497].subtitle.as_str(), "anime cached title");
        let refreshed = completed_episode_download_rows(jobs, |_, _| {
            Some(vec![Video {
                id: "episode".into(),
                name: "New metadata".into(),
                ..Default::default()
            }])
        });
        assert_eq!(
            refreshed[0].subtitle.as_str(),
            "New metadata",
            "memoization must end with each publication"
        );
    }

    #[test]
    fn delayed_cleanup_cannot_reuse_a_restored_download_identity() {
        const ROOT: &str = "NOVA_REMOVAL_IDENTITY_TEST_ROOT";
        let Some(data_root) = std::env::var_os(ROOT) else {
            let root = temp_root("removal-identity");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args([
                    "--exact",
                    "app::downloads::tests::delayed_cleanup_cannot_reuse_a_restored_download_identity",
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
        let data_root = PathBuf::from(data_root);
        crate::storage::init_at(&data_root.join("store"));
        let root = data_root.join("downloads");
        let id = "download-0000000000000001";
        let path = root.join("http").join(id).join("old.mkv");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"old completed episode").unwrap();
        let mut restored = DownloadJob::new(id, JobSource::http("https://example.test/old"));
        restored.phase = DownloadPhase::Completed;
        restored.artifact_path = Some(path.clone());
        crate::storage::try_set_str(
            DOWNLOADS_KEY,
            &DownloadManifest::new(vec![restored]).to_json().unwrap(),
        )
        .unwrap();
        let coordinator = DownloadCoordinator::new(root);
        let pending = {
            let mut state = coordinator.inner.state();
            coordinator
                .claim_removals(&mut state, &HashSet::from([id.to_owned()]))
                .unwrap()
        };
        assert!(coordinator.job(id).is_none());
        assert!(path.is_file(), "claim leaves filesystem cleanup pending");

        // Occupy the worker slot so this real enqueue stays entirely local.
        install_active(&coordinator, "blocking", DownloadPhase::Downloading, false);
        let replacement_id = coordinator.enqueue(NewDownload {
            source_owner: String::new(),
            source_revision: String::new(),
            media_type: "series".into(),
            media_id: "replacement".into(),
            request_id: "replacement-episode".into(),
            title: "Replacement".into(),
            year: String::new(),
            display: String::new(),
            addon: String::new(),
            source: JobSource::http("https://example.test/new"),
        });
        assert_ne!(replacement_id, id, "pending cleanup still owns the old ID");
        let replacement_path = coordinator
            .job(&replacement_id)
            .unwrap()
            .artifact_path
            .unwrap();
        fs::create_dir_all(replacement_path.parent().unwrap()).unwrap();
        fs::write(&replacement_path, b"new completed episode").unwrap();
        coordinator
            .inner
            .state()
            .manifest
            .jobs
            .iter_mut()
            .find(|job| job.id == replacement_id)
            .unwrap()
            .phase = DownloadPhase::Completed;

        // Old cleanup now runs after the replacement owns completed bytes.
        coordinator.finish_removals(pending);
        assert!(!path.exists());
        assert_eq!(
            fs::read(&replacement_path).unwrap(),
            b"new completed episode"
        );
        assert_eq!(
            coordinator.job(&replacement_id).unwrap().phase,
            DownloadPhase::Completed
        );
        let persisted = DownloadManifest::from_json(
            &crate::storage::try_get_str(DOWNLOADS_KEY).unwrap().unwrap(),
        )
        .unwrap();
        assert!(persisted.jobs.iter().any(|job| job.id == replacement_id));
        assert!(!persisted.jobs.iter().any(|job| job.id == id));
    }

    #[test]
    fn queued_watched_cleanup_uses_latest_progress_setting_and_playing_target() {
        let mut state = Shared::default();
        state.download_settings.auto_delete_watched = true;
        for episode in ["a", "b"] {
            Bridge::set_episode_watched_locked(&mut state.progress, "series", episode, true);
        }
        let requested = vec!["a".into(), "b".into(), "unknown".into()];
        // The user reversed one action while cleanup waited for its worker.
        Bridge::set_episode_watched_locked(&mut state.progress, "series", "a", false);
        state.playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "b".into(),
            ..Default::default()
        });
        let (episodes, playing) = watched_download_cleanup_targets(&state, "series", &requested);
        assert_eq!(episodes, HashSet::from(["b".into()]));
        assert_eq!(playing.as_deref(), Some("b"));
        state.download_settings.auto_delete_watched = false;
        assert!(
            watched_download_cleanup_targets(&state, "series", &requested)
                .0
                .is_empty()
        );
    }

    #[test]
    fn watched_cleanup_claim_retries_nonblocking_and_observes_new_playback_or_unwatch() {
        const ROOT: &str = "NOVA_ATOMIC_WATCHED_CLAIM_ROOT";
        let Some(data_root) = std::env::var_os(ROOT) else {
            let root = temp_root("atomic-watched-claim-storage");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::downloads::tests::watched_cleanup_claim_retries_nonblocking_and_observes_new_playback_or_unwatch", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let data_root = PathBuf::from(data_root);
        storage::init_at(&data_root);
        let root = data_root.join("downloads");
        let coordinator = DownloadCoordinator::new(root.clone());
        let path = root.join("http/hls/episode.m3u8");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"playlist").unwrap();
        let mut job = DownloadJob::new("hls", JobSource::http("https://example.test/video"));
        job.media_id = "series".into();
        job.request_id = "episode".into();
        job.phase = DownloadPhase::Completed;
        job.artifact_path = Some(path.clone());
        coordinator.inner.state().manifest.jobs.push(job);
        let mut state = Shared::default();
        state.download_settings.auto_delete_watched = true;
        Bridge::set_episode_watched_locked(&mut state.progress, "series", "episode", true);
        let shared = profile_session::SessionState(profile_session::SessionDirectory::new());
        *shared.lock().unwrap() = state;
        let requested = vec!["episode".into()];
        // The IO-owning coordinator is busy: no Shared wait and no claim.
        let busy = coordinator.inner.state();
        assert!(
            coordinator
                .try_claim_watched_removals(&shared, "series", &requested)
                .is_err()
        );
        // Playback began before a retried claim; the playlist must survive.
        shared.lock().unwrap().playback = Some(PlaybackTarget {
            series_id: "series".into(),
            episode_id: "episode".into(),
            ..Default::default()
        });
        drop(busy);
        let busy_profile = shared.lock().unwrap();
        assert!(
            coordinator
                .try_claim_watched_removals(&shared, "series", &requested)
                .is_err(),
            "a busy profile must also retry without waiting"
        );
        drop(busy_profile);
        assert!(
            coordinator
                .try_claim_watched_removals(&shared, "series", &requested)
                .unwrap()
                .is_none()
        );
        assert!(coordinator.job("hls").is_some());
        assert!(path.is_file());
        {
            let mut state = shared.lock().unwrap();
            state.playback = None;
            Bridge::set_episode_watched_locked(&mut state.progress, "series", "episode", false);
        }
        assert!(
            coordinator
                .try_claim_watched_removals(&shared, "series", &requested)
                .unwrap()
                .is_none()
        );
        Bridge::set_episode_watched_locked(
            &mut shared.lock().unwrap().progress,
            "series",
            "episode",
            true,
        );
        let pending = coordinator
            .try_claim_watched_removals(&shared, "series", &requested)
            .unwrap()
            .unwrap();
        assert!(
            coordinator.job("hls").is_none(),
            "claim removes playback authority atomically"
        );
        assert!(path.is_file(), "claim performs no file cleanup");
        assert!(shared.try_lock().is_ok());
        // Execution must persist the current manifest, including newer jobs.
        let mut later = DownloadJob::new("later", JobSource::http("https://example.test/later"));
        later.phase = DownloadPhase::Completed;
        coordinator.inner.state().manifest.jobs.push(later);
        coordinator.finish_removals(pending);
        assert!(!path.exists());
        let persisted =
            DownloadManifest::from_json(&crate::storage::get_str(DOWNLOADS_KEY).unwrap()).unwrap();
        assert_eq!(
            persisted
                .jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            ["later"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bulk_watched_cleanup_batches_manifest_and_preserves_playing_hls_and_other_titles() {
        let root = temp_root("bulk-watched-cleanup");
        let coordinator = DownloadCoordinator::new(root.clone());
        for (id, episode, series, extension) in [
            ("a", "a", "series", "mkv"),
            ("b", "b", "series", "mkv"),
            ("c", "c", "series", "m3u8"),
            ("other", "a", "other-series", "mkv"),
        ] {
            let path = root
                .join("http")
                .join(id)
                .join(format!("episode.{extension}"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"episode artifact").unwrap();
            let mut job = DownloadJob::new(id, JobSource::http("https://example.test/video"));
            job.media_id = series.into();
            job.request_id = episode.into();
            job.phase = DownloadPhase::Completed;
            job.artifact_path = Some(path);
            coordinator.inner.state().manifest.jobs.push(job);
        }
        let revision = coordinator.revision();
        assert!(coordinator.remove_for_watched_episodes(
            "series",
            &HashSet::from(["a".into(), "b".into(), "c".into()]),
            Some("c")
        ));
        assert_eq!(
            coordinator.revision(),
            revision + 1,
            "one bulk manifest mutation"
        );
        assert_eq!(
            coordinator
                .jobs()
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "other"]
        );
        assert!(!root.join("http/a").exists());
        assert!(!root.join("http/b").exists());
        assert!(root.join("http/c/episode.m3u8").is_file());
        assert!(root.join("http/other/episode.mkv").is_file());
        let _ = fs::remove_dir_all(root);
    }

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
                prompt_only: false,
                confidence_percent: None,
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
    fn page_removal_requires_confirmation_for_current_completion_state() {
        let coordinator = DownloadCoordinator::new(temp_root("page-removal-confirmation"));
        let path = coordinator.inner.root.join("http/completed/episode.mkv");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"offline file").unwrap();
        coordinator.inner.state().manifest.jobs = vec![
            DownloadJob {
                id: "completed".into(),
                phase: DownloadPhase::Completed,
                artifact_path: Some(path.clone()),
                ..Default::default()
            },
            DownloadJob {
                id: "transfer".into(),
                phase: DownloadPhase::Paused,
                ..Default::default()
            },
        ];
        // A stale cancel callback after worker completion cannot delete files.
        assert!(!coordinator.remove_from_page("completed", false));
        assert!(path.exists());
        assert!(!coordinator.remove_from_page("transfer", true));
        assert!(!coordinator.remove_from_page("missing", true));
        assert!(coordinator.remove_from_page("completed", true));
        assert!(!path.exists());
        assert!(coordinator.job("completed").is_none());
        assert!(coordinator.job("transfer").is_some());
        assert!(!coordinator.remove_from_page("completed", true));
        assert!(coordinator.remove_from_page("transfer", false));
        fs::remove_dir_all(&coordinator.inner.root).unwrap();
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
        job.total_bytes = None;
        job.resource_progress = Some((3, 12));
        assert!(DownloadCoordinator::status_text(&job).contains("25%"));
        assert_eq!(DownloadCoordinator::progress_fraction(&job), 0.25);
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
                    token: "runtime-only-private-token".into(),
                    resolve: Arc::new(|_, _, _| {
                        panic!("owner invalidation test must not start resolver transport")
                    }),
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
    fn automatic_download_matching_requires_completed_exact_typed_title() {
        let root = temp_root("typed-media");
        let coordinator = DownloadCoordinator::new(root.clone());
        let mut wanted = DownloadJob::new("wanted", JobSource::http("https://example.test/wanted"));
        wanted.phase = DownloadPhase::Completed;
        wanted.media_type = "series".into();
        wanted.media_id = "title-a".into();
        wanted.request_id = "episode-1".into();
        let mut other_title = wanted.clone();
        other_title.id = "other-title".into();
        other_title.media_id = "title-b".into();
        let mut other_type = wanted.clone();
        other_type.id = "other-type".into();
        other_type.media_type = "anime".into();
        let mut pending = wanted.clone();
        pending.id = "pending".into();
        pending.phase = DownloadPhase::Downloading;
        coordinator.inner.state().manifest.jobs = vec![other_title, other_type, pending, wanted];
        let jobs = coordinator.jobs_for_media("series", "title-a", "episode-1");
        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            vec!["wanted"]
        );
        assert!(
            coordinator
                .jobs_for_media("series", "title-a", "episode-2")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
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
    fn torrent_progress_populates_unknown_total_before_completion() {
        let coordinator = DownloadCoordinator::new(temp_root("torrent-byte-progress"));
        install_active(&coordinator, "torrent", DownloadPhase::Downloading, false);
        assert_eq!(coordinator.job("torrent").unwrap().total_bytes, None);
        coordinator.update_torrent_progress(
            "torrent",
            TorrentStats {
                downloaded_bytes: 25,
                total_bytes: 100,
                progress: 0.25,
                ..Default::default()
            },
        );
        let job = coordinator.job("torrent").unwrap();
        assert_eq!(job.total_bytes, Some(100));
        assert_eq!(job.bytes_downloaded, 25);
        assert_eq!(DownloadCoordinator::progress_fraction(&job), 0.25);
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
        let _ = crate::app::profiles::thread::spawn(move || {
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

//! Embedded BitTorrent streaming for torrent stream rows.
//!
//! Stremio addons (Torrentio and friends) return torrent streams as an
//! `infoHash` (+ optional `fileIdx`) instead of a direct URL. This module runs
//! one [`librqbit::Session`] inside the app and turns such a row into a
//! loopback HTTP URL that the existing mpv player can load unchanged:
//!
//! 1. [`TorrentEngine::resolve`] adds the torrent list-only to enumerate its
//!    files (seeding the magnet with [`PUBLIC_TRACKERS`] so metadata does not
//!    depend on DHT alone), picks the video file ([`pick_video_file`]), then
//!    re-adds from the resolved metainfo bytes with just that file selected.
//! 2. librqbit's HTTP API (bound to `127.0.0.1:<ephemeral>`) serves
//!    `/torrents/{id}/stream/{file_idx}` with HTTP Range support, blocking on
//!    missing pieces. That URL goes straight to `Player::play`.
//!
//! One engine lives for the whole process (like the mpv [`Player`]). It owns
//! its tokio runtime (not just a `Handle`, which would leave the runtime shut
//! down); the public API stays fire-and-continue, mirroring `crate::net` —
//! callers pass a continuation and hop back to the UI thread with
//! `slint::invoke_from_event_loop`.
//!
//! Files are kept between plays for instant replay and trimmed least-recently
//! used once the configured cache cap is exceeded; when the user enables
//! "no caching", a torrent's data is deleted as soon as its playback stops.
//!

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use librqbit::api::TorrentIdOrHash;
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use serde::{Deserialize, Serialize};

/// librqbit's torrent id type (`librqbit::session::TorrentId`, re-exported
/// here for readability).
type TorrentId = usize;

use nova_config::TorrentSettings;

/// Extensions treated as playable video when choosing the file inside a
/// torrent (lower-case, without the dot).
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "webm", "m4v", "mov", "ts", "m2ts", "flv", "wmv",
];

/// How long [`TorrentEngine::resolve`] waits for magnet metadata before
/// giving up. DHT alone can be slow (or blocked), so the session also seeds
/// every torrent with [`PUBLIC_TRACKERS`].
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// Public trackers added to every torrent. Addon stream rows usually carry a
/// bare `infoHash` with no `tr=` parameters, so without these metadata has to
/// come from DHT alone — slow and often blocked on mobile networks.
const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.tracker.cl:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    // HTTPS trackers matter on networks that drop UDP.
    "https://tracker.tamersunion.org:443/announce",
    "https://tracker.gbitt.info:443/announce",
    "https://tracker1.bt.moack.co.kr:443/announce",
];

/// A torrent ready to stream: the loopback URL to hand to the player plus a
/// human-readable name for the status line.
#[derive(Clone, Debug)]
pub struct StreamReady {
    pub url: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentDownloadReady {
    pub path: PathBuf,
    pub file_name: String,
    pub file_len: u64,
}

/// Inputs for one retained torrent download. Keeping request state together
/// makes cancellation and output ownership explicit at the subsystem boundary.
pub struct TorrentDownloadRequest {
    pub owner_id: String,
    pub info_hash: String,
    pub file_idx: Option<u32>,
    pub requested_name: String,
    pub output_directory: PathBuf,
    pub cancel: Arc<AtomicBool>,
}

/// Identity shared by the UI intent and its engine work. Cancellation is
/// immediate; cleanup is serialized with activation on the worker side.
#[derive(Clone, Debug, Default)]
pub struct PlaybackRequest(Arc<AtomicBool>);

impl PlaybackRequest {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_current(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }
    pub fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Manifest ownership restored before any cache sweep or new transfer.
#[derive(Clone, Debug)]
pub struct RetainedDownload {
    pub owner_id: String,
    pub info_hash: String,
    pub requested_file_idx: Option<u32>,
    pub directory: PathBuf,
    pub file_idx: Option<usize>,
    pub complete: bool,
}

#[derive(Clone, Debug)]
pub struct DownloadLocation {
    pub directory: PathBuf,
    pub file_idx: usize,
}

type DownloadCompletion = Box<dyn FnOnce(Result<TorrentDownloadReady, String>) + Send + 'static>;

/// Live torrent stats for the player overlay.
#[derive(Clone, Copy, Debug, Default)]
pub struct TorrentStats {
    /// Download speed in bytes/second.
    pub down_bps: u64,
    /// Connected (`live`) peer count.
    pub peers: usize,
    /// Completion fraction 0..=1 over the selected files.
    pub progress: f32,
    /// Exact byte counts for the selected file (or selected torrent files).
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// One torrent this session is (or was) streaming.
struct Entry {
    /// librqbit session id, or [`UNMANAGED`] for entries adopted from a
    /// previous process (the new session never managed them).
    torrent_id: TorrentId,
    file_idx: usize,
    /// Folder the torrent writes into (used for cache accounting/deletion).
    dir: PathBuf,
    /// Selected file size in bytes.
    file_len: u64,
    last_used: Instant,
    /// True while the player is presenting this torrent (never LRU-evicted).
    playing: bool,
    owner: Option<PlaybackRequest>,
}

#[derive(Clone)]
struct RetainedEntry {
    info_hash: String,
    torrent_id: TorrentId,
    dir: PathBuf,
    file_idx: Option<usize>,
    requested_file_idx: Option<u32>,
    file_len: Option<u64>,
    job_id: u64,
    cancel: Arc<AtomicBool>,
    complete: bool,
    worker_active: bool,
}

impl RetainedEntry {
    fn restart(&mut self, job_id: u64, file_idx: Option<u32>, cancel: Arc<AtomicBool>) -> bool {
        if self.worker_active || self.complete || file_idx != self.requested_file_idx {
            return false;
        }
        // Retention protects the partial files independently of the worker.
        // A retry takes a fresh generation; a cancelled worker is never reused.
        self.job_id = job_id;
        self.cancel = cancel;
        self.worker_active = true;
        true
    }

    fn worker_finished(&mut self, job_id: u64) {
        if self.job_id == job_id {
            self.worker_active = false;
        }
    }
}

struct RetainedWorker {
    inner: Arc<EngineInner>,
    key: String,
    job_id: u64,
}

impl Drop for RetainedWorker {
    fn drop(&mut self) {
        if let Some(entry) = self.inner.retained.lock().unwrap().get_mut(&self.key) {
            entry.worker_finished(self.job_id);
        }
    }
}

struct SelectedTorrent {
    candidate: FileCandidate,
    torrent_bytes: Option<Vec<u8>>,
}

/// Session id for adopted entries: never passed to librqbit (lookups miss),
/// only used for dir-based accounting and deletion.
const UNMANAGED: TorrentId = usize::MAX;

/// The process-wide torrent session.
pub struct TorrentEngine {
    /// `None` when the session could not be created (no network stack, bad
    /// output folder): every resolve then fails fast and the UI degrades.
    inner: Option<Arc<EngineInner>>,
}

struct EngineInner {
    session: Arc<Session>,
    /// Serialize handle selection, activation, removal and cache cleanup.
    operations: Mutex<()>,
    playback_requests: Mutex<Vec<std::sync::Weak<AtomicBool>>>,
    /// Loopback port librqbit's HTTP API listens on.
    port: u16,
    active: Mutex<HashMap<String, Entry>>,
    retained: Mutex<HashMap<String, RetainedEntry>>,
    next_download_id: AtomicU64,
    /// Session output dir (torrents live in per-infohash subdirs of it).
    dir: PathBuf,
    /// Sweep root-level files too (only when `dir` is the app-private
    /// default cache dir, never a user-chosen folder).
    sweep_root_files: bool,
    /// Runtime driving librqbit. Owned (not just its `Handle`) so it stays
    /// alive for the engine's lifetime — dropping it would shut the runtime
    /// down and make every later `block_on` panic.
    rt: tokio::runtime::Runtime,
}

impl TorrentEngine {
    /// Start the session on a dedicated tokio runtime. Never fails: a broken
    /// setup yields an inert engine whose resolves report an error, so the
    /// catalog keeps working.
    pub fn setup(dir: PathBuf, settings: &TorrentSettings) -> Self {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("nova torrent: runtime: {e}");
                return Self { inner: None };
            }
        };
        // Enter the runtime context through a handle so the future can
        // `tokio::spawn`; the owned `rt` is moved into the engine afterwards.
        let handle = rt.handle().clone();
        let built = handle.block_on(async {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("nova torrent: cannot create {}: {e}", dir.display());
                return None;
            }

            // librqbit's default DHT persistence derives its file path from
            // `directories::ProjectDirs`, which needs `$HOME`. Android app
            // processes have no `HOME` (and `dirs-sys` has no Android
            // fallback), so `Session::new_with_opts` fails with "cannot
            // determine project directory for com.rqbit.dht" and the engine
            // never starts. Point the DHT state at our own writable folder
            // instead, which also keeps it out of librqbit's OS-default cache.
            let dht_file = dir.join("dht.json");

            // Session options for one listener flavor. A listener with uTP
            // enabled is what unlocks *outgoing* uTP too: librqbit only builds
            // its uTP socket when a listener requests it, and without it the
            // connector can only try TCP — which many networks filter on peer
            // ports, leaving metadata unresolvable. The listener also lets the
            // session announce a reachable port to DHT/trackers.
            let make_opts = |ipv4_only: bool| SessionOptions {
                client_name_and_version: Some("nova/0.1".to_string()),
                persistence: None,
                dht: Some(librqbit::DhtSessionConfig {
                    persistence: Some(librqbit::dht::DhtPersistenceConfig {
                        config_filename: Some(dht_file.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ipv4_only,
                ratelimits: librqbit::limits::LimitsConfig {
                    download_bps: NonZeroU32::new(settings.down_limit_kbps.saturating_mul(1024)),
                    upload_bps: None,
                },
                listen: Some(librqbit::ListenerOptions {
                    mode: librqbit::ListenerMode::TcpAndUtp,
                    ipv4_only,
                    ..Default::default()
                }),
                ..Default::default()
            };

            // Prefer dual-stack; fall back to IPv4-only when the host cannot
            // bind `[::]` (or has IPv6 disabled), so the engine still starts.
            let session = match Session::new_with_opts(dir.clone(), make_opts(false)).await {
                Ok(session) => Some(session),
                Err(e) => {
                    eprintln!("nova torrent: dual-stack session failed: {e:#}; retrying IPv4-only");
                    match Session::new_with_opts(dir.clone(), make_opts(true)).await {
                        Ok(session) => Some(session),
                        Err(e) => {
                            eprintln!("nova torrent: cannot create session: {e:#}");
                            None
                        }
                    }
                }
            };

            if let Some(session) = session {
                // Bind loopback on an ephemeral port; the returned port is
                // what the stream URL embeds.
                let bind_addr: std::net::SocketAddr =
                    "127.0.0.1:0".parse().expect("valid loopback addr");
                match librqbit_dualstack_sockets::TcpListener::bind_tcp(
                    bind_addr,
                    librqbit_dualstack_sockets::BindOpts {
                        request_dualstack: false,
                        ..Default::default()
                    },
                ) {
                    Ok(listener) => {
                        let port = listener.bind_addr().port();
                        let api = librqbit::Api::new(session.clone(), None, None);
                        let http = librqbit::http_api::HttpApi::new(api, None);
                        tokio::spawn(http.make_http_api_and_run(listener, None));
                        return Some((session, port));
                    }
                    Err(e) => {
                        eprintln!("nova torrent: cannot bind loopback API: {e}");
                    }
                }
            }
            None
        });

        match built {
            Some((session, port)) => {
                eprintln!("nova torrent: session ready (loopback API on port {port})");
                let inner = Arc::new(EngineInner {
                    session,
                    operations: Mutex::new(()),
                    playback_requests: Mutex::new(Vec::new()),
                    port,
                    active: Mutex::new(HashMap::new()),
                    retained: Mutex::new(HashMap::new()),
                    next_download_id: AtomicU64::new(1),
                    dir: dir.clone(),
                    // Root-level files are only ours to sweep in the
                    // app-private default dir; a custom folder may hold the
                    // user's own files.
                    sweep_root_files: settings.dir.trim().is_empty(),
                    rt,
                });
                // Adopt last run's downloads (durable tracked set) and sweep
                // whatever it doesn't own, so restarts never leak cache that
                // trimming and clearing can no longer see.
                inner.adopt_persisted();
                // Download owners must be restored before sweeping (see restore_downloads).
                Self { inner: Some(inner) }
            }
            None => Self { inner: None },
        }
    }

    fn cancel_all(&self) {
        if let Some(inner) = &self.inner {
            for request in inner
                .playback_requests
                .lock()
                .unwrap()
                .iter()
                .filter_map(std::sync::Weak::upgrade)
            {
                request.store(true, Ordering::Release);
            }
            for entry in inner.retained.lock().unwrap().values() {
                entry.cancel.store(true, Ordering::Release);
            }
        }
    }

    /// Whether the engine is usable.
    pub fn is_ready(&self) -> bool {
        self.inner.is_some()
    }

    /// Resolve a torrent stream to a playable loopback URL.
    ///
    /// `file_idx` is the addon's suggested file (may be missing or wrong);
    /// `requested_name` is the stream row label, used as a tiebreaker when
    /// choosing between several video files. `cb` runs on a worker thread —
    /// callers marshal to the UI thread themselves.
    pub fn resolve(
        &self,
        request: PlaybackRequest,
        info_hash: String,
        file_idx: Option<u32>,
        requested_name: String,
        settings: TorrentSettings,
        cb: impl FnOnce(Result<StreamReady, String>) + Send + 'static,
    ) {
        let Some(inner) = self.inner.clone() else {
            cb(Err("torrent engine unavailable".into()));
            return;
        };
        {
            let mut requests = inner.playback_requests.lock().unwrap();
            requests.retain(|request| request.strong_count() > 0);
            requests.push(Arc::downgrade(&request.0));
        }
        let spawn_result = std::thread::Builder::new()
            .name("torrent-resolve".into())
            .spawn(move || {
                let result = inner.clone().resolve_blocking(
                    request,
                    info_hash,
                    file_idx,
                    requested_name,
                    settings,
                );
                cb(result);
            });
        if let Err(e) = spawn_result {
            // Spawn failed (process teardown): report rather than hang.
            eprintln!("nova torrent: could not start worker: {e}");
        }
    }

    pub fn start_download(
        &self,
        request: TorrentDownloadRequest,
        on_location: impl FnOnce(DownloadLocation) + Send + 'static,
        on_progress: impl FnMut(TorrentStats) + Send + 'static,
        on_complete: impl FnOnce(Result<TorrentDownloadReady, String>) + Send + 'static,
    ) {
        let Some(inner) = self.inner.clone() else {
            on_complete(Err("torrent engine unavailable".into()));
            return;
        };
        let key = request.owner_id.clone();
        let hash = canonical_hash(&request.info_hash);
        let job_id = inner.next_download_id.fetch_add(1, Ordering::Relaxed);
        let dir = request.output_directory.join(&hash);
        {
            let operation = inner.operations.lock().unwrap();
            if request.cancel.load(Ordering::Acquire) {
                drop(operation);
                on_complete(Err("torrent download cancelled".into()));
                return;
            }
            let mut retained = inner.retained.lock().unwrap();
            if let Some(entry) = retained.get_mut(&key) {
                if entry.info_hash != hash
                    || !entry.restart(job_id, request.file_idx, request.cancel.clone())
                {
                    drop(retained);
                    on_complete(Err("torrent download already retained".into()));
                    return;
                }
            } else {
                retained.insert(
                    key.clone(),
                    RetainedEntry {
                        info_hash: hash,
                        torrent_id: UNMANAGED,
                        dir,
                        file_idx: None,
                        requested_file_idx: request.file_idx,
                        file_len: None,
                        job_id,
                        cancel: request.cancel.clone(),
                        complete: false,
                        worker_active: true,
                    },
                );
            }
        }
        let completion = Arc::new(Mutex::new(
            Some(Box::new(on_complete) as DownloadCompletion),
        ));
        let worker_completion = completion.clone();
        let worker_inner = inner.clone();
        let worker_key = key.clone();
        let spawn_result = std::thread::Builder::new()
            .name("torrent-download".into())
            .spawn(move || {
                let worker = RetainedWorker {
                    inner: worker_inner.clone(),
                    key: worker_key.clone(),
                    job_id,
                };
                let result = worker_inner.download_blocking(
                    request,
                    worker_key.clone(),
                    job_id,
                    on_location,
                    on_progress,
                );
                // Release the worker reservation before notifying the queue,
                // which may immediately restart this same paused download.
                drop(worker);
                deliver_download_completion(&worker_completion, result);
            });
        if let Err(e) = spawn_result {
            if let Some(entry) = inner.retained.lock().unwrap().get_mut(&key) {
                entry.worker_finished(job_id);
            }
            eprintln!("nova torrent: could not start download worker: {e}");
            deliver_download_completion(&completion, Err(format!("could not start worker: {e}")));
        }
    }

    pub fn is_retained(&self, info_hash: &str) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.retained.lock().unwrap().contains_key(info_hash))
    }

    pub fn retained_stats(&self, info_hash: &str) -> Option<TorrentStats> {
        let inner = self.inner.as_ref()?;
        let (id, file_idx, file_len) = {
            let retained = inner.retained.lock().unwrap();
            let entry = retained.get(info_hash)?;
            (entry.torrent_id, entry.file_idx, entry.file_len)
        };
        if id == UNMANAGED {
            return None;
        }
        let handle = inner.session.get(TorrentIdOrHash::Id(id))?;
        let stats = handle.stats();
        Some(to_torrent_stats(
            &stats,
            file_len.map(|len| (file_idx.unwrap_or(0), len)),
        ))
    }

    pub fn remove_retained(&self, info_hash: &str) -> Result<bool, String> {
        let Some(inner) = &self.inner else {
            return Ok(false);
        };
        let _operation = inner.operations.lock().unwrap();
        inner.remove_retained(info_hash)
    }

    /// Live stats for the torrent currently playing, if any. Cheap and
    /// non-blocking (a map lookup + librqbit's own atomics).
    pub fn stats(&self, info_hash: &str) -> TorrentStats {
        let Some(inner) = &self.inner else {
            return TorrentStats::default();
        };
        let id = {
            let active = inner.active.lock().unwrap();
            match active.get(&canonical_hash(info_hash)) {
                Some(e) => e.torrent_id,
                None => return TorrentStats::default(),
            }
        };
        let handle = inner.session.get(TorrentIdOrHash::Id(id));
        let Some(handle) = handle else {
            return TorrentStats::default();
        };
        let stats = handle.stats();
        to_torrent_stats(&stats, None)
    }

    /// Release cancelled playback owners after any in-flight activation.
    /// A newer playback request and retained downloads keep their files and
    /// selections. Otherwise pause for replay, or delete when `no_cache` is set.
    pub fn on_playback_stopped(&self, settings: &TorrentSettings) {
        let Some(inner) = self.inner.clone() else {
            return;
        };
        let settings = settings.clone();
        std::thread::spawn(move || {
            let _operation = inner.operations.lock().unwrap();
            inner.stop_cancelled_playback(&settings);
        });
    }

    /// Restore all manifest owners before publishing a newly built engine.
    pub fn restore_downloads(&self, downloads: Vec<RetainedDownload>) {
        let Some(inner) = &self.inner else {
            return;
        };
        let _operation = inner.operations.lock().unwrap();
        for download in downloads {
            inner.retain_download(download);
        }
        inner.sweep_current_dir();
    }

    pub fn retain_download(&self, download: RetainedDownload) {
        if let Some(inner) = &self.inner {
            let _operation = inner.operations.lock().unwrap();
            inner.retain_download(download);
        }
    }

    /// A completed local torrent file still owns its shared bundle while playing.
    pub fn pin_local_playback(
        &self,
        request: PlaybackRequest,
        path: PathBuf,
        ready: impl FnOnce() + Send + 'static,
    ) {
        let Some(inner) = self.inner.clone() else {
            ready();
            return;
        };
        {
            let mut requests = inner.playback_requests.lock().unwrap();
            requests.retain(|request| request.strong_count() > 0);
            requests.push(Arc::downgrade(&request.0));
        }
        std::thread::spawn(move || {
            {
                let _operation = inner.operations.lock().unwrap();
                if request.is_current() {
                    let retained = inner.retained.lock().unwrap();
                    if let Some(entry) = retained
                        .values()
                        .find(|entry| path_is_within(&path, &entry.dir))
                    {
                        inner.active.lock().unwrap().insert(
                            entry.info_hash.clone(),
                            Entry {
                                torrent_id: entry.torrent_id,
                                file_idx: entry.file_idx.unwrap_or(0),
                                dir: entry.dir.clone(),
                                file_len: entry.file_len.unwrap_or(0),
                                last_used: Instant::now(),
                                playing: true,
                                owner: Some(request),
                            },
                        );
                    }
                    drop(retained);
                    inner.save_tracked();
                }
            }
            ready();
        });
    }

    /// Delete every cached torrent (Settings → Clear torrent cache) plus any
    /// orphaned data the live map doesn't own. Torrents still playing are
    /// spared so playback isn't interrupted.
    pub fn clear_cache(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        let _operation = inner.operations.lock().unwrap();
        inner.purge_cache(None, false);
        inner.sweep_current_dir();
    }

    /// On-disk size of the torrent cache folder, in bytes.
    pub fn cache_bytes(&self, dir: &Path) -> u64 {
        cache_bytes(dir)
    }

    /// Apply live-updatable settings (download rate limit, and clearing the
    /// cache when "no caching" is switched on).
    pub fn apply_settings(&self, settings: &TorrentSettings) {
        let Some(inner) = &self.inner else {
            return;
        };
        inner.session.ratelimits.set_download_bps(NonZeroU32::new(
            settings.down_limit_kbps.saturating_mul(1024),
        ));
        let _operation = inner.operations.lock().unwrap();
        if settings.no_cache {
            // Turning "no caching" on must also clear what earlier plays left
            // behind. The torrent that is playing (if any) is deleted when it
            // stops.
            inner.purge_cache(None, false);
        }
    }
}

impl EngineInner {
    fn retain_download(&self, download: RetainedDownload) {
        self.retained
            .lock()
            .unwrap()
            .entry(download.owner_id)
            .or_insert_with(|| RetainedEntry {
                info_hash: canonical_hash(&download.info_hash),
                torrent_id: UNMANAGED,
                dir: download.directory,
                file_idx: download.file_idx,
                requested_file_idx: download.requested_file_idx,
                file_len: None,
                job_id: 0,
                cancel: Arc::new(AtomicBool::new(false)),
                complete: download.complete,
                worker_active: false,
            });
    }

    fn stop_cancelled_playback(&self, settings: &TorrentSettings) {
        let stopped: Vec<_> = {
            let mut active = self.active.lock().unwrap();
            active
                .iter_mut()
                .filter_map(|(hash, entry)| {
                    if entry.playing
                        && entry
                            .owner
                            .as_ref()
                            .is_some_and(|owner| !owner.is_current())
                    {
                        entry.playing = false;
                        entry.owner = None;
                        entry.last_used = Instant::now();
                        Some((hash.clone(), entry.torrent_id, entry.dir.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (hash, id, dir) in stopped {
            self.refresh_transfer(id, &hash, &dir);
        }
        if settings.no_cache {
            self.purge_cache(None, false);
        }
    }

    fn transfer_needs_running(&self, hash: &str, id: TorrentId, dir: &Path) -> bool {
        self.has_playing_transfer(id, dir)
            || Self::retained_needs_running(hash, id, dir, &self.retained.lock().unwrap())
    }

    /// Recompute the selection from every surviving owner before pausing.
    /// Caller holds operations, so a replacement cannot acquire this handle
    /// between the ownership check and the librqbit mutation.
    fn refresh_transfer(&self, id: TorrentId, hash: &str, dir: &Path) {
        if id == UNMANAGED {
            return;
        }
        let Some(handle) = self.session.get(TorrentIdOrHash::Id(id)) else {
            return;
        };
        let mut only = self.active_files_for(id);
        only.extend(
            self.retained
                .lock()
                .unwrap()
                .values()
                .filter(|entry| Self::transfers_match(entry.torrent_id, &entry.dir, id, dir))
                .filter_map(|entry| entry.file_idx),
        );
        let _ = self
            .rt
            .block_on(self.session.update_only_files(&handle, &only));
        if !self.transfer_needs_running(hash, id, dir) {
            self.pause(id);
        }
    }

    fn managed_handle(&self, info_hash: &str) -> Option<Arc<librqbit::ManagedTorrent>> {
        let key = canonical_hash(info_hash);
        if let Some(id) = librqbit::Magnet::parse(&magnet_for(info_hash))
            .ok()
            .and_then(|magnet| magnet.as_id20())
            && let Some(handle) = self.session.get(TorrentIdOrHash::Hash(id))
        {
            return Some(handle);
        }
        self.session.with_torrents(|torrents| {
            let mut found = None;
            for (_, handle) in torrents {
                if handle.info_hash().as_string() == key {
                    found = Some(handle.clone());
                    break;
                }
            }
            found
        })
    }

    async fn select_video(
        &self,
        info_hash: &str,
        file_idx: Option<u32>,
        requested_name: &str,
        output_dir: &Path,
    ) -> Result<SelectedTorrent, String> {
        let (files, torrent_bytes) = if let Some(handle) = self.managed_handle(info_hash) {
            handle
                .with_metadata(|metadata| {
                    let files = metadata
                        .info
                        .iter_file_details_ext()
                        .enumerate()
                        .map(|(idx, details)| {
                            (
                                idx,
                                details
                                    .details
                                    .filename
                                    .to_pathbuf()
                                    .to_string_lossy()
                                    .into_owned(),
                                details.details.len,
                            )
                        })
                        .collect::<Vec<_>>();
                    (files, metadata.torrent_bytes.to_vec())
                })
                .map_err(|e| format!("{e:#}"))?
        } else {
            let list = tokio::time::timeout(
                METADATA_TIMEOUT,
                self.session.add_torrent(
                    AddTorrent::from_url(magnet_for(info_hash)),
                    Some(AddTorrentOptions {
                        list_only: true,
                        overwrite: true,
                        trackers: Some(public_trackers()),
                        output_folder: Some(output_dir.to_string_lossy().into_owned()),
                        ..Default::default()
                    }),
                ),
            )
            .await
            .map_err(|_| "timed out fetching torrent metadata".to_string())?
            .map_err(|e| format!("{e:#}"))?;
            match list {
                AddTorrentResponse::ListOnly(list) => {
                    let files = list
                        .info
                        .iter_file_details_ext()
                        .enumerate()
                        .map(|(idx, details)| {
                            (
                                idx,
                                details
                                    .details
                                    .filename
                                    .to_pathbuf()
                                    .to_string_lossy()
                                    .into_owned(),
                                details.details.len,
                            )
                        })
                        .collect();
                    (files, list.torrent_bytes.to_vec())
                }
                AddTorrentResponse::Added(_, _) | AddTorrentResponse::AlreadyManaged(_, _) => {
                    return Err("unexpected non-list metadata response".into());
                }
            }
        };
        let candidate = pick_video_file(&files, file_idx, requested_name)
            .ok_or_else(|| "no video file in torrent".to_string())?;
        Ok(SelectedTorrent {
            candidate,
            torrent_bytes: Some(torrent_bytes),
        })
    }

    fn active_files_for(&self, id: TorrentId) -> HashSet<usize> {
        self.active
            .lock()
            .unwrap()
            .values()
            .filter(|entry| {
                id != UNMANAGED
                    && entry.torrent_id == id
                    && entry.playing
                    && entry.owner.as_ref().is_none_or(PlaybackRequest::is_current)
            })
            .map(|entry| entry.file_idx)
            .collect()
    }

    fn only_files_for(&self, id: TorrentId, hash: &str, selected: usize) -> HashSet<usize> {
        let mut only = HashSet::from([selected]);
        only.extend(self.active_files_for(id));
        let retained = self.retained.lock().unwrap();
        only.extend(
            retained
                .values()
                .filter(|entry| {
                    (id != UNMANAGED && entry.torrent_id == id)
                        || entry.info_hash == canonical_hash(hash)
                })
                .filter_map(|entry| entry.file_idx),
        );
        only
    }

    async fn add_selected_torrent(
        &self,
        info_hash: &str,
        output_dir: &Path,
        selected: SelectedTorrent,
    ) -> Result<(TorrentId, Arc<librqbit::ManagedTorrent>, bool), String> {
        let add = match selected.torrent_bytes {
            Some(bytes) => AddTorrent::TorrentFileBytes(bytes.into()),
            None => AddTorrent::from_url(magnet_for(info_hash)),
        };
        let response = self
            .session
            .add_torrent(
                add,
                Some(AddTorrentOptions {
                    only_files: Some(
                        self.only_files_for(UNMANAGED, info_hash, selected.candidate.idx)
                            .into_iter()
                            .collect(),
                    ),
                    overwrite: true,
                    trackers: Some(public_trackers()),
                    output_folder: Some(output_dir.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("{e:#}"))?;
        match response {
            AddTorrentResponse::Added(id, handle) => Ok((id, handle, true)),
            AddTorrentResponse::AlreadyManaged(id, handle) => {
                let _ =
                    tokio::time::timeout(Duration::from_secs(30), handle.wait_until_initialized())
                        .await;
                let only = self.only_files_for(id, info_hash, selected.candidate.idx);
                self.session
                    .update_only_files(&handle, &only)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                // librqbit's Error state is not necessarily paused; start()
                // (through unpause) reinitializes it for an explicit retry.
                if handle.is_paused() || handle.stats().error.is_some() {
                    self.session
                        .unpause(&handle)
                        .await
                        .map_err(|e| format!("{e:#}"))?;
                }
                Ok((id, handle, false))
            }
            AddTorrentResponse::ListOnly(_) => Err("unexpected list-only response".into()),
        }
    }

    fn download_blocking(
        self: Arc<Self>,
        request: TorrentDownloadRequest,
        key: String,
        job_id: u64,
        on_location: impl FnOnce(DownloadLocation),
        mut on_progress: impl FnMut(TorrentStats) + Send + 'static,
    ) -> Result<TorrentDownloadReady, String> {
        let TorrentDownloadRequest {
            owner_id: _,
            info_hash,
            file_idx,
            requested_name,
            output_directory,
            cancel,
        } = request;
        if cancel.load(Ordering::Acquire) {
            return Err("torrent download cancelled".into());
        }
        let output_dir = self
            .retained
            .lock()
            .unwrap()
            .get(&key)
            .map(|entry| entry.dir.clone())
            .unwrap_or_else(|| output_directory.join(canonical_hash(&info_hash)));
        let selected = match self.rt.block_on(self.select_video(
            &info_hash,
            file_idx,
            &requested_name,
            &output_dir,
        )) {
            Ok(selected) => selected,
            Err(e) => {
                return Err(e);
            }
        };
        if cancel.load(Ordering::Acquire) {
            return Err("torrent download cancelled".into());
        }
        let operation = self.operations.lock().unwrap();
        if cancel.load(Ordering::Acquire) || !self.retained_job_is_current(&key, job_id) {
            return Err("torrent download cancelled".into());
        }
        let candidate = selected.candidate.clone();
        let (id, handle, added) =
            match self
                .rt
                .block_on(self.add_selected_torrent(&info_hash, &output_dir, selected))
            {
                Ok(added) => added,
                Err(e) => {
                    return Err(e);
                }
            };
        let dir = handle.output_folder().to_path_buf();
        let path = dir.join(&candidate.name);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| candidate.name.clone());
        if !self.install_retained_transfer(&key, job_id, id, &dir, candidate.idx, candidate.len) {
            if added {
                self.delete_unclaimed_transfer(id, &dir);
            }
            return Err("torrent download was removed".into());
        }
        on_location(DownloadLocation {
            directory: dir.clone(),
            file_idx: candidate.idx,
        });
        drop(operation);
        loop {
            if cancel.load(Ordering::Acquire) {
                self.stop_cancelled_transfer(&key, job_id, id);
                return Err("torrent download cancelled".into());
            }
            if !self.retained_job_is_current(&key, job_id) {
                return Err("torrent download was removed".into());
            }
            let Some(handle) = self.session.get(TorrentIdOrHash::Id(id)) else {
                return Err("torrent download disappeared from the session".into());
            };
            let raw = handle.stats();
            on_progress(to_torrent_stats(&raw, Some((candidate.idx, candidate.len))));
            if let Some(error) = raw.error {
                return Err(error);
            }
            if selected_file_is_complete(&raw, candidate.idx, candidate.len) {
                if !path.is_file() {
                    return Err("completed torrent file is missing".into());
                }
                let _operation = self.operations.lock().unwrap();
                if !self.mark_retained_complete(&key, job_id) {
                    return Err("torrent download was removed".into());
                }
                if !self.transfer_needs_running(&info_hash, id, &dir) {
                    self.pause(id);
                    if !handle.is_paused() {
                        return Err("could not stop completed torrent".into());
                    }
                }
                return Ok(TorrentDownloadReady {
                    path,
                    file_name,
                    file_len: candidate.len,
                });
            }
            std::thread::sleep(DOWNLOAD_PROGRESS_INTERVAL);
        }
    }

    fn install_retained_transfer(
        &self,
        key: &str,
        job_id: u64,
        id: TorrentId,
        dir: &Path,
        file_idx: usize,
        file_len: u64,
    ) -> bool {
        let mut retained = self.retained.lock().unwrap();
        for entry in retained.values_mut() {
            if paths_equivalent(&entry.dir, dir) {
                entry.torrent_id = id;
            }
        }
        let Some(entry) = retained.get_mut(key) else {
            return false;
        };
        if entry.job_id != job_id {
            return false;
        }
        entry.torrent_id = id;
        entry.dir = dir.to_path_buf();
        entry.file_idx = Some(file_idx);
        entry.file_len = Some(file_len);
        true
    }

    fn retained_job_is_current(&self, key: &str, job_id: u64) -> bool {
        self.retained
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|entry| entry.job_id == job_id)
    }

    fn mark_retained_complete(&self, key: &str, job_id: u64) -> bool {
        let mut retained = self.retained.lock().unwrap();
        let Some(entry) = retained.get_mut(key) else {
            return false;
        };
        if entry.job_id != job_id {
            return false;
        }
        entry.complete = true;
        true
    }

    fn stop_cancelled_transfer(&self, key: &str, job_id: u64, id: TorrentId) {
        let _operation = self.operations.lock().unwrap();
        if !self.retained_job_is_current(key, job_id) {
            return;
        }
        let dir = self
            .retained
            .lock()
            .unwrap()
            .get(key)
            .map(|entry| (entry.info_hash.clone(), entry.dir.clone()));
        if !dir
            .as_ref()
            .is_some_and(|(hash, dir)| self.transfer_needs_running(hash, id, dir))
        {
            self.pause(id);
        }
    }

    fn delete_unclaimed_transfer(&self, id: TorrentId, dir: &Path) {
        let active_share = self
            .active
            .lock()
            .unwrap()
            .values()
            .any(|entry| Self::transfers_match(entry.torrent_id, &entry.dir, id, dir));
        let retained_share = self
            .retained
            .lock()
            .unwrap()
            .values()
            .any(|entry| Self::transfers_match(entry.torrent_id, &entry.dir, id, dir));
        if active_share || retained_share {
            return;
        }
        let session = self.session.clone();
        let _ = self
            .rt
            .block_on(async move { session.delete(TorrentIdOrHash::Id(id), false).await });
        let _ = std::fs::remove_dir_all(dir);
    }

    fn resolve_blocking(
        self: Arc<Self>,
        request: PlaybackRequest,
        info_hash: String,
        file_idx: Option<u32>,
        requested_name: String,
        settings: TorrentSettings,
    ) -> Result<StreamReady, String> {
        if !request.is_current() {
            return Err("playback superseded".into());
        }
        let info_hash = canonical_hash(&info_hash);
        let subdir = self.dir.join(&info_hash);
        let selected =
            self.rt
                .block_on(self.select_video(&info_hash, file_idx, &requested_name, &subdir))?;
        self.activate_playback(
            request,
            info_hash,
            requested_name,
            settings,
            selected,
            || {},
        )
    }

    fn activate_playback(
        self: &Arc<Self>,
        request: PlaybackRequest,
        info_hash: String,
        requested_name: String,
        settings: TorrentSettings,
        selected: SelectedTorrent,
        initialized: impl FnOnce(),
    ) -> Result<StreamReady, String> {
        let _operation = self.operations.lock().unwrap();
        if !request.is_current() {
            return Err("playback superseded".into());
        }
        let subdir = self.dir.join(&info_hash);
        self.stop_cancelled_playback(&settings);
        let subdir = self
            .retained
            .lock()
            .unwrap()
            .values()
            .find(|entry| entry.info_hash == info_hash)
            .map(|entry| entry.dir.clone())
            .unwrap_or(subdir);
        let picked = selected.candidate.clone();
        let (id, handle, added) = self
            .rt
            .block_on(self.add_selected_torrent(&info_hash, &subdir, selected))?;

        for entry in self.retained.lock().unwrap().values_mut() {
            if paths_equivalent(&entry.dir, handle.output_folder()) {
                entry.torrent_id = id;
            }
        }

        // 3b. Wait for storage initialization. The HTTP stream endpoint
        // rejects requests while the torrent is still Initializing
        // ("with_storage_and_file: invalid state: initializing"), and mpv
        // issues its first Range request as soon as the player opens, so
        // returning the URL too early would fail playback.
        self.rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(30), handle.wait_until_initialized())
                .await;
        });

        initialized();
        if !request.is_current() {
            if added {
                self.delete_unclaimed_transfer(id, handle.output_folder());
            }
            self.refresh_transfer(id, &info_hash, handle.output_folder());
            return Err("playback superseded".into());
        }

        // 4. Record it for LRU / stats / cleanup.
        let dir = handle.output_folder().to_path_buf();
        // `requested_name` is the full (possibly multi-line) stream row text;
        // fall back to its longest line so the status line stays one line.
        let display_name = handle.name().filter(|n| !n.is_empty()).unwrap_or_else(|| {
            requested_name
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .max_by_key(|l| l.len())
                .unwrap_or("")
                .to_string()
        });
        {
            let mut active = self.active.lock().unwrap();
            let entry = active.entry(info_hash.clone()).or_insert_with(|| Entry {
                torrent_id: id,
                file_idx: picked.idx,
                dir: dir.clone(),
                file_len: picked.len,
                last_used: Instant::now(),
                playing: true,
                owner: Some(request.clone()),
            });
            entry.torrent_id = id;
            entry.file_idx = picked.idx;
            entry.dir = dir.clone();
            entry.file_len = picked.len;
            entry.last_used = Instant::now();
            entry.playing = true;
            entry.owner = Some(request.clone());
        }
        self.save_tracked();

        // 5. Enforce the cache cap (never evicting the just-started torrent).
        // With no-cache, drop everything else first so switching streams can't
        // leave the previous torrent's data on disk.
        if settings.no_cache {
            self.purge_cache(Some(&info_hash), true);
        } else {
            self.trim_to_cap(settings.max_mb, &info_hash);
        }

        Ok(StreamReady {
            url: format!(
                "http://127.0.0.1:{}/torrents/{}/stream/{}",
                self.port, id, picked.idx
            ),
            display_name,
        })
    }

    /// Adopt the durable tracked set from the previous process: entries whose
    /// folders still exist rejoin the map as idle + unmanaged (the new
    /// session never managed them), so trimming, clearing and accounting see
    /// them again. Vanished folders are pruned from the persisted set.
    fn adopt_persisted(self: &Arc<Self>) {
        let tracked = load_tracked();
        if tracked.is_empty() {
            return;
        }
        let mut adopted = 0;
        {
            let mut active = self.active.lock().unwrap();
            for (hash, t) in &tracked {
                if !t.dir.is_dir() {
                    continue;
                }
                // One folder, one owner: a second hash resolving to an
                // already-adopted dir shares it (same torrent under two URL
                // forms) and must not double-delete it later.
                if active.values().any(|e| e.dir == t.dir) {
                    continue;
                }
                active.insert(
                    canonical_hash(hash),
                    Entry {
                        torrent_id: UNMANAGED,
                        file_idx: t.file_idx,
                        dir: t.dir.clone(),
                        file_len: t.file_len,
                        last_used: Instant::now(),
                        playing: false,
                        owner: None,
                    },
                );
                adopted += 1;
            }
        }
        eprintln!("nova torrent: adopted {adopted} cached torrent(s) from last run");
        self.save_tracked();
    }

    /// Persist the live tracked set.
    fn save_tracked(&self) {
        let active = self.active.lock().unwrap();
        save_tracked(&active);
    }

    /// Delete orphaned data in the session dir: anything the live map doesn't
    /// own (crash-mid-resolve leftovers, pre-tracking layouts, failed deletes
    /// from before).
    fn sweep_current_dir(&self) {
        let owned: HashSet<PathBuf> = {
            let active = self.active.lock().unwrap();
            let retained = self.retained.lock().unwrap();
            active
                .values()
                .map(|entry| entry.dir.clone())
                .chain(retained.values().map(|entry| entry.dir.clone()))
                .collect()
        };
        sweep_orphans(&self.dir, &owned, self.sweep_root_files);
    }

    /// The least-recently-used evictable entry: not playing, not spared by
    /// `keep`, not already attempted this pass. Pure over a snapshot, so the
    /// trim loop (which needs the session) stays thin and this stays testable.
    fn select_victim(
        active: &HashMap<String, Entry>,
        retained: &HashMap<String, RetainedEntry>,
        keep: &str,
        attempted: &HashSet<String>,
    ) -> Option<(String, TorrentId)> {
        active
            .iter()
            .filter(|(hash, entry)| {
                !entry.playing
                    && hash.as_str() != keep
                    && !attempted.contains(hash.as_str())
                    && !Self::entry_is_retained(hash, entry, retained)
            })
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(hash, entry)| (hash.clone(), entry.torrent_id))
    }

    /// Delete least-recently-used torrents until the cache fits `max_mb`.
    /// `keep` (the torrent just started) is never evicted.
    fn trim_to_cap(self: &Arc<Self>, max_mb: u64, keep: &str) {
        if max_mb == 0 {
            return;
        }
        let limit = max_mb.saturating_mul(1024 * 1024);
        // Torrents whose deletion failed stay tracked for a retry; the
        // attempted set keeps one pass from spinning on them forever.
        let mut attempted: HashSet<String> = HashSet::new();
        loop {
            let bytes = self.total_cache_bytes();
            if bytes <= limit {
                break;
            }
            let victim = {
                let active = self.active.lock().unwrap();
                let retained = self.retained.lock().unwrap();
                Self::select_victim(&active, &retained, keep, &attempted)
            };
            let Some((hash, id)) = victim else {
                break;
            };
            if !attempted.insert(hash.clone()) {
                break;
            }
            self.forget_and_delete(id, &hash);
        }
    }

    fn total_cache_bytes(&self) -> u64 {
        let dirs: Vec<PathBuf> = {
            let active = self.active.lock().unwrap();
            let retained = self.retained.lock().unwrap();
            active
                .iter()
                .filter(|(hash, entry)| !Self::entry_is_retained(hash, entry, &retained))
                .map(|(_, entry)| entry.dir.clone())
                .collect()
        };
        dirs.iter().map(|d| cache_bytes(d)).sum()
    }

    fn pause(&self, id: TorrentId) {
        let session = self.session.clone();
        self.rt.block_on(async move {
            if let Some(handle) = session.get(TorrentIdOrHash::Id(id)) {
                let _ = session.pause(&handle).await;
            }
        });
    }

    fn forget_and_delete(&self, id: TorrentId, info_hash: &str) {
        let Some(dir) = self
            .active
            .lock()
            .unwrap()
            .get(info_hash)
            .map(|entry| entry.dir.clone())
        else {
            return;
        };
        let retained = self.retained.lock().unwrap();
        let retained_share = retained
            .values()
            .any(|entry| Self::transfers_match(entry.torrent_id, &entry.dir, id, &dir));
        drop(retained);
        let active_share = self.active.lock().unwrap().iter().any(|(hash, entry)| {
            hash.as_str() != info_hash
                && Self::transfers_match(entry.torrent_id, &entry.dir, id, &dir)
        });
        if retained_share || active_share {
            self.active.lock().unwrap().remove(info_hash);
            self.save_tracked();
            return;
        }
        if id != UNMANAGED {
            let session = self.session.clone();
            let _ = self
                .rt
                .block_on(async move { session.delete(TorrentIdOrHash::Id(id), false).await });
        }
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "nova torrent: could not delete torrent {info_hash} data at {}: {e:#}",
                dir.display()
            );
            return;
        }
        self.active.lock().unwrap().remove(info_hash);
        self.save_tracked();
    }

    /// Delete cached torrents. `keep` spares one info hash; `include_playing`
    /// also removes torrents still marked as playing (the no-cache path calls
    /// this once playback has stopped, so nothing is left behind).
    fn purge_cache(&self, keep: Option<&str>, include_playing: bool) {
        let victims: Vec<(String, TorrentId)> = {
            let active = self.active.lock().unwrap();
            let retained = self.retained.lock().unwrap();
            active
                .iter()
                .filter(|(_, entry)| include_playing || !entry.playing)
                .filter(|(hash, _)| keep.map(|key| hash.as_str() != key).unwrap_or(true))
                .filter(|(hash, entry)| !Self::entry_is_retained(hash, entry, &retained))
                .map(|(hash, entry)| (hash.clone(), entry.torrent_id))
                .collect()
        };
        for (hash, id) in victims {
            self.forget_and_delete(id, &hash);
        }
    }

    fn entry_is_retained(
        info_hash: &str,
        entry: &Entry,
        retained: &HashMap<String, RetainedEntry>,
    ) -> bool {
        let key = canonical_hash(info_hash);
        retained.values().any(|retained_entry| {
            retained_entry.info_hash == key
                || Self::transfers_match(
                    retained_entry.torrent_id,
                    &retained_entry.dir,
                    entry.torrent_id,
                    &entry.dir,
                )
        })
    }

    fn retained_needs_running(
        info_hash: &str,
        id: TorrentId,
        dir: &Path,
        retained: &HashMap<String, RetainedEntry>,
    ) -> bool {
        let key = canonical_hash(info_hash);
        retained.values().any(|entry| {
            entry.worker_active
                && !entry.complete
                && !entry.cancel.load(Ordering::Acquire)
                && (entry.info_hash == key
                    || Self::transfers_match(entry.torrent_id, &entry.dir, id, dir))
        })
    }

    fn transfers_match(
        left_id: TorrentId,
        left_dir: &Path,
        right_id: TorrentId,
        right_dir: &Path,
    ) -> bool {
        (left_id != UNMANAGED && left_id == right_id) || paths_equivalent(left_dir, right_dir)
    }

    fn has_playing_transfer(&self, id: TorrentId, dir: &Path) -> bool {
        self.active.lock().unwrap().values().any(|entry| {
            entry.playing
                && entry.owner.as_ref().is_none_or(PlaybackRequest::is_current)
                && Self::transfers_match(entry.torrent_id, &entry.dir, id, dir)
        })
    }

    fn remove_retained(&self, key: &str) -> Result<bool, String> {
        let entry = self.retained.lock().unwrap().remove(key);
        let Some(mut entry) = entry else {
            return Ok(false);
        };
        entry.cancel.store(true, Ordering::Release);
        let retained_share = self.retained.lock().unwrap().values().any(|retained| {
            Self::transfers_match(
                retained.torrent_id,
                &retained.dir,
                entry.torrent_id,
                &entry.dir,
            )
        });
        // The tracked directory is also the legacy proof for cache-origin
        // download owners. Do not erase it while a sibling still uses it.
        if !retained_share {
            self.active.lock().unwrap().retain(|_, active| {
                active.playing
                    || !Self::transfers_match(
                        active.torrent_id,
                        &active.dir,
                        entry.torrent_id,
                        &entry.dir,
                    )
            });
            self.save_tracked();
        }
        let active_share = self.active.lock().unwrap().values().any(|active| {
            Self::transfers_match(active.torrent_id, &active.dir, entry.torrent_id, &entry.dir)
        });
        if active_share || retained_share {
            self.refresh_transfer(entry.torrent_id, &entry.info_hash, &entry.dir);
            return Ok(true);
        }
        if entry.torrent_id != UNMANAGED {
            let session = self.session.clone();
            let id = entry.torrent_id;
            if let Err(e) = self.rt.block_on(async move {
                session
                    .delete(TorrentIdOrHash::Id(id), false)
                    .await
                    .map_err(|error| format!("{error:#}"))
            }) {
                self.retained.lock().unwrap().insert(key.to_string(), entry);
                return Err(e);
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&entry.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            entry.torrent_id = UNMANAGED;
            let dir = entry.dir.display().to_string();
            self.retained.lock().unwrap().insert(key.to_string(), entry);
            return Err(format!(
                "could not remove retained torrent directory {dir}: {e}"
            ));
        }
        Ok(true)
    }
}

fn deliver_download_completion(
    slot: &Mutex<Option<DownloadCompletion>>,
    result: Result<TorrentDownloadReady, String>,
) {
    let callback = slot.lock().unwrap().take();
    if let Some(callback) = callback {
        callback(result);
    }
}

fn to_torrent_stats(
    stats: &librqbit::TorrentStats,
    selected: Option<(usize, u64)>,
) -> TorrentStats {
    let (progress_bytes, total_bytes) = match selected {
        Some((idx, len)) if idx < stats.file_progress.len() => (stats.file_progress[idx], len),
        _ => (stats.progress_bytes, stats.total_bytes),
    };
    let progress = if total_bytes == 0 {
        0.0
    } else {
        (progress_bytes as f64 / total_bytes as f64) as f32
    };
    let (down_bps, peers) = stats
        .live
        .as_ref()
        .map(|live| {
            (
                live.download_speed.as_bytes(),
                live.snapshot.peer_stats.live as usize,
            )
        })
        .unwrap_or((0, 0));
    TorrentStats {
        down_bps,
        peers,
        progress,
        downloaded_bytes: progress_bytes,
        total_bytes,
    }
}

fn selected_file_is_complete(
    stats: &librqbit::TorrentStats,
    file_idx: usize,
    file_len: u64,
) -> bool {
    stats
        .file_progress
        .get(file_idx)
        .is_some_and(|progress| *progress >= file_len)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    left == right
        || match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
}

/// A parsed file candidate inside a torrent.
#[derive(Clone, Debug, PartialEq)]
pub struct FileCandidate {
    pub idx: usize,
    pub name: String,
    pub len: u64,
}

/// Choose the video file to stream.
///
/// Order of preference:
/// 1. the addon's `file_idx`, when it names a video file;
/// 2. the feature video whose name best matches the stream row text
///    (case-insensitive substring, then shared-token count);
/// 3. the largest feature video.
///
/// Sample/trailer/extra clips are ignored while a real feature is present, so
/// a torrent that ships `movie.mkv` next to `Sample/sample.mkv` (or a bonus
/// clip larger than the feature) still streams the feature.
///
/// Returns `None` when the torrent has no video extension at all.
pub fn pick_video_file(
    files: &[(usize, String, u64)],
    file_idx: Option<u32>,
    requested_name: &str,
) -> Option<FileCandidate> {
    let videos: Vec<FileCandidate> = files
        .iter()
        .filter(|(_, name, _)| is_video(name))
        .map(|(idx, name, len)| FileCandidate {
            idx: *idx,
            name: name.clone(),
            len: *len,
        })
        .collect();

    if let Some(idx) = file_idx
        && let Some(v) = videos.iter().find(|v| v.idx == idx as usize)
    {
        return Some(v.clone());
    }

    // Drop obvious sidecars unless that would leave nothing to play.
    let features: Vec<FileCandidate> = videos
        .iter()
        .filter(|v| !is_sidecar(&v.name))
        .cloned()
        .collect();
    let candidates = if features.is_empty() {
        videos
    } else {
        features
    };

    if let Some(v) = best_name_match(&candidates, requested_name) {
        return Some(v.clone());
    }

    candidates.into_iter().max_by_key(|v| v.len)
}

fn is_video(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    matches!(ext.as_deref(), Some(e) if VIDEO_EXTS.contains(&e))
}

/// True for names that mark a sample, trailer or bonus clip rather than the
/// feature itself. Matched on whole words so a title like "Extraction" is not
/// mistaken for an "extra".
fn is_sidecar(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "sample",
        "trailer",
        "teaser",
        "preview",
        "featurette",
        "extra",
        "extras",
        "interview",
        "deleted",
    ];
    let lower = name.to_ascii_lowercase();
    let stem = Path::new(&lower)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&lower);
    stem.split(|c: char| !c.is_alphanumeric())
        .any(|tok| !tok.is_empty() && WORDS.contains(&tok))
}

/// Pick the video whose file name best matches the stream row text.
///
/// Torrentio-style rows put the release name on a later line (the first line
/// is just the addon name), so every non-empty line is tried as a needle.
fn best_name_match<'a>(
    videos: &'a [FileCandidate],
    requested_name: &str,
) -> Option<&'a FileCandidate> {
    let lines: Vec<String> = requested_name
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || videos.is_empty() {
        return None;
    }
    // The release name is the longest line; short lines are quality/addon
    // labels ("4k", "1080p") that would substring-match every file.
    let release = lines.iter().max_by_key(|l| l.len())?;
    if let Some(v) = videos
        .iter()
        .find(|v| v.name.to_ascii_lowercase().contains(release.as_str()))
    {
        return Some(v);
    }
    // Token overlap across every line of the row.
    let tokens: Vec<&str> = lines
        .iter()
        .flat_map(|l| l.split(|c: char| !c.is_alphanumeric()))
        .filter(|t| t.len() > 2)
        .collect();
    videos
        .iter()
        .map(|v| {
            let low = v.name.to_ascii_lowercase();
            let score = tokens.iter().filter(|t| low.contains(**t)).count();
            (score, v)
        })
        .filter(|(score, _)| *score > 0)
        // Break token-score ties toward the larger file (the feature, not a
        // same-named clip).
        .max_by_key(|(score, v)| (*score, v.len))
        .map(|(_, v)| v)
}

/// The public tracker list as owned strings for [`librqbit::AddTorrentOptions`].
fn public_trackers() -> Vec<String> {
    PUBLIC_TRACKERS.iter().map(|t| (*t).to_string()).collect()
}

/// Build the magnet for a bare infohash (some addons send the magnet form
/// already; accept that too).
///
/// The public trackers are appended as `&tr=` parameters: for magnet inputs
/// librqbit only reads trackers from the URL itself (`AddTorrentOptions`'s
/// `trackers` is merged for torrent-file adds only), so a bare
/// `magnet:?xt=urn:btih:…` would otherwise rely on DHT alone.
fn magnet_for(info_hash: &str) -> String {
    let h = info_hash.trim();
    let mut magnet = if h.starts_with("magnet:") {
        h.to_string()
    } else {
        format!("magnet:?xt=urn:btih:{h}")
    };
    for tr in PUBLIC_TRACKERS {
        magnet.push_str("&tr=");
        magnet.push_str(tr);
    }
    magnet
}

/// Bytes used by `dir` and its subdirectories (0 when absent).
pub fn cache_bytes(dir: &Path) -> u64 {
    fn walk(dir: &Path, out: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let Ok(ty) = entry.file_type() else { continue };
            let path = entry.path();
            if ty.is_dir() {
                walk(&path, out);
            } else if ty.is_file()
                && let Ok(meta) = entry.metadata()
            {
                *out += meta.len();
            }
        }
    }
    let mut out = 0;
    walk(dir, &mut out);
    out
}

/// Delete a torrent's on-disk folder after playback when caching is disabled.
/// Exposed for the app layer's no-cache path.
pub fn delete_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Helper used by the app's no-cache path: true when a torrent's data should
/// be deleted once playback stops.
pub fn should_delete_after_playback(settings: &TorrentSettings) -> bool {
    settings.no_cache
}

/// `torrent_cache`: the tracked set, so restarts adopt (rather than orphan)
/// previously downloaded data. Keyed by info hash as passed at resolve time.
const TRACKED_KEY: &str = "torrent_cache";

/// Durable form of one tracked torrent. `playing` is deliberately not stored:
/// nothing presents anything at startup, so every adopted entry starts idle.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrackedTorrent {
    dir: PathBuf,
    file_len: u64,
    file_idx: usize,
    last_used_secs: u64,
}

/// Directory name for a torrent: the bare infohash, lower-cased. Accepts the
/// magnet form addons sometimes send (first 40/64-hex run wins). Falls back
/// to a stable hash squeeze for anything unparseable (never empty, never a
/// separator) so the dir logic stays total.
fn canonical_hash(info_hash: &str) -> String {
    let lower = info_hash.trim().to_ascii_lowercase();
    if let Some(run) = lower
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|r| r.len() == 40 || r.len() == 64)
    {
        return run.to_string();
    }
    format!("t-{:016x}", nova_config::fnv1a(lower.as_bytes()))
}

/// True for directory names this engine creates (see [`canonical_hash`]).
fn is_torrent_dir_name(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy().to_ascii_lowercase();
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Root-level files that are never swept (session DHT state).
const SWEEP_KEEP_FILES: &[&str] = &["dht.json"];

/// Locate a job's bundle from explicit persisted ownership or the legacy
/// playback cache record. Never infer a bundle root from a nested filename.
pub fn download_directory(
    info_hash: &str,
    output_directory: &Path,
    persisted: Option<&Path>,
) -> PathBuf {
    let hash = canonical_hash(info_hash);
    let default = output_directory.join(&hash);
    let tracked = load_tracked()
        .into_iter()
        .find(|(key, _)| canonical_hash(key) == hash)
        .map(|(_, entry)| entry.dir);
    if let Some(persisted) = persisted
        && (paths_equivalent(persisted, &default)
            || tracked
                .as_deref()
                .is_some_and(|dir| paths_equivalent(dir, persisted)))
    {
        return persisted.to_path_buf();
    }
    tracked.unwrap_or(default)
}

pub fn path_is_within(path: &Path, directory: &Path) -> bool {
    match (
        std::fs::canonicalize(path),
        std::fs::canonicalize(directory),
    ) {
        (Ok(path), Ok(directory)) => path != directory && path.starts_with(directory),
        _ => false,
    }
}

/// Offline cleanup when P2P is disabled. Snapshots are validated by the same
/// directory resolver used for restoration; sibling owners keep the bundle.
pub fn remove_download_files(
    download: &RetainedDownload,
    remaining: &[RetainedDownload],
) -> Result<(), String> {
    if remaining
        .iter()
        .any(|other| paths_equivalent(&download.directory, &other.directory))
    {
        return Ok(());
    }
    if let Err(error) = std::fs::remove_dir_all(&download.directory)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error.to_string());
    }
    let mut tracked = load_tracked();
    tracked.retain(|_, entry| !paths_equivalent(&entry.dir, &download.directory));
    if let Ok(json) = serde_json::to_string(&tracked) {
        nova_storage::set_str(TRACKED_KEY, &json);
    }
    Ok(())
}

/// Load the persisted tracked set (empty when absent/unreadable, or when
/// storage isn't initialized, e.g. in unit tests).
fn load_tracked() -> HashMap<String, TrackedTorrent> {
    nova_storage::get_str(TRACKED_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the live tracked set.
fn save_tracked(active: &HashMap<String, Entry>) {
    let map: HashMap<String, TrackedTorrent> = active
        .iter()
        .map(|(hash, e)| {
            (
                hash.clone(),
                TrackedTorrent {
                    dir: e.dir.clone(),
                    file_len: e.file_len,
                    file_idx: e.file_idx,
                    last_used_secs: nova_config::now_secs(),
                },
            )
        })
        .collect();
    match serde_json::to_string(&map) {
        Ok(s) => nova_storage::set_str(TRACKED_KEY, &s),
        Err(e) => eprintln!("nova torrent: serialize tracked set: {e}"),
    }
}

/// Delete orphaned torrent data in `dir`: subdirectories no live entry owns.
/// With `aggressive` (only ever true for the app-private default cache dir,
/// never a user-chosen folder) any unknown subdir goes, plus root-level
/// files outside [`SWEEP_KEEP_FILES`]; otherwise only infohash-named subdirs
/// are removed, so user files in a custom folder are never touched. This
/// also retires pre-isolation layouts (torrent-name subdirs, loose files).
fn sweep_orphans(dir: &Path, owned: &HashSet<PathBuf>, aggressive: bool) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    // Canonicalize once so `a/b` vs `a//b` style mismatches can't fake an
    // orphan. Failures fall back to the raw path (never skip deletion over
    // a canonicalization error).
    let owned: HashSet<PathBuf> = owned
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    for entry in rd.flatten() {
        let Ok(ty) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ty.is_dir() {
            if !aggressive && !is_torrent_dir_name(&entry.file_name()) {
                continue;
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if owned.iter().any(|owner| owner.starts_with(&canonical)) {
                continue;
            }
            eprintln!(
                "nova torrent: removing orphaned cache dir {}",
                path.display()
            );
            if let Err(e) = std::fs::remove_dir_all(&path) {
                eprintln!("nova torrent: could not remove {}: {e:#}", path.display());
            }
        } else if aggressive && ty.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SWEEP_KEEP_FILES.contains(&name.as_str()) {
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("nova torrent: could not remove {}: {e:#}", path.display());
            }
        }
    }
}

/// Slot holding the process-wide engine, installed once at startup.
static ENGINE: Mutex<Option<Arc<TorrentEngine>>> = Mutex::new(None);

/// Install (or replace) the process-wide engine.
pub fn install(engine: TorrentEngine) {
    *ENGINE.lock().unwrap() = Some(Arc::new(engine));
}

/// Tear down the process-wide engine, releasing its session, sockets and
/// tokio runtime. Called when the user turns torrent playback off so a
/// disabled setting costs nothing.
pub fn uninstall() {
    if let Some(engine) = ENGINE.lock().unwrap().take() {
        engine.cancel_all();
        // A worker may still own this engine. Its cancellation and removal
        // continue through the same operations lock until that worker exits.
        std::thread::spawn(move || drop(engine));
    }
}

/// The installed engine, if any.
pub fn engine() -> Option<Arc<TorrentEngine>> {
    ENGINE.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_HASH: &str = "e31fa81b9fb7a29e1999e1555b55cf7f34b99353";

    fn offline_engine(label: &str) -> Arc<EngineInner> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "nova-owners-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let session = rt
            .block_on(Session::new_with_opts(
                dir.clone(),
                SessionOptions {
                    dht: None,
                    listen: None,
                    disable_trackers: true,
                    disable_local_service_discovery: true,
                    persistence: None,
                    ..Default::default()
                },
            ))
            .unwrap();
        Arc::new(EngineInner {
            session,
            operations: Mutex::new(()),
            playback_requests: Mutex::new(Vec::new()),
            port: 1,
            active: Mutex::new(HashMap::new()),
            retained: Mutex::new(HashMap::new()),
            next_download_id: AtomicU64::new(1),
            dir,
            sweep_root_files: true,
            rt,
        })
    }

    fn fixture_selection(idx: usize) -> SelectedTorrent {
        let mut bytes = b"d4:infod5:filesld6:lengthi1e4:pathl5:a.mkveed6:lengthi1e4:pathl5:b.mkveee4:name6:bundle12:piece lengthi16384e6:pieces20:".to_vec();
        bytes.extend([
            218, 35, 97, 78, 2, 70, 154, 13, 124, 123, 209, 189, 171, 92, 156, 71, 75, 25, 4, 220,
        ]);
        bytes.extend(b"ee");
        SelectedTorrent {
            candidate: FileCandidate {
                idx,
                name: if idx == 0 { "a.mkv" } else { "b.mkv" }.into(),
                len: 1,
            },
            torrent_bytes: Some(bytes),
        }
    }

    fn owner(id: &str, dir: &Path, index: Option<usize>) -> RetainedDownload {
        RetainedDownload {
            owner_id: id.into(),
            info_hash: FIXTURE_HASH.into(),
            requested_file_idx: index.map(|i| i as u32),
            directory: dir.to_path_buf(),
            file_idx: index,
            complete: true,
        }
    }

    #[test]
    fn restored_sibling_owners_keep_nested_files_until_last_removal() {
        for reverse in [false, true] {
            let inner = offline_engine("siblings");
            let dir = inner.dir.join(FIXTURE_HASH);
            std::fs::create_dir_all(dir.join("season")).unwrap();
            std::fs::write(dir.join("season/a.mkv"), b"a").unwrap();
            std::fs::write(dir.join("season/b.mkv"), b"b").unwrap();
            let engine = TorrentEngine {
                inner: Some(inner.clone()),
            };
            engine.restore_downloads(vec![owner("a", &dir, Some(0)), owner("b", &dir, Some(1))]);
            assert!(
                dir.join("season/b.mkv").exists(),
                "startup sweep preserves owners"
            );
            let (first, second) = if reverse { ("b", "a") } else { ("a", "b") };
            engine.remove_retained(first).unwrap();
            assert!(dir.join("season/a.mkv").exists());
            assert!(dir.join("season/b.mkv").exists());
            // Restart restoration uses the surviving stable owner, not a session id.
            let restored = offline_engine("restart");
            let restored_engine = TorrentEngine {
                inner: Some(restored.clone()),
            };
            restored_engine.restore_downloads(vec![owner(
                second,
                &dir,
                Some(usize::from(second == "b")),
            )]);
            restored_engine.remove_retained(second).unwrap();
            assert!(!dir.exists());
            std::fs::remove_dir_all(&inner.dir).unwrap();
            std::fs::remove_dir_all(&restored.dir).unwrap();
        }
    }

    #[test]
    fn local_playback_pins_download_bundle_until_its_request_is_cancelled() {
        let inner = offline_engine("local-pin");
        let dir = inner.dir.join(FIXTURE_HASH);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.mkv");
        std::fs::write(&path, b"a").unwrap();
        let engine = TorrentEngine {
            inner: Some(inner.clone()),
        };
        engine.restore_downloads(vec![owner("a", &dir, Some(0))]);
        let request = PlaybackRequest::default();
        let (ready, done) = std::sync::mpsc::channel();
        engine.pin_local_playback(request.clone(), path.clone(), move || {
            ready.send(()).unwrap()
        });
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        engine.remove_retained("a").unwrap();
        assert!(
            path.exists(),
            "playing a local download keeps its shared bundle"
        );
        engine.cancel_all();
        assert!(
            !request.is_current(),
            "shutdown also cancels local playback owners"
        );
        inner.stop_cancelled_playback(&TorrentSettings {
            no_cache: true,
            ..Default::default()
        });
        assert!(!path.exists(), "last cancelled owner permits cleanup");
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn cancelled_activation_cannot_replace_or_evict_newer_playback() {
        let inner = offline_engine("obsolete");
        let old = PlaybackRequest::default();
        old.cancel();
        let current = PlaybackRequest::default();
        inner
            .activate_playback(
                current.clone(),
                FIXTURE_HASH.into(),
                "b".into(),
                TorrentSettings::default(),
                fixture_selection(1),
                || {},
            )
            .unwrap();
        let no_cache = TorrentSettings {
            no_cache: true,
            ..Default::default()
        };
        assert!(
            inner
                .activate_playback(
                    old,
                    FIXTURE_HASH.into(),
                    "a".into(),
                    no_cache.clone(),
                    fixture_selection(0),
                    || panic!("stale request initialized")
                )
                .is_err()
        );
        // Delayed cleanup for an old owner cannot stop the replacement either.
        inner.stop_cancelled_playback(&no_cache);
        let active = inner.active.lock().unwrap();
        let entry = &active[FIXTURE_HASH];
        assert_eq!(entry.file_idx, 1);
        assert!(entry.owner.as_ref().unwrap().same_request(&current));
        assert!(entry.playing);
        drop(active);
        current.cancel();
        inner.stop_cancelled_playback(&no_cache);
        assert!(!inner.dir.join(FIXTURE_HASH).exists());
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn cancellation_during_initialization_preserves_download_owner_and_selection() {
        let inner = offline_engine("cancel-init");
        let dir = inner.dir.join(FIXTURE_HASH);
        inner.retain_download(owner("download", &dir, Some(1)));
        let request = PlaybackRequest::default();
        let cancelled = request.clone();
        assert!(
            inner
                .activate_playback(
                    request,
                    FIXTURE_HASH.into(),
                    "a".into(),
                    TorrentSettings::default(),
                    fixture_selection(0),
                    move || cancelled.cancel()
                )
                .is_err()
        );
        assert!(inner.active.lock().unwrap().is_empty());
        let entry = inner.retained.lock().unwrap()["download"].clone();
        assert_ne!(entry.torrent_id, UNMANAGED);
        let handle = inner
            .session
            .get(TorrentIdOrHash::Id(entry.torrent_id))
            .unwrap();
        assert!(handle.is_paused());
        assert!(dir.exists());
        inner.remove_retained("download").unwrap();
        assert!(!dir.exists());
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn playback_and_downloads_share_selection_and_removal_keeps_playing_files() {
        let inner = offline_engine("playing-share");
        let dir = inner.dir.join(FIXTURE_HASH);
        inner.retain_download(owner("a", &dir, Some(0)));
        inner.retain_download(owner("b", &dir, Some(1)));
        let request = PlaybackRequest::default();
        inner
            .activate_playback(
                request.clone(),
                FIXTURE_HASH.into(),
                "a".into(),
                TorrentSettings::default(),
                fixture_selection(0),
                || {},
            )
            .unwrap();
        let id = inner.active.lock().unwrap()[FIXTURE_HASH].torrent_id;
        assert_eq!(
            inner.only_files_for(id, FIXTURE_HASH, 0),
            HashSet::from([0, 1])
        );
        inner.remove_retained("a").unwrap();
        assert_eq!(
            inner.only_files_for(id, FIXTURE_HASH, 0),
            HashSet::from([0, 1])
        );
        inner.remove_retained("b").unwrap();
        assert!(dir.exists());
        assert!(inner.active.lock().unwrap()[FIXTURE_HASH].playing);
        request.cancel();
        let settings = TorrentSettings {
            no_cache: true,
            ..Default::default()
        };
        inner.stop_cancelled_playback(&settings);
        assert!(!dir.exists());
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn newer_same_hash_activation_waits_for_cancelled_initialization_cleanup() {
        let inner = offline_engine("overlap");
        let old = PlaybackRequest::default();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let old_worker = {
            let inner = inner.clone();
            let old = old.clone();
            std::thread::spawn(move || {
                inner.activate_playback(
                    old,
                    FIXTURE_HASH.into(),
                    "old".into(),
                    TorrentSettings {
                        no_cache: true,
                        ..Default::default()
                    },
                    fixture_selection(0),
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    },
                )
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        old.cancel();
        let current = PlaybackRequest::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let new_worker = {
            let inner = inner.clone();
            let current = current.clone();
            std::thread::spawn(move || {
                let result = inner.activate_playback(
                    current,
                    FIXTURE_HASH.into(),
                    "new".into(),
                    TorrentSettings {
                        no_cache: true,
                        ..Default::default()
                    },
                    fixture_selection(1),
                    || {},
                );
                done_tx.send(result).unwrap();
            })
        };
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap();
        assert!(old_worker.join().unwrap().is_err());
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        new_worker.join().unwrap();
        inner.stop_cancelled_playback(&TorrentSettings {
            no_cache: true,
            ..Default::default()
        });
        assert_eq!(inner.active.lock().unwrap()[FIXTURE_HASH].file_idx, 1);
        assert!(
            inner.active.lock().unwrap()[FIXTURE_HASH]
                .owner
                .as_ref()
                .unwrap()
                .same_request(&current)
        );
        current.cancel();
        inner.stop_cancelled_playback(&TorrentSettings {
            no_cache: true,
            ..Default::default()
        });
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn two_files_from_one_torrent_complete_as_distinct_jobs() {
        let inner = offline_engine("two-jobs");
        let dir = inner.dir.join(FIXTURE_HASH);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mkv"), b"a").unwrap();
        std::fs::write(dir.join("b.mkv"), b"b").unwrap();
        let request = PlaybackRequest::default();
        inner
            .activate_playback(
                request.clone(),
                FIXTURE_HASH.into(),
                "a".into(),
                TorrentSettings::default(),
                fixture_selection(0),
                || {},
            )
            .unwrap();
        request.cancel();
        inner.stop_cancelled_playback(&TorrentSettings::default());
        let engine = TorrentEngine {
            inner: Some(inner.clone()),
        };
        for (idx, owner_id) in ["a", "b"].into_iter().enumerate() {
            let (tx, rx) = std::sync::mpsc::channel();
            engine.start_download(
                TorrentDownloadRequest {
                    owner_id: owner_id.into(),
                    info_hash: FIXTURE_HASH.into(),
                    file_idx: Some(idx as u32),
                    requested_name: owner_id.into(),
                    output_directory: inner.dir.clone(),
                    cancel: Arc::new(AtomicBool::new(false)),
                },
                |_| {},
                |_| {},
                move |result| {
                    tx.send(result).unwrap();
                },
            );
            let result = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            assert_eq!(std::fs::read(result.path).unwrap(), owner_id.as_bytes());
        }
        engine.remove_retained("a").unwrap();
        assert!(dir.join("b.mkv").exists());
        engine.remove_retained("b").unwrap();
        assert!(!dir.exists());
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    #[test]
    fn cache_origin_sibling_keeps_directory_proof_after_restart() {
        const ENV: &str = "NOVA_TORRENT_OWNER_STORAGE_TEST";
        if std::env::var_os(ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ENV, "1")
                .args([
                    "--exact",
                    "tests::cache_origin_sibling_keeps_directory_proof_after_restart",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let inner = offline_engine("cache-origin");
        nova_storage::init_at(&inner.dir.join("db"));
        let dir = inner.dir.join(FIXTURE_HASH);
        std::fs::create_dir_all(dir.join("season")).unwrap();
        std::fs::write(dir.join("season/b.mkv"), b"b").unwrap();
        let mut cached = test_entry(dir.clone(), false, Instant::now());
        cached.torrent_id = UNMANAGED;
        inner
            .active
            .lock()
            .unwrap()
            .insert(FIXTURE_HASH.into(), cached);
        inner.save_tracked();
        inner.retain_download(owner("a", &dir, Some(0)));
        inner.retain_download(owner("b", &dir, Some(1)));
        inner.remove_retained("a").unwrap();
        let recovered = download_directory(FIXTURE_HASH, &inner.dir.join("downloads"), Some(&dir));
        assert_eq!(recovered, dir);
        assert!(path_is_within(&dir.join("season/b.mkv"), &recovered));
        let restarted = offline_engine("cache-restart");
        restarted.adopt_persisted();
        let engine = TorrentEngine {
            inner: Some(restarted.clone()),
        };
        engine.restore_downloads(vec![owner("b", &recovered, Some(1))]);
        engine.clear_cache();
        assert!(dir.join("season/b.mkv").exists());
        engine.remove_retained("b").unwrap();
        assert!(!dir.exists());
        // The database remains open until this isolated child exits.
        std::fs::remove_dir_all(&restarted.dir).unwrap();
    }

    #[test]
    fn unresolved_requested_index_does_not_poison_selected_files() {
        let inner = offline_engine("bad-index");
        let dir = inner.dir.join(FIXTURE_HASH);
        let mut invalid = owner("invalid", &dir, None);
        invalid.requested_file_idx = Some(99);
        invalid.complete = false;
        inner.retain_download(invalid);
        assert_eq!(
            inner.only_files_for(UNMANAGED, FIXTURE_HASH, 0),
            HashSet::from([0])
        );
        let request = PlaybackRequest::default();
        inner
            .activate_playback(
                request.clone(),
                FIXTURE_HASH.into(),
                "a".into(),
                TorrentSettings::default(),
                fixture_selection(0),
                || {},
            )
            .unwrap();
        request.cancel();
        inner.stop_cancelled_playback(&TorrentSettings::default());
        inner.remove_retained("invalid").unwrap();
        assert!(!dir.exists());
        std::fs::remove_dir_all(&inner.dir).unwrap();
    }

    fn files(items: &[(usize, &str, u64)]) -> Vec<(usize, String, u64)> {
        items
            .iter()
            .map(|(i, n, l)| (*i, n.to_string(), *l))
            .collect()
    }

    #[test]
    fn explicit_index_wins_when_video() {
        let f = files(&[
            (0, "readme.txt", 100),
            (1, "movie.1080p.mkv", 9000),
            (2, "sample.mp4", 50),
        ]);
        let got = pick_video_file(&f, Some(2), "").unwrap();
        assert_eq!(got.idx, 2);
        assert_eq!(got.name, "sample.mp4");
    }

    #[test]
    fn explicit_index_ignored_when_not_video() {
        let f = files(&[(0, "readme.txt", 100), (1, "movie.mkv", 9000)]);
        let got = pick_video_file(&f, Some(0), "").unwrap();
        assert_eq!(got.idx, 1);
    }

    #[test]
    fn name_match_prefers_requested_release() {
        let f = files(&[
            (0, "Some.Other.Movie.2020.mkv", 9000),
            (1, "The.Matrix.1999.1080p.mkv", 8000),
        ]);
        let got = pick_video_file(&f, None, "The.Matrix.1999.1080p").unwrap();
        assert_eq!(got.idx, 1);
    }

    #[test]
    fn token_match_when_no_substring() {
        let f = files(&[
            (0, "aaa.bbb.ccc.mkv", 100),
            (1, "matrix.1999.remux.mkv", 200),
        ]);
        let got = pick_video_file(&f, None, "the matrix 1999").unwrap();
        assert_eq!(got.idx, 1);
    }

    #[test]
    fn falls_back_to_largest_video() {
        let f = files(&[
            (0, "small.mkv", 100),
            (1, "big.mkv", 5000),
            (2, "notes.txt", 9000),
        ]);
        let got = pick_video_file(&f, None, "").unwrap();
        assert_eq!(got.idx, 1);
    }

    #[test]
    fn no_video_is_none() {
        let f = files(&[(0, "a.txt", 1), (1, "b.nfo", 2)]);
        assert!(pick_video_file(&f, None, "movie").is_none());
    }

    #[test]
    fn magnet_for_bare_hash_and_full() {
        let bare = magnet_for("abcdef");
        assert!(bare.starts_with("magnet:?xt=urn:btih:abcdef"));
        // Public trackers ride along so metadata does not depend on DHT.
        assert!(bare.contains("&tr=udp://tracker.opentrackr.org:1337/announce"));

        let full = magnet_for("magnet:?xt=urn:btih:abcdef&dn=x");
        assert!(full.starts_with("magnet:?xt=urn:btih:abcdef&dn=x"));
        assert!(full.contains("&tr=udp://tracker.opentrackr.org:1337/announce"));
    }

    #[test]
    fn no_cache_flag_maps_to_delete_decision() {
        let mut s = TorrentSettings::default();
        assert!(!should_delete_after_playback(&s));
        s.no_cache = true;
        assert!(should_delete_after_playback(&s));
    }

    #[test]
    fn cache_bytes_counts_files() {
        let dir = std::env::temp_dir().join(format!("nova-torrent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 50]).unwrap();
        assert_eq!(cache_bytes(&dir), 150);
        delete_dir(&dir);
        assert!(!dir.exists());
    }

    #[test]
    fn canonical_hash_accepts_bare_magnet_and_uppercase() {
        let bare = "abcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(canonical_hash(bare), bare);
        assert_eq!(canonical_hash(&bare.to_ascii_uppercase()), bare);
        let magnet = format!("magnet:?xt=urn:btih:{bare}&dn=Example&tr=udp://t.org:1");
        assert_eq!(canonical_hash(&magnet), bare);
        // 64-hex (v2) also works.
        let v2 = "ab".repeat(32);
        assert_eq!(canonical_hash(&v2), v2);
        // Garbage never yields an empty or path-breaking name.
        let fallback = canonical_hash("not a hash at all!!!");
        assert!(!fallback.is_empty());
        assert!(!fallback.contains('/') && !fallback.contains('\x01'));
    }

    #[test]
    fn tracked_torrent_serde_round_trip() {
        let mut map = HashMap::new();
        map.insert(
            "abc".to_string(),
            TrackedTorrent {
                dir: PathBuf::from("/tmp/x"),
                file_len: 42,
                file_idx: 3,
                last_used_secs: 99,
            },
        );
        let json = serde_json::to_string(&map).unwrap();
        let back: HashMap<String, TrackedTorrent> = serde_json::from_str(&json).unwrap();
        assert_eq!(back["abc"].file_len, 42);
        assert_eq!(back["abc"].file_idx, 3);
    }

    fn test_entry(dir: PathBuf, playing: bool, last_used: Instant) -> Entry {
        Entry {
            torrent_id: 1,
            file_idx: 0,
            dir,
            file_len: 10,
            last_used,
            playing,
            owner: None,
        }
    }

    fn test_retained(id: TorrentId, dir: PathBuf, complete: bool) -> RetainedEntry {
        RetainedEntry {
            info_hash: "hash".into(),
            torrent_id: id,
            dir,
            file_idx: Some(0),
            requested_file_idx: Some(0),
            file_len: Some(10),
            job_id: 1,
            cancel: Arc::new(AtomicBool::new(false)),
            complete,
            worker_active: !complete,
        }
    }

    #[test]
    fn retained_retry_waits_for_worker_exit_and_preserves_partial_files() {
        let dir = PathBuf::from("/retained/partial");
        let mut entry = test_retained(7, dir.clone(), false);
        entry.cancel.store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(!entry.restart(2, Some(0), cancel.clone()));
        entry.worker_finished(1);
        assert!(!EngineInner::retained_needs_running(
            "hash",
            7,
            &dir,
            &HashMap::from([("hash".to_string(), entry.clone())])
        ));
        assert!(entry.restart(2, Some(0), cancel.clone()));
        assert_eq!(entry.torrent_id, 7);
        assert_eq!(entry.dir, dir);
        assert!(Arc::ptr_eq(&entry.cancel, &cancel));
        entry.worker_finished(1);
        assert!(
            entry.worker_active,
            "stale completion cannot release the replacement"
        );
        entry.worker_finished(2);
        assert!(entry.restart(3, Some(0), Arc::new(AtomicBool::new(false))));
        entry.worker_finished(3);
        entry.complete = true;
        assert!(!entry.restart(4, Some(0), cancel));
    }

    #[test]
    fn retained_retry_uses_the_requested_index_even_after_video_fallback() {
        let mut entry = test_retained(7, PathBuf::from("/retained/fallback"), false);
        entry.requested_file_idx = Some(99);
        entry.file_idx = Some(0);
        entry.worker_finished(1);
        assert!(entry.restart(2, Some(99), Arc::new(AtomicBool::new(false))));
        assert_eq!(entry.file_idx, Some(0));
    }

    #[test]
    fn select_victim_skips_playing_kept_and_attempted() {
        let now = Instant::now();
        let old = now - Duration::from_secs(100);
        let mut active = HashMap::new();
        active.insert(
            "old".to_string(),
            test_entry(PathBuf::from("/o"), false, old),
        );
        active.insert(
            "playing".to_string(),
            test_entry(PathBuf::from("/p"), true, old),
        );
        active.insert(
            "new".to_string(),
            test_entry(PathBuf::from("/n"), false, now),
        );
        let none: HashSet<String> = HashSet::new();
        let retained: HashMap<String, RetainedEntry> = HashMap::new();
        // Oldest idle entry first.
        assert_eq!(
            EngineInner::select_victim(&active, &retained, "zzz", &none).map(|(h, _)| h),
            Some("old".to_string())
        );
        // `keep` spares the just-started torrent even when oldest.
        assert_eq!(
            EngineInner::select_victim(&active, &retained, "old", &none).map(|(h, _)| h),
            Some("new".to_string())
        );
        // Attempted entries don't spin the trim loop.
        let mut attempted = HashSet::new();
        attempted.insert("old".to_string());
        attempted.insert("new".to_string());
        assert!(EngineInner::select_victim(&active, &retained, "zzz", &attempted).is_none());
    }

    #[test]
    fn retained_transfer_is_never_an_lru_victim() {
        let now = Instant::now();
        let old = now - Duration::from_secs(100);
        let mut active = HashMap::new();
        let mut retained_entry = test_entry(PathBuf::from("/retained"), false, old);
        retained_entry.torrent_id = 7;
        active.insert("retained".to_string(), retained_entry);
        active.insert(
            "cache".to_string(),
            test_entry(PathBuf::from("/cache"), false, now),
        );
        let mut retained = HashMap::new();
        retained.insert(
            canonical_hash("retained"),
            test_retained(7, PathBuf::from("/retained"), false),
        );
        let none = HashSet::new();
        assert_eq!(
            EngineInner::select_victim(&active, &retained, "zzz", &none).map(|(hash, _)| hash),
            Some("cache".to_string())
        );
        assert!(EngineInner::retained_needs_running(
            "retained",
            7,
            Path::new("/retained"),
            &retained,
        ));
        retained
            .get_mut(&canonical_hash("retained"))
            .unwrap()
            .complete = true;
        assert!(!EngineInner::retained_needs_running(
            "retained",
            7,
            Path::new("/retained"),
            &retained,
        ));
    }

    #[test]
    fn retained_progress_uses_the_selected_file_state() {
        let stats = librqbit::TorrentStats {
            total_bytes: 30,
            file_progress: vec![5, 9],
            error: None,
            progress_bytes: 15,
            uploaded_bytes: 0,
            finished: false,
            live: None,
            state: librqbit::TorrentStatsState::Live,
        };
        assert!(!selected_file_is_complete(&stats, 1, 10));
        assert!(selected_file_is_complete(&stats, 0, 4));
        let selected = to_torrent_stats(&stats, Some((0, 10)));
        assert_eq!(selected.progress, 0.5);
        assert_eq!(selected.downloaded_bytes, 5);
        assert_eq!(selected.total_bytes, 10);
    }

    #[test]
    fn sweep_removes_only_unowned_torrent_dirs() {
        let dir = std::env::temp_dir().join(format!("nova-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let orphan_hex = "cd".repeat(20);
        let owned_hex = "ab".repeat(20);
        let owned_dir = dir.join(&owned_hex);
        let nested_owned_dir = dir.join("retained").join(&owned_hex);
        std::fs::create_dir_all(dir.join(&orphan_hex)).unwrap();
        std::fs::create_dir_all(&owned_dir).unwrap();
        std::fs::create_dir_all(&nested_owned_dir).unwrap();
        std::fs::write(dir.join(&orphan_hex).join("data.bin"), b"x").unwrap();
        // Non-hex leftovers (pre-isolation layouts) and user folders stay in
        // conservative mode but go in aggressive mode.
        std::fs::create_dir_all(dir.join("Some.Torrent.Name")).unwrap();
        std::fs::write(dir.join("dht.json"), b"{}").unwrap();
        std::fs::write(dir.join("loose.bin"), b"y").unwrap();

        let mut owned = HashSet::new();
        owned.insert(owned_dir.clone());
        owned.insert(nested_owned_dir);
        sweep_orphans(&dir, &owned, false);
        assert!(!dir.join(&orphan_hex).exists(), "orphan hex dir removed");
        assert!(owned_dir.exists(), "owned dir kept");
        assert!(
            dir.join("Some.Torrent.Name").exists(),
            "conservative: name dir kept"
        );
        assert!(dir.join("dht.json").exists(), "dht.json kept");
        assert!(
            dir.join("loose.bin").exists(),
            "conservative: loose file kept"
        );

        sweep_orphans(&dir, &owned, true);
        assert!(
            !dir.join("Some.Torrent.Name").exists(),
            "aggressive: name dir removed"
        );
        assert!(
            !dir.join("loose.bin").exists(),
            "aggressive: loose file removed"
        );
        assert!(
            dir.join("dht.json").exists(),
            "dht.json kept even when aggressive"
        );
        assert!(owned_dir.exists(), "owned dir kept even when aggressive");
        assert!(
            dir.join("retained").exists(),
            "retained descendant kept its owned parent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

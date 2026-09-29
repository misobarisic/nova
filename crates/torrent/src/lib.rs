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
}

#[derive(Clone)]
struct RetainedEntry {
    torrent_id: TorrentId,
    dir: PathBuf,
    file_idx: Option<usize>,
    file_len: Option<u64>,
    job_id: u64,
    cancel: Arc<AtomicBool>,
    complete: bool,
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
            let make_opts = |ipv4_only: bool| {
                let mut opts = SessionOptions::default();
                opts.client_name_and_version = Some("nova/0.1".to_string());
                opts.persistence = None;
                opts.dht = Some(librqbit::DhtSessionConfig {
                    persistence: Some(librqbit::dht::DhtPersistenceConfig {
                        config_filename: Some(dht_file.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                opts.ipv4_only = ipv4_only;
                opts.ratelimits = librqbit::limits::LimitsConfig {
                    download_bps: NonZeroU32::new(settings.down_limit_kbps.saturating_mul(1024)),
                    upload_bps: None,
                };
                opts.listen = Some(librqbit::ListenerOptions {
                    mode: librqbit::ListenerMode::TcpAndUtp,
                    ipv4_only,
                    ..Default::default()
                });
                opts
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
                inner.sweep_current_dir();
                Self { inner: Some(inner) }
            }
            None => Self { inner: None },
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
        let spawn_result = std::thread::Builder::new()
            .name("torrent-resolve".into())
            .spawn(move || {
                let result =
                    inner
                        .clone()
                        .resolve_blocking(info_hash, file_idx, requested_name, settings);
                cb(result);
            });
        if let Err(e) = spawn_result {
            // Spawn failed (process teardown): report rather than hang.
            eprintln!("nova torrent: could not start worker: {e}");
        }
    }

    pub fn start_download(
        &self,
        info_hash: String,
        file_idx: Option<u32>,
        requested_name: String,
        output_directory: PathBuf,
        cancel: Arc<AtomicBool>,
        on_progress: impl FnMut(TorrentStats) + Send + 'static,
        on_complete: impl FnOnce(Result<TorrentDownloadReady, String>) + Send + 'static,
    ) {
        let Some(inner) = self.inner.clone() else {
            on_complete(Err("torrent engine unavailable".into()));
            return;
        };
        let key = canonical_hash(&info_hash);
        let job_id = inner.next_download_id.fetch_add(1, Ordering::Relaxed);
        let dir = output_directory.join(&key);
        {
            let mut retained = inner.retained.lock().unwrap();
            if retained.contains_key(&key) {
                drop(retained);
                on_complete(Err("torrent download already retained".into()));
                return;
            }
            retained.insert(
                key.clone(),
                RetainedEntry {
                    torrent_id: UNMANAGED,
                    dir,
                    file_idx: file_idx.map(|idx| idx as usize),
                    file_len: None,
                    job_id,
                    cancel: cancel.clone(),
                    complete: false,
                },
            );
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
                let result = worker_inner.download_blocking(
                    info_hash,
                    file_idx,
                    requested_name,
                    output_directory,
                    worker_key.clone(),
                    job_id,
                    cancel,
                    on_progress,
                );
                deliver_download_completion(&worker_completion, result);
            });
        if let Err(e) = spawn_result {
            inner.clear_retained_job(&key, job_id);
            eprintln!("nova torrent: could not start download worker: {e}");
            deliver_download_completion(&completion, Err(format!("could not start worker: {e}")));
        }
    }

    pub fn is_retained(&self, info_hash: &str) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            inner
                .retained
                .lock()
                .unwrap()
                .contains_key(&canonical_hash(info_hash))
        })
    }

    pub fn retained_stats(&self, info_hash: &str) -> Option<TorrentStats> {
        let inner = self.inner.as_ref()?;
        let (id, file_idx, file_len) = {
            let retained = inner.retained.lock().unwrap();
            let entry = retained.get(&canonical_hash(info_hash))?;
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
        inner.remove_retained(&canonical_hash(info_hash))
    }

    /// Live stats for the torrent currently playing, if any. Cheap and
    /// non-blocking (a map lookup + librqbit's own atomics).
    pub fn stats(&self, info_hash: &str) -> TorrentStats {
        let Some(inner) = &self.inner else {
            return TorrentStats::default();
        };
        let id = {
            let active = inner.active.lock().unwrap();
            match active.get(info_hash) {
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

    /// Stop downloading everything (called when the player closes). Files are
    /// kept for replay unless `no_cache` is set, in which case every cached
    /// torrent is deleted so nothing is left on disk. Stopped torrents are
    /// unmarked as playing so later trims can evict them; without this a
    /// torrent replaced by a direct stream would stay pinned (paused but
    /// unevictable) for the rest of the session.
    pub fn on_playback_stopped(&self, settings: &TorrentSettings) {
        let Some(inner) = &self.inner else {
            return;
        };
        if settings.no_cache {
            // No-cache means nothing may remain on disk: delete every cached
            // torrent, not just the one that just stopped. This also sweeps
            // stale entries left by a stream switch or an external player.
            for entry in inner.active.lock().unwrap().values_mut() {
                entry.playing = false;
                entry.last_used = Instant::now();
            }
            inner.purge_cache(None, true);
            return;
        }
        let playing: Vec<(String, TorrentId, PathBuf)> = {
            let active = inner.active.lock().unwrap();
            active
                .iter()
                .filter(|(_, e)| e.playing)
                .map(|(h, e)| (h.clone(), e.torrent_id, e.dir.clone()))
                .collect()
        };
        let retained = inner.retained.lock().unwrap().clone();
        for (hash, id, dir) in playing {
            if !EngineInner::retained_needs_running(&hash, id, &dir, &retained) {
                inner.pause(id);
            }
            if let Some(entry) = inner.active.lock().unwrap().get_mut(&hash) {
                entry.playing = false;
                entry.last_used = Instant::now();
            }
        }
    }

    /// Delete every cached torrent (Settings → Clear torrent cache) plus any
    /// orphaned data the live map doesn't own. Torrents still playing are
    /// spared so playback isn't interrupted.
    pub fn clear_cache(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
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
        if settings.no_cache {
            // Turning "no caching" on must also clear what earlier plays left
            // behind. The torrent that is playing (if any) is deleted when it
            // stops.
            inner.purge_cache(None, false);
        }
    }
}

impl EngineInner {
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
            .filter(|entry| entry.torrent_id == id)
            .map(|entry| entry.file_idx)
            .collect()
    }

    fn only_files_for(&self, id: TorrentId, selected: usize) -> HashSet<usize> {
        let mut only = HashSet::from([selected]);
        only.extend(self.active_files_for(id));
        let retained = self.retained.lock().unwrap();
        only.extend(
            retained
                .values()
                .filter(|entry| entry.torrent_id == id)
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
                    only_files: Some(vec![selected.candidate.idx]),
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
                let only = self.only_files_for(id, selected.candidate.idx);
                let _ = self.session.update_only_files(&handle, &only).await;
                if handle.is_paused() {
                    let _ = self.session.unpause(&handle).await;
                }
                Ok((id, handle, false))
            }
            AddTorrentResponse::ListOnly(_) => Err("unexpected list-only response".into()),
        }
    }

    fn download_blocking(
        self: Arc<Self>,
        info_hash: String,
        file_idx: Option<u32>,
        requested_name: String,
        output_directory: PathBuf,
        key: String,
        job_id: u64,
        cancel: Arc<AtomicBool>,
        mut on_progress: impl FnMut(TorrentStats) + Send + 'static,
    ) -> Result<TorrentDownloadReady, String> {
        if cancel.load(Ordering::Acquire) {
            self.clear_retained_job(&key, job_id);
            return Err("torrent download cancelled".into());
        }
        let output_dir = output_directory.join(&key);
        let selected = match self.rt.block_on(self.select_video(
            &info_hash,
            file_idx,
            &requested_name,
            &output_dir,
        )) {
            Ok(selected) => selected,
            Err(e) => {
                self.clear_retained_job(&key, job_id);
                return Err(e);
            }
        };
        if cancel.load(Ordering::Acquire) {
            self.clear_retained_job(&key, job_id);
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
                    self.clear_retained_job(&key, job_id);
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
                if !self.mark_retained_complete(&key, job_id) {
                    return Err("torrent download was removed".into());
                }
                if !self.has_playing_transfer(id, &dir) {
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

    fn clear_retained_job(&self, key: &str, job_id: u64) {
        let mut retained = self.retained.lock().unwrap();
        if retained
            .get(key)
            .is_some_and(|entry| entry.job_id == job_id)
        {
            retained.remove(key);
        }
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
        if !self.retained_job_is_current(key, job_id) {
            return;
        }
        let dir = self
            .retained
            .lock()
            .unwrap()
            .get(key)
            .map(|entry| entry.dir.clone());
        if !dir
            .as_deref()
            .is_some_and(|dir| self.has_playing_transfer(id, dir))
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
        info_hash: String,
        file_idx: Option<u32>,
        requested_name: String,
        settings: TorrentSettings,
    ) -> Result<StreamReady, String> {
        let subdir = self.dir.join(canonical_hash(&info_hash));
        let selected =
            self.rt
                .block_on(self.select_video(&info_hash, file_idx, &requested_name, &subdir))?;
        let picked = selected.candidate.clone();
        let (id, handle, _) = self
            .rt
            .block_on(self.add_selected_torrent(&info_hash, &subdir, selected))?;

        // 3b. Wait for storage initialization. The HTTP stream endpoint
        // rejects requests while the torrent is still Initializing
        // ("with_storage_and_file: invalid state: initializing"), and mpv
        // issues its first Range request as soon as the player opens, so
        // returning the URL too early would fail playback.
        self.rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(30), handle.wait_until_initialized())
                .await;
        });

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
            });
            entry.torrent_id = id;
            entry.file_idx = picked.idx;
            entry.dir = dir.clone();
            entry.file_len = picked.len;
            entry.last_used = Instant::now();
            entry.playing = true;
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
                    hash.clone(),
                    Entry {
                        torrent_id: UNMANAGED,
                        file_idx: t.file_idx,
                        dir: t.dir.clone(),
                        file_len: t.file_len,
                        last_used: Instant::now(),
                        playing: false,
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
        let _ = self.rt.block_on(async move {
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
        retained.iter().any(|(retained_hash, retained_entry)| {
            retained_hash == &key
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
        retained.iter().any(|(retained_hash, entry)| {
            !entry.complete
                && (retained_hash == &key
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
            entry.playing && Self::transfers_match(entry.torrent_id, &entry.dir, id, dir)
        })
    }

    fn remove_retained(&self, key: &str) -> Result<bool, String> {
        let entry = self.retained.lock().unwrap().remove(key);
        let Some(mut entry) = entry else {
            return Ok(false);
        };
        entry.cancel.store(true, Ordering::Release);
        let active_share = self.active.lock().unwrap().values().any(|active| {
            Self::transfers_match(active.torrent_id, &active.dir, entry.torrent_id, &entry.dir)
        });
        if active_share {
            if entry.torrent_id != UNMANAGED {
                let only = self.active_files_for(entry.torrent_id);
                if !only.is_empty() {
                    let session = self.session.clone();
                    let handle = session.get(TorrentIdOrHash::Id(entry.torrent_id));
                    if let Some(handle) = handle {
                        let _ = self.rt.block_on(async move {
                            session.update_only_files(&handle, &only).await
                        });
                    }
                }
            }
            if !self.has_playing_transfer(entry.torrent_id, &entry.dir) {
                self.pause(entry.torrent_id);
            }
            return Ok(true);
        }
        let retained_share = self.retained.lock().unwrap().values().any(|retained| {
            Self::transfers_match(
                retained.torrent_id,
                &retained.dir,
                entry.torrent_id,
                &entry.dir,
            )
        });
        if retained_share {
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
            eprintln!("nova torrent: removing orphaned cache dir {}", path.display());
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
    *ENGINE.lock().unwrap() = None;
}

/// The installed engine, if any.
pub fn engine() -> Option<Arc<TorrentEngine>> {
    ENGINE.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    fn test_retained(id: TorrentId, dir: PathBuf, complete: bool) -> RetainedEntry {
        RetainedEntry {
            torrent_id: id,
            dir,
            file_idx: Some(0),
            file_len: Some(10),
            job_id: 1,
            cancel: Arc::new(AtomicBool::new(false)),
            complete,
        }
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

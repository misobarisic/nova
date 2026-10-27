//! Nova's bounded streaming store. Admission happens before any peer requests a
//! piece. Eviction is driven by the live tracker while its state lock is held.
use super::{StorageFactory, StorageFactoryExt, TorrentStorage};
use crate::{ManagedTorrentShared, TorrentMetadata, type_aliases::FileInfos};
use anyhow::{Context, ensure};
use librqbit_core::lengths::{Lengths, ValidPieceIndex};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

/// Shared by every RAM session, including readers which outlive playback.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: AtomicU64,
    used: AtomicU64,
    sessions: AtomicU64,
    accounting: Mutex<()>,
}
impl MemoryBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit: AtomicU64::new(limit),
            used: AtomicU64::new(0),
            sessions: AtomicU64::new(0),
            accounting: Mutex::new(()),
        })
    }
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }
    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }
    // Reductions take effect for subsequent admissions. Existing readers retain
    // their old reservation until safely released; no allocation is overcommitted.
    pub fn set_limit(&self, limit: u64) {
        let _accounting = self.accounting.lock();
        self.limit.store(limit, Ordering::Release);
    }
    pub fn reserve_transport(self: &Arc<Self>) -> anyhow::Result<Arc<MemoryReservation>> {
        let len = 4 * 1024 * 1024;
        ensure!(
            self.reserve(len),
            "torrent buffering RAM is occupied by an existing reader; close that stream or increase the buffer limit"
        );
        self.sessions.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(MemoryReservation {
            budget: self.clone(),
            len,
        }))
    }
    fn reserve(&self, len: u64) -> bool {
        let _accounting = self.accounting.lock();
        let Some(next) = self.used().checked_add(len).filter(|v| *v <= self.limit()) else {
            return false;
        };
        self.used.store(next, Ordering::Release);
        true
    }
    fn release(&self, len: u64) {
        let _accounting = self.accounting.lock();
        self.used.fetch_sub(len, Ordering::AcqRel);
    }
}

pub struct MemoryReservation {
    budget: Arc<MemoryBudget>,
    len: u64,
}
impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.budget.release(self.len);
        self.budget.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default, Debug)]
pub struct MemoryTelemetry {
    pub resident: AtomicU64,
    pub target: AtomicU64,
    consumption: AtomicU64,
    stalls: AtomicU64,
    stalled: std::sync::atomic::AtomicBool,
}
impl MemoryTelemetry {
    pub fn consumption(&self, bytes_per_second: u64, stalled: bool) {
        self.consumption.store(bytes_per_second, Ordering::Relaxed);
        if stalled && !self.stalled.swap(stalled, Ordering::Relaxed) {
            self.stalls.fetch_add(1, Ordering::Relaxed);
        } else if !stalled {
            self.stalled.store(false, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub struct MemoryStorageFactory {
    pub budget: Arc<MemoryBudget>,
    pub telemetry: Arc<MemoryTelemetry>,
    pub transport: Arc<MemoryReservation>,
}
impl StorageFactory for MemoryStorageFactory {
    type Storage = MemoryStorage;
    fn create(
        &self,
        _: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        let lengths = *metadata.lengths();
        // Reader count and windows adapt to piece size, leaving at least two
        // pieces for the demanded region and a seek rather than rejecting
        // ordinary 4/16 MiB pieces at the Android/desktop defaults.
        ensure!(
            lengths.default_piece_length() as u64
                <= self.budget.limit().saturating_sub(4 * 1024 * 1024) / 2,
            "torrent pieces exceed the RAM buffer budget; increase torrent buffering RAM"
        );
        Ok(MemoryStorage {
            valid: std::sync::atomic::AtomicBool::new(true),
            inner: Arc::new(MemoryInner {
                lengths,
                files: metadata.file_infos.clone(),
                budget: self.budget.clone(),
                telemetry: self.telemetry.clone(),
                pieces: Mutex::new(HashMap::new()),
                adaptive: Mutex::new(Adaptive::default()),
            }),
        })
    }
    fn clone_box(&self) -> super::BoxStorageFactory {
        self.clone().boxed()
    }
}

struct MemoryInner {
    lengths: Lengths,
    files: FileInfos,
    budget: Arc<MemoryBudget>,
    telemetry: Arc<MemoryTelemetry>,
    pieces: Mutex<HashMap<ValidPieceIndex, Box<[u8]>>>,
    adaptive: Mutex<Adaptive>,
}
impl Drop for MemoryInner {
    fn drop(&mut self) {
        let bytes = self
            .pieces
            .get_mut()
            .values()
            .map(|v| v.len() as u64)
            .sum::<u64>();
        self.budget.release(bytes);
        self.telemetry.resident.fetch_sub(bytes, Ordering::Relaxed);
    }
}
pub struct MemoryStorage {
    inner: Arc<MemoryInner>,
    valid: std::sync::atomic::AtomicBool,
}
#[derive(Default)]
struct Adaptive {
    last: Option<Instant>,
    speed: f64,
    jitter: f64,
    stalls: u64,
}

/// Target is finite even for a below-bitrate swarm, and always leaves seek room.
pub fn adaptive_target(
    consumption: u64,
    throughput: f64,
    jitter: f64,
    stalls: u64,
    capacity: u64,
    piece: u64,
) -> u64 {
    let unhealthy = throughput > 0.0 && throughput < consumption as f64 * 1.25;
    let seconds = if unhealthy { 12.0 } else { 3.0 }
        + (jitter * 8.0).min(12.0)
        + (stalls as f64 * 2.0).min(12.0);
    let wanted = ((consumption.max(256 * 1024) as f64 * seconds) as u64).max(piece);
    wanted
        .min(capacity.saturating_mul(3) / 5)
        .max(piece.min(capacity))
}
/// Budget both piece rounding at an arbitrary HTTP byte offset and rewind.
pub(crate) fn memory_window(target: u64, capacity: u64, piece: u64, readers: u64) -> u64 {
    let readers = readers.max(1);
    let slots = capacity / piece / readers;
    let overhead = if slots >= 4 { 2 } else { 1 };
    (target / readers)
        .max(piece)
        .min(slots.saturating_sub(overhead).max(1) * piece)
}

impl MemoryStorage {
    pub(crate) fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
    pub(crate) fn capacity(&self) -> u64 {
        if !self.is_valid() {
            return 0;
        }
        let sessions = self.inner.budget.sessions.load(Ordering::Relaxed).max(1);
        self.inner
            .budget
            .limit()
            .saturating_sub(sessions * 4 * 1024 * 1024)
            / sessions
    }
    pub(crate) fn reader_limit(&self) -> usize {
        ((self.capacity() / self.inner.lengths.default_piece_length() as u64) / 4).clamp(1, 4)
            as usize
    }
    pub(crate) fn target_for(&self, readers: u64) -> u64 {
        self.window();
        memory_window(
            self.inner.telemetry.target.load(Ordering::Relaxed),
            self.capacity(),
            self.inner.lengths.default_piece_length() as u64,
            readers,
        )
    }
    pub(crate) fn window(&self) -> u64 {
        if !self.is_valid() {
            return 0;
        }
        let a = self.inner.adaptive.lock();
        let piece = self.inner.lengths.default_piece_length() as u64;
        let capacity = self.capacity();
        let target = adaptive_target(
            self.inner.telemetry.consumption.load(Ordering::Relaxed),
            a.speed,
            a.jitter,
            a.stalls,
            capacity,
            piece,
        );
        self.inner.telemetry.target.store(target, Ordering::Relaxed);
        target
    }
    pub(crate) fn resident(&self) -> Vec<ValidPieceIndex> {
        let pieces = self.inner.pieces.lock();
        if !self.is_valid() {
            return Vec::new();
        }
        pieces.keys().copied().collect()
    }
    pub(crate) fn reserve(&self, piece: ValidPieceIndex) -> bool {
        let mut pieces = self.inner.pieces.lock();
        if !self.is_valid() {
            return false;
        }
        if pieces.contains_key(&piece) {
            return true;
        }
        let len = self.inner.lengths.piece_length(piece) as u64;
        if !self.inner.budget.reserve(len) {
            return false;
        }
        pieces.insert(piece, vec![0; len as usize].into_boxed_slice());
        self.inner
            .telemetry
            .resident
            .fetch_add(len, Ordering::Relaxed);
        true
    }
    /// Caller owns the live tracker write lock and this piece's exclusive pin.
    pub(crate) fn evict(&self, piece: ValidPieceIndex) {
        let mut pieces = self.inner.pieces.lock();
        if !self.is_valid() {
            return;
        }
        if let Some(bytes) = pieces.remove(&piece) {
            self.inner.budget.release(bytes.len() as u64);
            self.inner
                .telemetry
                .resident
                .fetch_sub(bytes.len() as u64, Ordering::Relaxed);
        }
    }
    fn access(
        &self,
        file: usize,
        offset: u64,
        len: usize,
    ) -> anyhow::Result<(ValidPieceIndex, usize)> {
        ensure!(
            self.valid.load(Ordering::Acquire),
            "RAM storage handle transferred"
        );
        let fi = self.inner.files.get(file).context("invalid memory file")?;
        ensure!(
            offset.checked_add(len as u64).is_some_and(|v| v <= fi.len),
            "memory file range out of bounds"
        );
        let abs = fi.offset_in_torrent + offset;
        let plen = self.inner.lengths.default_piece_length() as u64;
        let id = self
            .inner
            .lengths
            .validate_piece_index((abs / plen).try_into()?)
            .context("invalid memory piece")?;
        Ok((id, (abs % plen) as usize))
    }
}
impl TorrentStorage for MemoryStorage {
    fn init(&mut self, _: &ManagedTorrentShared, _: &TorrentMetadata) -> anyhow::Result<()> {
        Ok(())
    }
    fn pread_exact(&self, file: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let (id, pos) = self.access(file, offset, buf.len())?;
        let pieces = self.inner.pieces.lock();
        ensure!(self.is_valid(), "RAM storage handle transferred");
        let bytes = pieces.get(&id).context("RAM piece is not resident")?;
        buf.copy_from_slice(
            bytes
                .get(pos..pos + buf.len())
                .context("read crosses RAM piece")?,
        );
        Ok(())
    }
    fn pwrite_all(&self, file: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let (id, pos) = self.access(file, offset, buf.len())?;
        let mut pieces = self.inner.pieces.lock();
        ensure!(self.is_valid(), "RAM storage handle transferred");
        let bytes = pieces
            .get_mut(&id)
            .context("RAM write without a piece reservation")?;
        bytes
            .get_mut(pos..pos + buf.len())
            .context("write crosses RAM piece")?
            .copy_from_slice(buf);
        Ok(())
    }
    fn remove_file(&self, _: usize, _: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    fn remove_directory_if_empty(&self, _: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    fn ensure_file_length(&self, _: usize, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let _pieces = self.inner.pieces.lock();
        ensure!(
            self.valid.swap(false, Ordering::AcqRel),
            "RAM storage already transferred"
        );
        Ok(Box::new(Self {
            inner: self.inner.clone(),
            valid: std::sync::atomic::AtomicBool::new(true),
        }))
    }
    fn memory(&self) -> Option<&MemoryStorage> {
        Some(self)
    }
    fn on_piece_completed(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        if !self.is_valid() {
            return Ok(());
        }
        let mut a = self.inner.adaptive.lock();
        let now = Instant::now();
        if let Some(last) = a.last {
            let rate = self.inner.lengths.piece_length(piece) as f64
                / now.duration_since(last).as_secs_f64().max(0.01);
            let delta = (rate - a.speed).abs() / rate.max(a.speed).max(1.0);
            a.jitter = a.jitter * 0.8 + delta * 0.2;
            a.speed = if a.speed == 0.0 {
                rate
            } else {
                a.speed * 0.8 + rate * 0.2
            };
        }
        a.last = Some(now);
        a.stalls = self.inner.telemetry.stalls.load(Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_admission_never_overcommits() {
        let budget = MemoryBudget::new(64);
        assert!(budget.reserve(48));
        assert!(!budget.reserve(32));
        budget.release(48);
        assert!(budget.reserve(64));
        assert!(!budget.reserve(1));
    }
    #[test]
    fn shrink_is_linearizable_with_concurrent_admission() {
        let budget = MemoryBudget::new(1024);
        let started = Arc::new(std::sync::Barrier::new(9));
        let jobs: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                let started = started.clone();
                std::thread::spawn(move || {
                    started.wait();
                    for _ in 0..1000 {
                        if budget.reserve(1) {
                            budget.release(1);
                        }
                    }
                })
            })
            .collect();
        started.wait();
        budget.set_limit(0);
        for job in jobs {
            job.join().unwrap();
        }
        assert_eq!(budget.limit(), 0);
        assert_eq!(budget.used(), 0);
        assert!(!budget.reserve(1));
        budget.set_limit(32);
        assert!(budget.reserve(32));
    }
    #[test]
    fn default_limits_accept_common_piece_sizes_and_windows_fit() {
        for (limit_mib, piece_mib) in [(64u64, 4u64), (256, 16), (32, 2)] {
            let capacity = (limit_mib - 4) * 1024 * 1024;
            let piece = piece_mib * 1024 * 1024;
            assert!(piece <= capacity / 2);
            let readers = ((capacity / piece) / 4).clamp(1, 4);
            let window = memory_window(capacity * 3 / 5, capacity, piece, readers);
            let slots = capacity / piece / readers;
            let rewind = if slots >= 4 { 1 } else { 0 };
            assert!((window.div_ceil(piece) + 1 + rewind) * readers * piece <= capacity);
        }
    }
    #[test]
    fn transfer_revokes_old_handle_and_keeps_reservations_until_last_owner() {
        let lengths = Lengths::new(4 * 16384, 16384).unwrap();
        let budget = MemoryBudget::new(65536);
        let storage = MemoryStorage {
            valid: std::sync::atomic::AtomicBool::new(true),
            inner: Arc::new(MemoryInner {
                lengths,
                files: vec![crate::file_info::FileInfo {
                    relative_filename: "video".into(),
                    offset_in_torrent: 0,
                    piece_range: 0..4,
                    attrs: Default::default(),
                    len: 4 * 16384,
                }],
                budget: budget.clone(),
                telemetry: Arc::new(MemoryTelemetry::default()),
                pieces: Mutex::new(HashMap::new()),
                adaptive: Mutex::new(Adaptive::default()),
            }),
        };
        let first = lengths.validate_piece_index(0).unwrap();
        assert!(storage.reserve(first));
        storage.pwrite_all(0, 0, &[42; 32]).unwrap();
        let new = storage.take().unwrap();
        assert!(!storage.reserve(lengths.validate_piece_index(1).unwrap()));
        storage.evict(first);
        assert_eq!(budget.used(), 16384);
        assert!(storage.pwrite_all(0, 0, &[7]).is_err());
        assert!(storage.pread_exact(0, 0, &mut [0; 1]).is_err());
        assert!(storage.take().is_err());
        let mut bytes = [0; 32];
        new.pread_exact(0, 0, &mut bytes).unwrap();
        assert_eq!(bytes, [42; 32]);
        drop(storage);
        assert_eq!(budget.used(), 16384);
        drop(new);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn adaptive_targets_fit_and_grow_after_instability() {
        let healthy = adaptive_target(
            1024 * 1024,
            10_000_000.0,
            0.0,
            0,
            64 * 1024 * 1024,
            1024 * 1024,
        );
        let unstable = adaptive_target(
            1024 * 1024,
            500_000.0,
            0.9,
            2,
            64 * 1024 * 1024,
            1024 * 1024,
        );
        assert!(unstable > healthy);
        assert!(adaptive_target(u64::MAX / 2, 1.0, 1.0, 99, 65536, 1024) <= 65536 * 3 / 5);
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use crate::{
        AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerMode, ListenerOptions,
        Session, SessionOptions, create_torrent, spawn_utils::BlockingSpawner,
    };
    use std::{io::SeekFrom, net::Ipv4Addr, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bounded_stream_seeks_evict_verified_data_and_refetch_with_parallel_probe()
    -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            let seed_dir = tempfile::tempdir()?;
            let client_root = tempfile::tempdir()?;
            let never_created = client_root.path().join("no-payload-or-state");
            let contents: Vec<_> = (0..8 * 1024 * 1024)
                .map(|n| ((n * 31 + n / 65536) % 251) as u8)
                .collect();
            let path = seed_dir.path().join("video.mkv");
            std::fs::write(&path, &contents)?;
            let torrent = create_torrent(
                &path,
                CreateTorrentOptions {
                    piece_length: Some(65536),
                    ..Default::default()
                },
                &BlockingSpawner::new(2),
            )
            .await?;
            let metainfo = torrent.as_bytes()?;
            let seed = Session::new_with_opts(
                seed_dir.path().to_owned(),
                SessionOptions {
                    dht: None,
                    persistence: None,
                    disable_trackers: true,
                    disable_local_service_discovery: true,
                    listen: Some(ListenerOptions {
                        listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                        mode: ListenerMode::TcpOnly,
                        ipv4_only: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
            let seed_handle = seed
                .add_torrent(
                    AddTorrent::from_bytes(metainfo.clone()),
                    Some(AddTorrentOptions {
                        overwrite: true,
                        output_folder: Some(seed_dir.path().to_string_lossy().into_owned()),
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .unwrap();
            seed_handle.wait_until_completed().await?;
            let budget = MemoryBudget::new(8 * 1024 * 1024);
            let telemetry = Arc::new(MemoryTelemetry::default());
            let factory = MemoryStorageFactory {
                transport: budget.reserve_transport()?,
                budget: budget.clone(),
                telemetry: telemetry.clone(),
            };
            let client = Session::new_with_opts(
                never_created.clone(),
                SessionOptions {
                    dht: None,
                    persistence: None,
                    disable_trackers: true,
                    disable_local_service_discovery: true,
                    disable_upload: true,
                    listen: None,
                    default_storage_factory: Some(factory.boxed()),
                    // A RAM reader must not monopolize the worker permit
                    // needed to receive and verify its missing pieces.
                    runtime_worker_threads: Some(1),
                    peer_limit: Some(8),
                    ..Default::default()
                },
            )
            .await?;
            // Resolve magnet metadata using only the local seeder, then hand
            // discovery peers to the payload add without a second DHT/tracker
            // round trip (Nova carries these through SelectedTorrent).
            let discovered = client
                .add_torrent(
                    AddTorrent::from_url(torrent.as_magnet().to_string()),
                    Some(AddTorrentOptions {
                        list_only: true,
                        initial_peers: Some(vec![seed.listen_addr().unwrap()]),
                        ..Default::default()
                    }),
                )
                .await?;
            let crate::AddTorrentResponse::ListOnly(discovered) = discovered else {
                anyhow::bail!("expected metadata discovery");
            };
            assert!(discovered.seen_peers.contains(&seed.listen_addr().unwrap()));
            // The same hash deliberately has independent disk and RAM handles.
            let handle = client
                .add_torrent(
                    AddTorrent::from_bytes(discovered.torrent_bytes),
                    Some(AddTorrentOptions {
                        initial_peers: Some(discovered.seen_peers),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .unwrap();
            assert!(!Arc::ptr_eq(&handle, &seed_handle));
            handle.wait_until_initialized().await?;
            assert_eq!(
                telemetry.resident.load(Ordering::Relaxed),
                0,
                "no whole-file fallback before a read"
            );
            let mut main = handle.clone().stream(0).await?;
            let mut bytes = vec![0; 65536];
            main.read_exact(&mut bytes).await?;
            assert_eq!(bytes, contents[..65536]);
            assert!(budget.used() <= budget.limit());
            // A short remote index probe must neither demand intervening media
            // nor supersede the still-open decoding reader.
            let tail = contents.len() as u64 - 65536;
            let listener = librqbit_dualstack_sockets::TcpListener::bind_tcp(
                "127.0.0.1:0".parse()?,
                librqbit_dualstack_sockets::BindOpts {
                    request_dualstack: false,
                    ..Default::default()
                },
            )?;
            let port = listener.bind_addr().port();
            let http =
                crate::http_api::HttpApi::new(crate::Api::new(client.clone(), None, None), None);
            let http_task = tokio::spawn(http.make_http_api_and_run(listener, None));
            let response = reqwest::Client::new()
                .get(format!(
                    "http://127.0.0.1:{port}/torrents/{}/stream/0",
                    handle.id()
                ))
                .header("Range", format!("bytes={tail}-{}", tail + 1023))
                .send()
                .await?;
            assert_eq!(response.status(), http::StatusCode::PARTIAL_CONTENT);
            let index = response.bytes().await?;
            assert_eq!(
                index.as_ref(),
                &contents[tail as usize..tail as usize + 1024]
            );
            main.read_exact(&mut bytes).await?;
            assert_eq!(bytes, contents[65536..131072]);
            telemetry.consumption(8 * 1024 * 1024, false);
            let missing_far_edge =
                handle.with_metadata(|m| m.lengths().validate_piece_index(40).unwrap())?;
            assert!(!handle.with_chunk_tracker(|ct| ct.is_piece_have(missing_far_edge))?);
            // Raising the target must not hold an already verified current
            // piece hostage to a newly missing speculative far edge.
            {
                let read = main.read_exact(&mut bytes);
                tokio::pin!(read);
                assert!(futures::poll!(read).is_ready());
            }
            assert_eq!(bytes, contents[131072..196608]);
            main.seek(SeekFrom::Start(6 * 1024 * 1024)).await?;
            main.read_exact(&mut bytes).await?;
            assert_eq!(bytes, contents[6 * 1024 * 1024..6 * 1024 * 1024 + 65536]);
            tokio::time::sleep(Duration::from_millis(250)).await;
            let first = handle.with_metadata(|m| m.lengths().validate_piece_index(0).unwrap())?;
            assert!(
                !handle.with_chunk_tracker(|ct| ct.is_piece_have(first))?,
                "eviction revokes verified HAVE"
            );
            assert!(budget.used() <= budget.limit());
            assert!(
                handle.stats().progress_bytes < contents.len() as u64,
                "only useful window downloaded"
            );
            main.seek(SeekFrom::Start(0)).await?;
            main.read_exact(&mut bytes).await?;
            assert_eq!(
                bytes,
                contents[..65536],
                "evicted verified bytes must be fetched again"
            );
            assert!(
                !never_created.exists(),
                "RAM session must never create its output path"
            );
            drop(main);
            http_task.abort();
            client.stop().await;
            seed.stop().await;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
}

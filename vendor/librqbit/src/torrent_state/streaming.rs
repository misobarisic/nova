use std::{
    collections::VecDeque,
    io::SeekFrom,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Poll, Waker},
    time::Instant,
};

use anyhow::Context;
use dashmap::DashMap;

use librqbit_core::lengths::{CurrentPiece, Lengths, ValidPieceIndex};
use tokio::{
    io::{AsyncRead, AsyncSeek},
    sync::OwnedSemaphorePermit,
};
use tracing::{debug, trace};

use crate::{ManagedTorrent, file_info::FileInfo, storage::TorrentStorage};

use super::{ManagedTorrentHandle, TorrentMetadata};

type StreamId = usize;

// 32 mb lookahead by default.
const PER_STREAM_BUF_DEFAULT: u64 = 32 * 1024 * 1024;

struct StreamState {
    file_id: usize,
    file_abs_offset: u64,
    position: u64,
    response_end: u64,
    waker: Option<Waker>,
}

impl StreamState {
    fn current_piece(&self, lengths: &Lengths) -> Option<CurrentPiece> {
        lengths.compute_current_piece(self.position, self.file_abs_offset)
    }

    fn queue<'a>(
        &self,
        lengths: &'a Lengths,
        window: u64,
    ) -> impl Iterator<Item = ValidPieceIndex> + use<'a> {
        let start = self.file_abs_offset + self.position;
        let end = (start.saturating_add(window)).min(self.file_abs_offset + self.response_end);
        let dpl = lengths.default_piece_length();
        let start_id = (start / dpl as u64).try_into().unwrap();
        let end_id = end.div_ceil(dpl as u64).try_into().unwrap();
        (start_id..end_id).filter_map(|i| lengths.validate_piece_index(i))
    }
}

#[derive(Default)]
pub(crate) struct TorrentStreams {
    next_stream_id: AtomicUsize,
    memory_readers: AtomicUsize,
    streams: DashMap<StreamId, StreamState>,
}

struct MemoryReadPermit(Arc<TorrentStreams>);
impl Drop for MemoryReadPermit {
    fn drop(&mut self) {
        self.0.memory_readers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TorrentStreams {
    fn reserve_reader(self: &Arc<Self>, limit: usize) -> anyhow::Result<MemoryReadPermit> {
        let mut count = self.memory_readers.load(Ordering::Acquire);
        loop {
            anyhow::ensure!(
                count < limit,
                "torrent RAM budget cannot accommodate another concurrent reader"
            );
            match self.memory_readers.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(MemoryReadPermit(self.clone())),
                Err(actual) => count = actual,
            }
        }
    }

    fn next_id(&self) -> usize {
        self.next_stream_id.fetch_add(1, Ordering::Relaxed)
    }

    fn register_waker(&self, stream_id: StreamId, waker: Waker) {
        if let Some(mut s) = self.streams.get_mut(&stream_id) {
            let vm = s.value_mut();
            vm.waker = Some(waker);
        }
    }

    // Interleave 1st, 2nd etc pieces from each active stream in turn until they get 1/10th of the file .
    pub(crate) fn iter_next_pieces<'a>(
        &'a self,
        lengths: &'a Lengths,
    ) -> impl Iterator<Item = ValidPieceIndex> + 'a {
        struct Interleave<I> {
            all: VecDeque<I>,
        }

        impl<I: Iterator<Item = ValidPieceIndex>> Iterator for Interleave<I> {
            type Item = ValidPieceIndex;

            fn next(&mut self) -> Option<Self::Item> {
                while let Some(mut it) = self.all.pop_front() {
                    if let Some(piece) = it.next() {
                        self.all.push_back(it);
                        return Some(piece);
                    }
                }
                None
            }
        }

        let mut all: Vec<_> = self
            .streams
            .iter()
            .map(|s| s.queue(lengths, PER_STREAM_BUF_DEFAULT))
            .collect();

        // Shuffle to decrease determinism and make queueing fairer.
        use rand::seq::SliceRandom;
        all.shuffle(&mut rand::rng());

        Interleave { all: all.into() }
    }

    /// Bound the combined forward windows of concurrent playback and index
    /// readers. A short HTTP probe only requests its actual response span.
    pub(crate) fn memory_demand(
        &self,
        lengths: &Lengths,
        window: u64,
        capacity: u64,
    ) -> (
        Vec<ValidPieceIndex>,
        std::collections::HashSet<ValidPieceIndex>,
    ) {
        let count = self.streams.len().max(1) as u64;
        let piece_len = lengths.default_piece_length() as u64;
        // Keep large-piece torrents pipelined before duration/bitrate is known.
        // This is scheduler prefetch only: startup may still begin at one piece.
        let prefetch = if capacity / piece_len / count >= 4 {
            window.max(2 * piece_len * count)
        } else {
            window
        };
        let window = crate::storage::memory::memory_window(prefetch, capacity, piece_len, count);
        let rewind = if capacity / piece_len / count >= 4 {
            piece_len
        } else {
            0
        };
        let mut heads = Vec::new();
        let mut forward = Vec::new();
        let mut keep = std::collections::HashSet::new();
        // Recent blocked reads first, all heads before speculative forward work.
        let mut ids: Vec<_> = self.streams.iter().map(|s| *s.key()).collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        for id in ids {
            let Some(s) = self.streams.get(&id) else {
                continue;
            };
            if s.position >= s.response_end {
                continue;
            }
            if let Some(p) = s.current_piece(lengths) {
                heads.push(p.id);
            }
            for p in s.queue(lengths, window) {
                forward.push(p);
                keep.insert(p);
            }
            let start = s.file_abs_offset + s.position.saturating_sub(rewind);
            let end = s.file_abs_offset + s.position;
            for id in start / lengths.default_piece_length() as u64
                ..=end / lengths.default_piece_length() as u64
            {
                if let Some(p) = lengths.validate_piece_index(id as u32) {
                    keep.insert(p);
                }
            }
        }
        heads.extend(forward);
        (heads, keep)
    }

    pub(crate) fn wake_all(&self) {
        for mut s in self.streams.iter_mut() {
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        }
    }

    pub(crate) fn wake_streams_on_piece_completed(
        &self,
        piece_id: ValidPieceIndex,
        lengths: &Lengths,
    ) {
        for mut w in self.streams.iter_mut() {
            if w.value().current_piece(lengths).map(|p| p.id) == Some(piece_id)
                && let Some(waker) = w.value_mut().waker.take()
            {
                debug!(
                    stream_id = *w.key(),
                    piece_id = piece_id.get(),
                    "waking stream"
                );
                waker.wake();
            }
        }
    }

    fn drop_stream(&self, stream_id: StreamId) -> Option<StreamState> {
        debug!(stream_id, "dropping stream");
        self.streams.remove(&stream_id).map(|s| s.1)
    }

    pub(crate) fn streamed_file_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.streams.iter().map(|s| s.value().file_id)
    }
}

pub struct FileStream {
    torrent: ManagedTorrentHandle,
    metadata: Arc<TorrentMetadata>,
    streams: Arc<TorrentStreams>,
    stream_id: usize,
    file_id: usize,
    position: u64,

    // file params
    file_len: u64,
    file_torrent_abs_offset: u64,
    response_end: u64,
    memory_buffering: bool,

    _blocking_permit: Option<OwnedSemaphorePermit>,
    _memory_reader: Option<MemoryReadPermit>,
}

macro_rules! map_io_err {
    ($e:expr) => {
        $e.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    };
}

macro_rules! poll_try_io {
    ($e:expr) => {{
        let e = map_io_err!($e);
        match e {
            Ok(r) => r,
            Err(e) => {
                debug!("stream error {e:#}");
                return Poll::Ready(Err(e));
            }
        }
    }};
}

impl AsyncRead for FileStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        tbuf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if tbuf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        // if the file is over, return 0
        if self.position == self.file_len {
            debug!(
                stream_id = self.stream_id,
                file_id = self.file_id,
                "stream completed, EOF"
            );
            return Poll::Ready(Ok(()));
        }

        let current = poll_try_io!(
            self.metadata
                .lengths()
                .compute_current_piece(self.position, self.file_torrent_abs_offset)
                .context("invalid position")
        );

        let memory_read = poll_try_io!(self.torrent.with_state(|state| {
            if let crate::ManagedTorrentState::Live(live) = state {
                if live.files.memory().is_some() {
                    // Bound in u64 before narrowing: large films can exceed
                    // usize on 32-bit Android even though each read is small.
                    let max = (tbuf.remaining() as u64)
                        .min(current.piece_remaining as u64)
                        .min(self.file_len - self.position) as usize;
                    return live
                        .read_memory(
                            self.file_id,
                            self.position,
                            current.id,
                            &mut tbuf.initialize_unfilled()[..max],
                            self.file_torrent_abs_offset,
                            self.response_end,
                            self.streams.streams.len().max(1) as u64,
                            self.memory_buffering,
                        )
                        .map(|(ready, buffering)| Some((ready, max, buffering)));
                }
            }
            Ok(None)
        }));
        if let Some((ready, len, buffering)) = memory_read {
            self.memory_buffering = buffering;
            if !ready {
                self.streams
                    .register_waker(self.stream_id, cx.waker().clone());
                return Poll::Pending;
            }
            self.as_mut().advance(len as u64);
            tbuf.advance(len);
            return Poll::Ready(Ok(()));
        }

        // if the piece is not there, register to wake when it is
        // check if we have the piece for real
        let have = poll_try_io!(self.torrent.with_chunk_tracker(|ct| {
            let have = ct.get_have_pieces().as_slice()[current.id.get() as usize];
            if !have {
                self.streams
                    .register_waker(self.stream_id, cx.waker().clone());
            }
            have
        }));
        if !have {
            debug!(stream_id = self.stream_id, file_id = self.file_id, piece_id = %current.id, "poll pending, not have");
            return Poll::Pending;
        }

        // actually stream the piece
        let buf = tbuf.initialize_unfilled();
        let file_remaining = self.file_len - self.position;
        let bytes_to_read: usize = poll_try_io!(
            (buf.len() as u64)
                .min(current.piece_remaining as u64)
                .min(file_remaining)
                .try_into()
        );

        let buf = &mut buf[..bytes_to_read];

        let start = Instant::now();
        poll_try_io!(poll_try_io!(self.torrent.shared.spawner.block_in_place(
            || {
                self.torrent.with_storage_and_file(
                    self.file_id,
                    |files, _fi| {
                        files.pread_exact(self.file_id, self.position, buf)?;
                        Ok::<_, anyhow::Error>(())
                    },
                    &self.metadata,
                )
            }
        )));

        trace!(
            buflen = buf.len(),
            stream_id = self.stream_id,
            file_id = self.file_id,
            read_time = ?start.elapsed(),
            "will write bytes"
        );

        self.as_mut().advance(bytes_to_read as u64);
        tbuf.advance(bytes_to_read);

        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for FileStream {
    fn start_seek(
        mut self: std::pin::Pin<&mut Self>,
        position: std::io::SeekFrom,
    ) -> std::io::Result<()> {
        let end_i64 = map_io_err!(TryInto::<i64>::try_into(self.file_len))?;
        let new_pos: i64 = match position {
            SeekFrom::Start(s) => map_io_err!(s.try_into())?,
            SeekFrom::End(e) => map_io_err!(TryInto::<i64>::try_into(self.file_len))? + e,
            SeekFrom::Current(o) => map_io_err!(TryInto::<i64>::try_into(self.position))? + o,
        };

        if new_pos < 0 || new_pos > end_i64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                anyhow::anyhow!("invalid seek"),
            ));
        }

        self.memory_buffering = true;
        self.as_mut().set_position(map_io_err!(new_pos.try_into())?);
        debug!(stream_id = self.stream_id, position = self.position, "seek");
        Ok(())
    }

    fn poll_complete(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        Poll::Ready(Ok(self.position))
    }
}

impl Drop for FileStream {
    fn drop(&mut self) {
        self.streams.drop_stream(self.stream_id);
    }
}

impl ManagedTorrent {
    fn with_storage_and_file<F, R>(
        &self,
        file_id: usize,
        f: F,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<R>
    where
        F: FnOnce(&dyn TorrentStorage, &FileInfo) -> R,
    {
        self.with_state(|s| {
            let files = match s {
                crate::ManagedTorrentState::Paused(p) => &*p.files,
                crate::ManagedTorrentState::Live(l) => &*l.files,
                s => anyhow::bail!("with_storage_and_file: invalid state: {}", s.name()),
            };
            let fi = metadata.file_infos.get(file_id).context("invalid file")?;
            Ok(f(files, fi))
        })
    }

    fn streams(&self) -> anyhow::Result<Arc<TorrentStreams>> {
        self.with_state(|s| match s {
            crate::ManagedTorrentState::Paused(p) => Ok(p.streams.clone()),
            crate::ManagedTorrentState::Live(l) => Ok(l.streams.clone()),
            s => anyhow::bail!("streams: invalid state {}", s.name()),
        })
    }

    fn maybe_reconnect_needed_peers_for_file(&self, file_id: usize) -> bool {
        // If we have the full file, don't bother.
        if self.is_file_finished(file_id) {
            return false;
        }
        self.with_state(|state| {
            if let crate::ManagedTorrentState::Live(l) = &state {
                l.reconnect_all_not_needed_peers();
            }
        });
        true
    }

    fn is_file_finished(&self, file_id: usize) -> bool {
        let metadata = self.metadata.load();
        let metadata = match metadata.as_ref() {
            Some(r) => r,
            None => return false,
        };
        // TODO: would be nice to remove locking
        self.with_chunk_tracker(|ct| ct.is_file_finished(&metadata.file_infos[file_id]))
            .unwrap_or(false)
    }

    pub async fn stream(self: Arc<Self>, file_id: usize) -> anyhow::Result<FileStream> {
        let metadata = self
            .metadata
            .load_full()
            .context("torrent metadata is not resolved")?;
        let (fd_len, fd_offset) = self.with_storage_and_file(
            file_id,
            |_fd, fi| (fi.len, fi.offset_in_torrent),
            &metadata,
        )?;
        let streams = self.streams()?;
        let memory_reader = self.with_storage_and_file(
            file_id,
            |storage, _| {
                storage
                    .memory()
                    .map(|m| streams.reserve_reader(m.reader_limit()))
                    .transpose()
            },
            &metadata,
        )??;
        // RAM copies do no blocking filesystem work. Retaining filesystem
        // permits here can starve the peer writes needed by these readers.
        let blocking_permit = if memory_reader.is_some() {
            None
        } else {
            Some(self.shared().spawner.semaphore().acquire_owned().await?)
        };
        let s = FileStream {
            stream_id: streams.next_id(),
            streams: streams.clone(),
            file_id,
            position: 0,

            file_len: fd_len,
            file_torrent_abs_offset: fd_offset,
            response_end: fd_len,
            memory_buffering: true,
            _blocking_permit: blocking_permit,
            _memory_reader: memory_reader,
            torrent: self,
            metadata,
        };
        s.torrent.maybe_reconnect_needed_peers_for_file(file_id);
        streams.streams.insert(
            s.stream_id,
            StreamState {
                file_id,
                position: 0,
                response_end: fd_len,
                waker: None,
                file_abs_offset: fd_offset,
            },
        );

        debug!(stream_id = s.stream_id, file_id, "started stream");

        Ok(s)
    }
}

impl FileStream {
    pub fn set_read_end(&mut self, end: u64) {
        self.response_end = end.min(self.file_len);
        if let Some(mut state) = self.streams.streams.get_mut(&self.stream_id) {
            state.response_end = self.response_end;
        }
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    fn advance(&mut self, diff: u64) {
        self.set_position(self.position + diff)
    }

    fn set_position(&mut self, new_pos: u64) {
        self.position = new_pos;
        self.streams
            .streams
            .get_mut(&self.stream_id)
            .unwrap()
            .value_mut()
            .position = new_pos;
    }

    pub fn len(&self) -> u64 {
        self.file_len
    }
}

#[cfg(test)]
mod memory_admission_tests {
    use super::*;
    #[test]
    fn concurrent_reader_admission_is_atomic_and_released() {
        let streams = Arc::new(TorrentStreams::default());
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let jobs: Vec<_> = (0..16)
            .map(|_| {
                let streams = streams.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    streams.reserve_reader(3).ok()
                })
            })
            .collect();
        let admitted: Vec<_> = jobs
            .into_iter()
            .filter_map(|job| job.join().unwrap())
            .collect();
        assert_eq!(admitted.len(), 3);
        assert!(streams.reserve_reader(3).is_err());
        drop(admitted);
        assert_eq!(streams.memory_readers.load(Ordering::Relaxed), 0);
        assert!(streams.reserve_reader(3).is_ok());
    }
}

#[cfg(test)]
mod memory_demand_tests {
    use super::*;

    #[test]
    fn common_large_pieces_prefetch_more_than_one_piece_without_raising_startup_target() {
        for (limit_mib, piece_mib) in [(64u64, 4u32), (256, 16)] {
            let piece_len = piece_mib * 1024 * 1024;
            let lengths = Lengths::new(32 * piece_len as u64, piece_len).unwrap();
            let streams = TorrentStreams::default();
            streams.streams.insert(
                0,
                StreamState {
                    file_id: 0,
                    file_abs_offset: 0,
                    position: 0,
                    response_end: 32 * piece_len as u64,
                    waker: None,
                },
            );
            let capacity = (limit_mib - 4) * 1024 * 1024;
            let (wanted, keep) = streams.memory_demand(&lengths, piece_len as u64, capacity);
            let unique: std::collections::HashSet<_> = wanted.iter().copied().collect();
            assert!(unique.len() >= 2);
            assert!(keep.len() as u64 * piece_len as u64 <= capacity);
        }
    }
    #[test]
    fn probes_are_bounded_and_a_seek_supersedes_old_byte_demand() {
        let lengths = Lengths::new(128 * 16384, 16384).unwrap();
        let streams = TorrentStreams::default();
        streams.streams.insert(
            0,
            StreamState {
                file_id: 0,
                file_abs_offset: 0,
                position: 0,
                response_end: 128 * 16384,
                waker: None,
            },
        );
        streams.streams.insert(
            1,
            StreamState {
                file_id: 0,
                file_abs_offset: 0,
                position: 127 * 16384,
                response_end: 127 * 16384 + 1024,
                waker: None,
            },
        );
        let (wanted, _) = streams.memory_demand(&lengths, 8 * 16384, 16 * 16384);
        assert_eq!(
            wanted[0].get(),
            127,
            "blocked probe precedes speculative prefetch"
        );
        assert_eq!(wanted[1].get(), 0, "main reader remains a critical demand");
        assert!(
            !wanted.iter().any(|p| p.get() == 64),
            "no downloading media between header and index"
        );
        streams.streams.get_mut(&0).unwrap().position = 64 * 16384;
        streams.drop_stream(1);
        let (wanted, keep) = streams.memory_demand(&lengths, 8 * 16384, 16 * 16384);
        assert_eq!(wanted[0].get(), 64);
        assert!(!keep.iter().any(|p| p.get() == 0 || p.get() == 127));
        assert!(
            keep.iter().any(|p| p.get() == 63),
            "small rewind cache survives forward progress"
        );
    }
}

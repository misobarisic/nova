# Nova bounded torrent streaming patch

Upstream: librqbit 9.0.1, https://github.com/ikatson/rqbit, registry source
published as `librqbit` 9.0.1 (Apache-2.0; see LICENSE-APACHE). The normalized
registry manifest is retained. The root Cargo manifest pins this directory;
other rqbit packages, including Nova's separate upstream uTP fix, stay unchanged.

`src/storage/memory.rs` implements heap-only piece storage with shared admission
accounting across sessions and outstanding read leases. Reservations cover full
pieces before any block is requested, including partially downloaded pieces.
Zero-length I/O is a no-op even at an unreserved piece boundary or aligned EOF;
the default vectored writer includes empty trailing slices there.
Four MiB per session is reserved for bounded peer/hash/HTTP staging; no payload,
metainfo, resume bitmap, or DHT state is persisted by these sessions. The budget
is not total process RSS (metadata, decoder surfaces and platform networking
state are separate). Heap pages may be swapped by the operating system.

`src/storage/mod.rs` adds an optional memory-store hook (default None), leaving
filesystem storage/middleware behavior unchanged. `torrent_state/initializing.rs`
skips the meaningless filesystem checksum scan of an empty RAM store.

`piece_tracker.rs`, `chunk_tracker.rs`, and `torrent_state/live/mod.rs` add
stream-demand-only acquisition with admission before scheduling, slow-owner
recovery, obsolete request cancellation, and coherent eviction. Eviction holds
the tracker write lock and a nonblocking exclusive per-piece pin, revokes HAVE,
chunk, queue, per-file byte and live completion counters, then releases storage.
Writes and SHA validation retain a shared per-piece pin through completion.
HTTP copies hold the tracker read lock from readiness check through copying.
RAM sessions disable uploads and HAVE advertisement: v1 HAVE cannot be retracted
when a piece is evicted. Normal disk torrents retain normal uploading behavior.

`torrent_state/streaming.rs` partitions forward demand across bounded concurrent
readers and retains a small rewind span. `http_api/handlers/streaming.rs` passes
the exact response end into demand scheduling so short container/index probes
never require downloading the gap to a resumed position. Byte reads drive demand;
playback duration/bitrate only tune the bounded adaptive target. Tests alongside
these modules exercise budget admission, adaptive clamping, seek/refetch,
concurrent probes, and storage transfer lifetime.

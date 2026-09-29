# Cross-device sync — follow-on ideas

Deferred UX and behaviour improvements for the iroh-based sync feature. Nothing
here is implemented; this file is a parking lot for later discussion. The
shipped feature covers: opt-in sync, manual peer ids, an invite ticket with
optional pairing confirmation, QR display of the ticket, an Android camera QR
scanner that joins a scanned invite automatically, last-write-wins record merge,
mesh peer propagation (pairing one device introduces it to the others, names
included, with removals and a pairing fan-out), bounded-parallel syncing with
per-peer backoff and trigger coalescing, and synced addon labels (`$NAME ($URL)`
for duplicates, identical on every device).

Legend: **effort** (S/M/L), **risk**, **value**.

## Pairing & onboarding

- **mDNS / LAN discovery (M–L / medium / medium).** Publish and browse
  `_sl-sync._udp.local` (`mdns-sd`), list nearby devices, pair with the normal
  invite/confirm. Removes id entry entirely when co-located. Android needs
  `CHANGE_WIFI_MULTICAST_STATE` + a `MulticastLock`, and multicast is historically
  flaky; consider wiring it as an iroh `address_lookup` so LAN peers dial
  directly and skip relays. Gate all of it behind the existing
  `SyncSettings.enable_local_discovery` kill-switch (Settings → Sync), which
  already disables UPnP/portmapper probing for faulty Wi-Fi.
- **Option B: one-sided id + accept tap (S).** The joiner pastes only the
  target's endpoint id and connects; the target taps Accept. No bearer ticket,
  but it requires both devices online and the target attending the prompt. Kept
  as an alternative flavour of the confirmation flow.
- **Short numeric code + rendezvous (L / medium / medium).** Both devices enter a
  6-digit code and meet on an iroh-gossip topic derived from it, so no id is
  shared. Needs PAKE or strict rate-limiting (1e6 space) plus `iroh-gossip`.
- **Device name editing polish (S / low / low).** Auto-detected names
  (Android `Build.MODEL`, desktop hostname) with an override field already ship.
  Could add a rename UI per peer and show names everywhere ids appear.
- **Invite management (S / low / medium).** Countdown until expiry, revoke a
  specific outstanding invite, and "share via…" intents on Android.
- **Pairing history / re-pair (S / low / low).** Show when a device was paired
  and offer a guided re-pair if the identity changed (storage reset).

## Sync behaviour & propagation

- **Debounced push-on-change (S–M / low / high).** After a local write, sync a
  few seconds after edits stop instead of waiting for the interval. **Important
  constraint raised during design:** while *playback* is active, progress writes
  fire roughly every 30 s, so the debounce must be playback-aware — use a
  multi-minute window (or suppress push entirely) during playback and a short
  window otherwise. Not implemented yet on purpose.
- **Configurable interval (done).** `SyncSettings.interval_secs` drives the
  app-running poll loop; Settings → Sync offers 30 s / 1 / 5 / 15 min presets
  (nearest-preset display for hand-edited values). While the app is alive but
  in the background (or kept alive by a download's foreground service) the loop
  clamps to a 5-minute floor via `nova_sync::set_foreground`, so a long download
  does not poll every 30 s. Closed-app cadence is the separate fixed job below,
  not this setting.
- **Multi-peer fan-out (done).** Syncs run bounded-parallel (4 at a time) with
  per-peer connect/exchange timeouts and per-peer backoff, and a pass repeats
  (bounded) while it keeps discovering new peers, so a freshly paired device
  reaches the whole mesh in one wake-up. Pairing and peer add/remove wake the
  worker directly. Remaining idea: tune the concurrency/timeouts per platform.
- **Per-peer status (done: last-seen).** Connections are cached for reuse but
  not held open for liveness, so true online presence isn't available. Each peer row shows "Last connected
  …": the engine publishes   `{observer}\x01{peer}` sightings into the mesh
  `presence` domain after every completed exchange (throttled to 15 min),
  and readers take the max across observers plus the local ack clock.
  Remaining ideas: last-error / record counts per peer (local-only).
- **Background sync on mobile (downloads done; periodic sync done).** Stream
  downloads keep the process alive with a work-only `dataSync` foreground
  service while a transfer is active (with a partial wake lock + notification),
  and a periodic `JobScheduler` job wakes the process roughly every 15 minutes
  to run one bounded sync pass with no Activity present. Note: backgrounded-but-
  alive already worked — the tokio `interval_loop` is process-bound and keeps
  firing passes; the job covers process death and Doze. Android 15 caps the
  `dataSync` type at 6 h/24 h, and deep Doze defers jobs to maintenance windows,
  so periodic sync is opportunistic rather than always-on. Remaining idea:
  continuous inbound reachability (a peer reaching a sleeping phone on demand)
  would need a permanent `specialUse` foreground service or FCM + a server, both
  deliberately out of scope; the mesh converges because passes are bidirectional
  and each device dials on its own schedule.
- **Always-on hub device (M / medium / low).** Designate a desktop as the
  rendezvous point so two phones don't need to be online simultaneously.
- **Selective sync per peer (M / low / low).** Choose which domains (library,
  progress, addons, settings) sync to a given peer.

## Merge correctness & conflict handling

- **Settings granularity (done).** Settings now sync one record per independent
  field, so concurrent edits to different settings union instead of racing as a
  single whole-settings blob. The image-cache controls (`cache_images`,
  `enabled`, `format`, `quality`, `downscale`, `lazy_reencode`, `lru_cache_mb`)
  are deliberately coupled into one `settings/cache` record with whole-value
  LWW, because they describe a single encoding configuration. `categories`
  remain their own per-name domain; `android_hwdec` and `rewrite_existing` are
  never synced.
- **Hybrid logical clocks (done).** Record versions now carry an HLC
  (`physical_ms`, `counter`) instead of a raw wall-clock second, so a causally
  later write always sorts after what it observed even across clock skew or a
  backward clock jump, and same-millisecond writes tie-break deterministically.
  Applied remote versions are folded in with a +1h forward-drift cap so a peer
  with a wild clock cannot drag ours into the far future. Per-peer ack clocks
  are HLC snapshots, making tombstone GC causal rather than wall-clock.
- **Binary wire format (done).** The sync and pairing frames use `postcard`
  instead of JSON (pairing ALPN `/2`; sync later bumped to `/3` for digest
  hashes and compressed frames, see "Wire & storage efficiency"). postcard is
  not self-describing, so wire structs carry no `#[serde(default)]` and any
  schema change requires an ALPN bump. Local storage stays JSON, with a one-time
  seconds→ms/HLC migration on load.
- **Exchange lifecycle hardening (done).** Records are streamed while the peer's
  records are read concurrently (no flow-control stall on a large first sync);
  the close handshake uses the send stream's end (each side finishes only after
  applying) rather than an extra frame, so a peer that goes away can't wedge the
  exchange. The writer task is aborted on any early return, the whole pass is
  bounded (`MAX_PASS_SECS`) and the `syncing` flag self-expires, so the UI can't
  get stuck on "Syncing…".
- **Progress conflict semantics (done).** Per-episode records merge field-wise
  in `merge_progress`, not plain LWW: an explicit unwatch (zeroed position +
  fresh `unwatched_at_secs`) wins wholesale, otherwise `watched` sticks with
  the watched side's position/duration, both-unwatched falls back to recency
  (deliberate rewinds survive), `play_count` takes the max. Converged records
  are written back so the whole mesh agrees. A stale unwatched record can no
  longer clobber a watched one; explicit unwatch still propagates because
  setting watched always clears the intent.
- **Library ordering (S / low / low).** `added_at_secs` plus id gives a global
  order; consider a user-draggable order synced as an explicit rank field.
- **Tombstone GC (done, per-peer ack gated).** Deletions are retained until
  every *current* peer has completed a sync since the tombstone was created
  (`sync:peer_acks`, local only), then reclaimed once past the 30-day TTL. So a
  device that was offline for any length still learns a deletion instead of
  resurrecting the record. Caveats: a peer that never returns keeps its
  tombstones alive until it is removed in Settings → Sync; acks and tombstone
  timestamps are wall-clock, so heavy clock skew can only delay collection (a
  lagging deleter's clock could, in theory, drop one early); with no peers the
  plain TTL applies. Mixed-version peers simply never ack, so upgraded devices
  stay conservative until they do.
- **Per-peer ack status UI (S / low / low).** Surface each peer's last synced /
  awaiting-ack state in Settings → Sync so the "remove a dead peer to unblock
  GC" caveat above is discoverable.

## Wire & storage efficiency

- **Digest short-circuit (done).** Each side hashes its per-domain version maps
  (blake3, 128-bit) into `Hello`; domains whose hashes agree are skipped, so an
  unchanged pass sends no version data. A changed domain sends a `Wire::Digest`
  carrying only that domain's version map (sync ALPN bumped to `/3`). Note: the
  `peers` domain is inherently asymmetric (each device stores every peer but
  itself), so it always differs and is always exchanged — it is tiny.
- **Compressed frames (done).** Sync frames carry a codec byte and are
  deflate-compressed above 256 bytes (`flate2`/miniz_oxide, pure Rust, Android
  safe); pairing/removal keep raw postcard so their wire schema is untouched.
- **Connection reuse (done).** The dialer keeps one iroh connection per device
  and opens a fresh bi stream per pass; the accept handler serves streams for
  the connection's lifetime. This drops a QUIC handshake per peer per pass and
  makes the digest round-trip cheap. A closed/failed connection is evicted and
  redialed; `notify_network_change` clears the cache.
- **Per-record sync storage (done).** `sync:records` (one JSON blob rewritten on
  every change) migrated to one redb row per record
  (`srec:{len}:{domain}{key}`) plus a clock row, written in a single batched
  transaction per save; the old blob is read once and removed. Local app state
  (library/addons/progress/settings) stays whole-value JSON — written rarely and
  read wholesale.
- **Merkle prefix trie / set reconciliation (L / medium / low, deferred).** A
  Merkle tree over `(key -> version)` would shrink a *changed* pass's digest from
  the whole changed domain to O(log n + changes), instead of shipping that
  domain's full version map (compressed) once the per-domain hash mismatches.
  Two reasons it was not built: (1) a naive balanced binary Merkle tree reshapes
  on every insert, so it must be an insertion-stable **prefix trie / Merkle
  search tree** to keep incremental hashing; (2) the win is bounded — the digest
  is already small for a personal library, and per-domain hashes + deflate cover
  the common idle case. Decide with measurements first (`NOVA_SYNC_DEBUG` logs
  digest/record bytes and changed-vs-skipped passes): build it only if
  changed-pass digests prove material (e.g. a very large watch history over
  cellular). For small symmetric differences, IBLT set reconciliation is the
  textbook tool, with a fallback to the full digest when the diff is large.

## Security

- **Pairing brute-force hardening (S / low / low).** The invite secret is
  128-bit and single-use, so brute force is infeasible; still add attempt
  counting and a concurrent-prompt cap for the confirmation path.
- **Identity rotation (M / medium / low).** Re-key the endpoint and re-pair if
  the identity or store is suspected compromised.
- **Encrypted sync state at rest (M / medium / low).** The record store and
  identity live in app-private storage in plaintext; consider platform keystores.

## Diagnostics & UI polish

- **Sync activity view (M / low / medium).** A per-domain record/byte summary
  and a short recent-events log for troubleshooting.
- **Transfer stats (S / low / low).** Show records exchanged and duration after
  each sync.
- **Pairing confirmation sheet (S / low / low).** The inline confirmation card
  could become a bottom sheet reusing `menusheet.slint`, with a large match code.
- **Android network-change hook (done).** `Endpoint::network_change()` is fed by
  a `ConnectivityManager` poll because netwatch's Android route monitor is a
  no-op. Revisit if iroh/Android gains native change callbacks.
- **QR for the manual id (S / low / low).** Also render the raw endpoint id as a
  QR for the advanced add-by-id flow.

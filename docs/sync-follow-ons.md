# Cross-device sync — follow-on ideas

Deferred UX and behavior improvements for iroh-based sync. Implemented behavior
is documented in [the project map](project-structure.md)
and [the hardening contracts](sync-hardening-plan.md). This file contains remaining
ideas, rather than a second inventory of shipped features.

Legend: **effort** (S/M/L), **risk**, **value**.

## Pairing & onboarding

- **mDNS / LAN discovery (M–L / medium / medium).** Publish and browse
  `_nova-sync._udp.local` (`mdns-sd`), list nearby devices, pair with the normal
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

- **Concurrency tuning (S / medium / low).** Measure platform-specific pass
  concurrency and timeouts before changing the current bounded fan-out.
- **Debounced push-on-change (S–M / low / high).** After a local write, sync a
  few seconds after edits stop instead of waiting for the interval. **Important
  constraint raised during design:** while *playback* is active, progress writes
  fire roughly every 30 s, so the debounce must be playback-aware — use a
  multi-minute window (or suppress push entirely) during playback and a short
  window otherwise. Not implemented yet on purpose.
- **Always-on hub device (M / medium / low).** Designate a desktop as the
  rendezvous point so two phones don't need to be online simultaneously.
- **Selective sync per peer (M / low / low).** Choose which domains (library,
  progress, addons, settings) sync to a given peer.

## Merge correctness & conflict handling

- **Library ordering (S / low / low).** `added_at_secs` plus id gives a global
  order; consider a user-draggable order synced as an explicit rank field.
- **Per-peer ack status UI (S / low / low).** Surface each peer's last synced /
  awaiting-ack state in Settings → Sync, identifying offline peers that prevent
  tombstone collection until they sync or are explicitly removed.

## Wire & storage efficiency

- **Merkle prefix trie / set reconciliation (L / medium / low, deferred).** A
  Merkle tree over `(key -> version)` would shrink a *changed* pass's digest from
  the whole changed domain to O(log n + changes), instead of shipping that
  domain's full version map (compressed) once the per-domain hash mismatches.
  Two reasons it was not built: (1) a naive balanced binary Merkle tree reshapes
  on every insert, so it must be an insertion-stable **prefix trie / Merkle
  search tree** to keep incremental hashing; (2) the win is bounded — the digest
  is already small for a personal library, and per-domain hashes + zstd cover
  the common idle case. Measure digest/record bytes and changed-versus-skipped
  passes before choosing an implementation; structured sync tracing uses
  `RUST_LOG=nova_sync=debug`. Build it only if
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

- **Per-peer record counts (S / low / low).** Extend existing attempt/success/error
  diagnostics with counts for each exchange.
- **Native Android network-change events (M / medium / low).** Replace the
  connectivity poll if a supported native route monitor becomes available.
- **Sync activity view (M / low / medium).** A per-domain record/byte summary
  and a short recent-events log for troubleshooting.
- **Transfer stats (S / low / low).** Show records exchanged and duration after
  each sync.
- **Pairing confirmation sheet (S / low / low).** The inline confirmation card
  could become a bottom sheet reusing `menusheet.slint`, with a large match code.
- **QR for the manual id (S / low / low).** Also render the raw endpoint id as a
  QR for the advanced add-by-id flow.

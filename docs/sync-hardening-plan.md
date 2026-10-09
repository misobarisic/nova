# Sync hardening contracts

Connection recovery, durable mutation ownership, projection replay, merge,
pairing, and Android lifecycle ownership are implemented. Device fault injection
and retained-connection power measurements remain outstanding. See
[the project map](project-structure.md) for code navigation and
[sync follow-ons](sync-follow-ons.md) for deferred feature ideas.

## Required invariants

1. No cache eviction/recovery/shutdown path re-acquires a mutex it already holds.
2. A UI edit changes authoritative memory immediately; persistence debouncing
   cannot erase it on navigation or an unrelated remote refresh.
3. Installation defaults are distinguishable from deliberate user changes.
4. Local changes remain durable while networking is disabled.
5. Missing materialized data is not evidence of an explicit deletion.
6. Tombstoned, absent, and live records remain distinguishable to app logic.
7. Deferred work cannot overwrite newer desired state or resurrect a removal.
8. Semantic merges converge deterministically across devices, including ties.
9. Acknowledged data is durable; failed writes retain retry/recovery information.
10. Every durable remote change is eventually projected to an attached UI,
    independently of whether the transport handshake finishes.
11. Removed peers cannot open new authorized exchanges on retained connections.
12. Foreground and headless execution cannot accidentally stop each other's engine.

## Implemented contracts

### Connection recovery and settings edits

- Cached connections are cloned under an explicit scoped guard; closed-handle
  eviction and dialing run after releasing it. Network cache clearing and stop
  do not hold that guard across asynchronous work.
- `SettingsPage::save_settings` now reports edits synchronously. Backend
  `capture_settings` updates authoritative cache/torrent memory and publishes
  synced fields immediately when an engine exists. `wire_settings_autosave`
  owns a UI-thread, application-lifetime timer shared with playback speed;
  persistence reads **current memory**, not a captured control snapshot.
  Existing immediate torrent callbacks remain immediate.
- Explicit requests and completed iroh network refreshes set a coalesced retry
  flag; the worker consumes it at the next pass and clears failure health once.
  Periodic/pairing wakes retain backoff. Retry health uses `Instant`, not epoch
  seconds. Network refreshes coalesce while in flight and do not wake a stopped
  engine. Refresh completion is not proof of peer reachability; a failed retry
  enters backoff again.
- Settings/foreground changes wake a separate cadence notifier and recalculate
  the deadline relative to the last periodic wake. The existing interval clamps,
  bounded concurrency, pass timeout, and discovery-round limit are unchanged.

### Storage, projection, merge, and Android ownership

- **Storage/recovery:** `try_get_str` / `try_scan_prefix` / `try_write_batch`
  distinguish absence from failure. Batch errors abort the transaction. Dirty
  rows, HLC, and extra app/baseline metadata clear only after successful commit.
  Migration removes the legacy source in that same batch. Corrupt rows/clocks
  move atomically to content-addressed `sync:quarantine:<hash>` evidence, outside
  `srec:`; malformed byte lengths cannot split UTF-8. Failed reads write-block
  the corresponding app key. Settings shows storage failure rather than silently
  claiming success. Identity creation is serialized and durable; invalid stored
  identity bytes block setup instead of silently changing endpoint identity.
- **Ownership/transaction boundary:** `local.rs::local_store` is a process-wide,
  socket-free `Arc<Mutex<Store>>`; the network engine attaches to that owner.
  App persistence prepares record changes, `sync:baseline:<domain>`, app JSON,
  and HLC for one redb transaction. The baseline, not the remote store, is the
  diff authority. Unchanged stale fields neither republish nor resurrect, and
  unseen remote additions are never inferred deletions. Unknown object fields
  survive record updates; whole collection maps are replaced exactly, since
  missing map keys are deletions rather than unknown schema fields.
- **Migration/first sync:** baseline-less legacy snapshots seed only absent
  record keys before authoritative projection; durable headless values and
  tombstones win over those stale snapshots. Existing settings conservatively
  count as local intent. An installation without a persisted settings object
  seeds a defaults baseline without publishing defaults. `settings_edited(field)`
  publishes an explicit choice even if it equals the baseline default. The
  durable record itself is edit provenance. Device-local settings stay local.
- **Projection:** remote apply commits `sync:projection:pending` with its rows
  before signaling callbacks. Startup/attachment and the UI poll replay pending
  domains. A domain is acknowledged as projected only if its version basis is
  still current and app writes did not fail; failed writes retain replay work.
  The apply guard is thread-local and nesting-safe, not held over awaits.
- **Addons:** `Installed` holds desired entries before manifests are available.
  Manifest/probe callbacks require the same generation; removed/re-added URLs
  invalidate stale callbacks. Completion reads current desired flags/order;
  remote-origin callbacks retain projection origin. Configure reachability is
  device-local, and unavailable entries remain persisted rather than uninstalled.
- **Progress/clock:** JSON activity/watch/unwatch candidates retain their action
  versions. Candidate union uses max version with a deterministic value tie-break;
  play count uses max. The operation is commutative/associative/idempotent.
  A synthesized merged record gets a local envelope version but does not invent
  a new action. Legacy records initialize candidates from record versions. Receive
  rejects clocks beyond one hour; reload quarantines poisoned rows/HLC and
  reconciles unaffected records. Action clocks must not exceed the envelope.
- **Completion/trust:** dirty outgoing state is committed before taking the
  digest/ack basis. EOF before Done, truncated headers, unexpected trailing
  frames, and writer failure cannot count as full success. Durable apply is
  independently replayable after later transport failure. Revocation shares the
  store-lock boundary with apply; a typed QUIC removal code is distinct from
  unknown authorization. Pairing is bounded, consumes invite+trust atomically,
  and the joiner installs host trust before its completion close permits fan-out.
- **Android:** serialized lifecycle transitions coordinate foreground ownership
  and a background lease. Job cancellation drops its one-shot future/JoinSet,
  never uninstalls a foreground-owned engine. Headless periodic passes are
  suppressed; the one-shot budget includes setup elapsed time. Setup itself is
  synchronous and cannot currently be interrupted mid-setup, so device tests
  must verify its latency against the OS job window. Java finishes at most once.

## Remaining validation

Remaining release work: Android target/device handoff, job cancellation, Doze,
network/process-death fault injection, and retained-connection power measurements.
The retention policy has deliberately not been changed without measurements.
Full concurrent lifecycle barrier coverage and the complete fault-injection matrix
remain validation work, not claims made by the host tests below. Behavioral tests run locally; CI runs formatting, Clippy, and build checks.

Android changes also require the configured Android toolchain, a target check
using `--no-default-features --features android`, and device tests for job/activity
handoff, Wi-Fi/cellular transitions, process death, and Doze. Host tests alone
cannot certify Android lifecycle behavior.

## Regression and fault-injection matrix

Tests should use controllable barriers/clocks rather than timing-only sleeps.
Real iroh endpoint tests can reuse the existing `Minimal` preset and memory
address lookup. App integration tests must exercise actual reconciliation and
mutation paths, not only the generic record store.

| Scenario | Required assertion |
|---|---|
| Cached connection closes between passes | Next attempt redials; cache lock remains available; shutdown completes. |
| Settings edit followed by navigation before 600 ms | Memory retains edit; persistence eventually commits; reopening cannot restore old controls. |
| Remote refresh during a pending local edit | Unrelated remote fields apply without erasing local intent, including device-local controls. |
| Fresh/default peer joins customized mesh | Untouched defaults do not win; deliberately selected defaults still sync. |
| Sync off: add/edit/delete, then restart/re-enable | Mutations survive and reconcile with remote/headless changes. |
| Remote apply paused before UI callback; local write occurs | Unseen additions are not tombstoned and stale fields are not republished. |
| Uncached addon fetches fail or complete out of order | Desired set/order survives; removal and newer flags supersede old callbacks. |
| Remote hide/progress tombstone with stale local mirror | No accidental resurrection; explicit newer local edits remain possible. |
| Same-second watch/unwatch/rewind across three peers | Stable converged result regardless of exchange direction and order. |
| Storage unavailable; batch commit/migration failure | Error surfaced; dirty work/source data retained; restart/retry recovers. |
| Corrupt row, malformed UTF-8 split, repeated reload | No panic or recursive quarantine; unaffected records survive. |
| GC removes a tombstone from a nonempty domain | Deleted row stays gone after reload; unacked tombstones remain. |
| Cancel after durable apply but before handshake end | UI eventually receives/replays change; no false full-success acknowledgement. |
| Peer removed with a connection/stream already open | No new authorized exchange or post-revocation application. |
| Pairing reply delayed while sync worker wakes | Both sides finish authorized pairing without rejection-driven removal. |
| Network restored while all peers are in backoff | One bounded recovery attempt occurs; repeated signals coalesce. |
| Clock moves backward or peer sends far-future version | Ordering/recovery follows the documented policy before and after reload. |
| Older/newer settings payloads and unknown fields | Unknown data is preserved, not tombstoned by unrelated edits. |
| Activity opens mid-headless-job; OS cancels job | Foreground ownership/callbacks survive; canceled job work terminates. |

For deadlock tests, use a process-level test timeout as well as assertions about
lock availability; an async timeout cannot reliably terminate a synchronously
blocked runtime worker.

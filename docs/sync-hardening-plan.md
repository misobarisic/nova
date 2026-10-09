# Sync hardening implementation plan

Status: **A–G code implemented; H partially implemented; release/device validation outstanding**. Baseline: `05d5ff4`; working branch:
`feat/sync-hardening`.

This plan follows a source audit of settings persistence, app materialization,
the sync store, connection recovery, pairing, and Android background execution.
It prioritizes freezes and unintended resets over new sync features. See
[project-structure.md](project-structure.md) for navigation and
[sync-follow-ons.md](sync-follow-ons.md) for the broader feature backlog.

## 1. Scope and evidence

The audit was read-only. The connection-cache mutex lifetime was verified with
Rust compiler MIR. Small in-memory models checked progress tie behavior and
quarantine recursion; those are not substitutes for application regression tests.
Network fault injection and Android device reproductions remain to be done.

In the inventory below, **source** means the problematic mechanism is visible
in the audited code; **race** means its impact depends on execution order and
needs a controlled reproduction. Neither label means an on-device test passed.

| ID | Priority | Evidence | Finding and starting point |
|---|---|---|---|
| H01 | critical | source + MIR | `peer_connection` holds the cache mutex through an `if let` body, then `forget_connection` locks it again for a closed connection (`crates/sync/src/lib.rs`). |
| H02 | high | source | The Settings page owns the 600 ms save timer; destroying the page can discard pending edits (`crates/ui/settings.slint`, `appwindow.slint`, `src/app/settings.rs`). |
| H03 | high | source | Fresh-device settings defaults receive new record versions and can override deliberate settings elsewhere (`src/app/sync.rs::sync_seed`, `settings_fields`). |
| H04 | high | source | Startup/re-enable applies old sync records before seeding, overwriting edits made while sync was disabled (`Bridge::start_sync`). |
| H05 | high | race | Full snapshot diffing interprets unseen remote records as local deletions (`sync_records`). |
| H06 | high | source + race | Pending addon manifests are absent from `installed`; snapshot writes can delete them, and late fetch callbacks can republish stale state (`sync_apply_addons`, `src/app/addon_mgr.rs`). |
| H07 | high | source | Live-only reads cannot distinguish absence from tombstones; progress/hide reconciliation can resurrect deleted records (`sync_apply_progress`, `sync_apply_continue_hidden`). |
| H08 | high | source | Whole-second progress ties favor local state, making watch/unwatch and rewind merges order-dependent (`merge_progress`). |
| H09 | high | source | Read errors collapse into defaults; writes return no outcome and sync saves drain dirty rows before durable success (`src/app/io.rs`, `crates/storage`, `Store::save`). |
| H10 | high | race | Remote records can be saved before a canceled handshake prevents UI notification; incomplete completion can still return success (`protocol.rs::exchange`). |
| H11 | high | source + race | Membership is checked once per connection, not each reused stream; pairing wakes sync before both sides have installed trust (`protocol.rs`, `pair.rs`, `attempt_join`). |
| H12 | high | race | Foreground startup can reuse a headless engine without attaching callbacks; the job can later uninstall the shared engine (`src/app/android_bg.rs`, `Bridge::start_sync`). |
| H13 | medium | source | Manual/network-triggered passes still respect failure backoff, network refresh is not awaited before waking sync, and interval changes wait for the current cycle (`lib.rs`). |
| H14 | medium | source | Future record versions are retained uncapped, receive-side clock observation is capped, and reload adopts the uncapped maximum (`hlc.rs`, `store.rs`). |
| H15 | medium | source | Quarantine backups remain under the active `srec:` prefix; GC persistence detects domain removal rather than all removed rows (`store.rs`, `Handler::gc_tombstones`, setup GC). |
| H16 | medium | source | Snapshot diffing can tombstone unknown settings; typed record rewrites can discard fields from newer clients (`src/app/sync.rs`). |
| H17 | medium | source | Android job cancellation does not cancel native work; its 150 s wait differs from the engine's 480 s maximum pass (`NovaSyncJobService.java`, `android_bg.rs`). |
| H18 | measure first | dependency inspection | iroh 1.2 defaults to 5 s keepalives on retained connections; a long sync interval is not a network-silence guarantee. |

### Behavior to preserve or clarify

- Sync remains opt-in; recording local changes offline must not bind sockets.
- Language, playback speed, decoder, player backend, episode-start behavior,
  torrent/download preferences, and sync options remain device-local.
- Animation/display settings are currently synced, including `anim_nav_slide`.
- Image-cache controls intentionally form a whole-value LWW group. Library
  entries and addon order also have whole-record semantics. Concurrent edits
  within these records can overwrite each other by policy, not by deadlock.
- Existing pairing identity, peer-removal safeguards, bidirectional exchange,
  compression, and digest short-circuiting should be retained.

## 2. Required invariants

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

## 3. Implementation sequence

Use small, independently reviewable changes. Add failing regressions before or
alongside each fix. Do not mark a stage complete based on compilation alone.

### Stage A — immediate liveness and edit preservation

- [x] **A1 / H01:** clone a cached connection inside an explicit guard scope;
  inspect and evict only after releasing the lock. Review cache access during
  network changes and shutdown. Do not rely on Tokio timeouts to cancel a
  thread blocked in `std::sync::Mutex::lock`.
- [x] **A2 / H02:** move setting mutations into backend-owned state immediately.
  Keep the persistence debounce in an application-lifetime owner, not a
  conditionally instantiated page. Remote refresh must preserve pending local
  edits. Cover torrent settings sharing the current timer and coordinate the
  separate playback-speed save path.
- [x] **A3 / H13:** distinguish periodic, manual, and network-restored triggers.
  A manual request should make one bounded attempt despite previous backoff;
  verified connectivity recovery should reset relevant failure state. Await
  network refresh before triggering recovery. Recompute interval deadlines
  when settings or foreground state changes; use monotonic retry timing.

Acceptance: a closed cached connection redials, settings survive immediate
navigation, and manual recovery actually attempts an eligible peer. Preserve
trigger coalescing and bounded concurrency; do not introduce retry storms.

Implemented contract:

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

Regression coverage: closed cached connection/redial/cache availability/shutdown
in `reused_connection_carries_successive_passes`; periodic skip vs explicit
worker retry in `explicit_worker_wake_retries_backed_off_peer`; controlled
network-refresh barrier and cadence-change tests in `lib.rs`; real Settings
toggle plus immediate navigation, torrent capture, current-state projection,
and shared playback-rate persistence in `app::settings::tests`. Deadlock tests
were run under a process-level `timeout 180s`.

Verified: `cargo test -p nova-sync --lib --test store_persistence --locked`
(71 unit + 1 persistence test passed), app library tests (130 passed),
`settings_sync_overflow`, `settings_playback_speed`, `settings_language`
(all passed), and `cargo check --workspace --locked` (passed). The full workspace
test run stopped at `tests/home_carousel_flick.rs:139` (flick-inertia assertion;
Home code is unchanged). Windows cross-check was attempted but blocked by the
missing `x86_64-pc-windows-gnu` Rust target. Existing duplicate-test-attribute and
`tmp_snap` warnings were left untouched. Android device/network fault injection remains deferred;
this stage does not claim to resolve the lifecycle and mutation-authority issues
in C/G. No persisted keys or wire schemas changed.

### Stage B — trustworthy persistence and recovery

Dependency: A can ship first; complete this stage before relying on durable
mutation tracking in C.

- [x] **B1 / H09:** return structured outcomes from storage reads and writes.
  Separate missing keys, schema errors, database unavailability, and failed
  transactions. Do not silently persist fallback defaults over unreadable data.
  Preserve raw data for recovery and surface a user-visible persistence error.
- [x] **B2 / H09:** retain dirty records until the complete batch commits. Failed
  migration writes must not remove their source blob. Serialization/table/write
  failures must not result in a supposedly successful partial batch.
- [x] **B3 / H15:** put quarantine backups outside active record namespaces.
  Make repeated recovery idempotent, recognize existing backups safely, and
  validate row-key UTF-8 boundaries before splitting malformed keys.
- [x] **B4 / H15:** have GC report actual removed records and persist every
  collection, including when a domain still contains live records. Cover both
  startup GC and pass-end GC.
- [x] **B5 / H09:** handle unsupported enum values deliberately. Prefer isolated
  field recovery with diagnostics over resetting the entire settings object;
  preserve unknown raw values when needed for downgrade compatibility.

Acceptance: injected commit failures remain retryable; corruption does not
multiply backups or overwrite recoverable settings; migration and GC survive
reload. Preserve the current database and stable endpoint identity.

### Stage C — local mutation authority and first-sync policy

This is the central design change. Decide and document the storage contract
before altering seed/apply order.

Recommended direction: separate the durable record/mutation owner from the
network endpoint. Local edits must be versioned and persisted with sync off;
the network engine reads and exchanges those records when enabled. App/UI
snapshots are projections, not deletion authorities.

- [x] **C1 / H04, H05, H09:** choose either atomic app-snapshot + record updates
  or a durable mutation journal with idempotent replay. Describe ownership,
  transaction boundaries, startup replay, and migration in this document.
- [x] **C2 / H05, H16:** replace routine full-snapshot diffing with explicit
  per-key upsert/delete mutations. If a reconciliation diff remains, compare
  against the app's acknowledged/materialized baseline, never against remote
  records the app has not yet seen. Preserve unknown domains and fields.
- [x] **C3 / H03:** define first-sync policy. Recommended: adopt existing mesh
  values for untouched settings; publish deliberate local edits. Persist edit
  provenance so choosing the default intentionally is still a real edit.
  Define a conservative migration for old installs lacking provenance.
- [x] **C4 / H04:** reconcile durable local mutations and headless remote updates
  before projection. Test offline additions, edits, and deletions separately.
- [x] **C5 / H07:** expose record metadata to app reconciliation, including
  version and tombstone state. Preserve only genuinely absent local records;
  explicit newer local actions may supersede a tombstone under normal ordering.
- [x] **C6 / H10:** provide a projection revision/pending-domain mechanism so UI
  attachment, callback failure, cancellation, and process restart can replay
  durable remote changes without relying on a later differing digest.

**Do not fix H04 by simply reversing `sync_apply` and `sync_seed`.** A stale local
snapshot could then overwrite newer headless remote state. Do not infer
intentional edits solely from `value != default` either.

Acceptance: a fresh peer does not reset customized mesh settings; disabling and
re-enabling sync preserves local intent; delayed UI callbacks cannot turn
remote additions into deletions; interrupted transactions replay consistently.

### Stage D — desired addon state and deferred work

Dependency: use C's mutation contract; a narrowly scoped containment fix may
ship earlier if it prevents pending manifest loads from emitting deletions.

- [x] **D1 / H06:** maintain a durable desired addon set independently of loaded
  manifests. Keep unavailable addons installed but marked unavailable rather
  than propagating their absence as an uninstall.
- [x] **D2 / H06:** associate fetch/probe callbacks with a desired-state generation
  or record version. Ignore results for removed/replaced entries and read the
  current desired flags rather than captured stale flags.
- [x] **D3 / H06:** carry mutation origin through deferred work. Keep
  `ApplyingGuard` scoped to synchronous UI apply; do not hold a global apply
  flag across network awaits or suppress unrelated user changes.
- [x] **D4:** preserve desired order even when manifests arrive out of order;
  deduplicate pending installations. Decide whether configure-page reachability
  belongs in synced desired state or device-local derived state.

Acceptance: slow/offline manifests, concurrent removal/toggling, and configure
probe failures do not remove unrelated addons or resurrect deleted ones.

### Stage E — deterministic semantic merge and clock policy

- [x] **E1 / H08:** use deterministic record/action version ordering rather
  than whole-second local-favoring ties in progress reconciliation. Preserve
  explicit unwatch and rewind intent, sticky watched behavior where intended,
  coherent position/duration, and monotonic play-count semantics.
- [x] **E2 / H08:** define whether separate watch/unwatch intent clocks are
  required. Test commutativity, idempotence, and multi-peer convergence;
  associativity must hold or be replaced by a documented convergent operation
  model. Do not blindly replace intended semantic rules with record LWW.
- [x] **E3 / H14:** choose one future-clock policy for receive and reload.
  Recommended: reject/quarantine versions beyond permitted drift with an
  actionable diagnostic, rather than retaining unbounded versions while
  capping only the local clock. Define recovery for already-poisoned stores.
  Never rewrite a remote version silently or weaken ack-gated tombstone GC.

Acceptance: same-second watch/unwatch/rewind and clock rollback converge;
far-future values do not repeatedly undo local edits or poison clocks on reload.

### Stage F — exchange completion, pairing, and peer revocation

- [x] **F1 / H10:** distinguish durable apply, transport completion, UI projection,
  and full success. Require the expected `Done`/stream-end sequence; truncated
  frame headers and EOF before completion must not count as successful exchange.
- [x] **F2 / H09, H10:** issue acknowledgements only after the required durable
  work and valid completion. Keep the digest snapshot basis for ack clocks;
  changes created mid-exchange must not be falsely acknowledged.
- [x] **F3 / H10:** guarantee writer-task cleanup on every cancellation path,
  including cancellation while joining. Report failed completion as an error,
  not merely as a skipped acknowledgement followed by `Ok(())`.
- [x] **F4 / H11:** re-check membership before each stream and before applying
  incoming changes. Revoke/close retained connections on removal and prevent
  in-flight work from applying after revocation under the chosen lock contract.
- [x] **F5 / H11:** coordinate pairing completion before immediate sync fan-out.
  Separate not-yet-authorized from explicitly removed where possible. Preserve
  the rule that only explicit pairing/manual add can re-add a removed peer.
- [x] **F6:** use structured error classification and retain full error chains.
  Do not classify removal by searching only an outermost context string.
  Bound pairing/removal operations and overlapping pending join attempts.

Acceptance: handshake cancellation still projects already-durable changes;
partial exchanges never claim full success; an existing connection cannot
bypass removal; delayed pairing responses do not destroy newly established trust.

### Stage G — Android engine ownership and cancellation

Dependency: use C's replay/attachment contract and F's exchange outcomes.

- [x] **G1 / H12:** add explicit lifecycle ownership/leases or an equivalent
  coordinator. Foreground startup must attach callbacks and reconcile records
  even if a headless engine exists. A finishing job must release only its own
  ownership, not unconditionally uninstall the current singleton.
- [x] **G2 / H12:** serialize concurrent setup/install/uninstall transitions.
  Cover a foreground activity arriving before, during, and after job setup.
- [x] **G3 / H17:** propagate `onStopJob` cancellation into Rust. Cancel pending
  work, release ownership, and finish/reschedule exactly once. Check both sync
  enable and background-enable flags before headless work starts.
- [x] **G4 / H17:** give a one-shot pass an explicit overall budget consistent
  with the job window. Distinguish completed, canceled, timed-out, and skipped
  outcomes; never log an unfinished pass as successful.

Acceptance: opening the app mid-job keeps foreground sync active and projects
records; stopping a job stops its work without stopping a foreground owner.
Keep Android code target-gated and preserve storage-before-settings initialization.

### Stage H — diagnostics, power policy, and release validation

- [x] Show per-peer attempt/success/error/backoff state and distinguish durable
  sync success from a worker wake-up. The watchdog must expose a stuck worker,
  not merely hide its syncing flag. Surface setup/storage failures.
- [x] Record bounded, payload-free diagnostics for trigger reason, connection
  reuse/redial, duration, domain counts, projection revision, and cancellation.
  Never log invite secrets, private identities, or credential-bearing addon URLs.
  `RUST_LOG=nova_sync=debug` enables structured `tracing` events/spans for
  reason/reuse/redial/duration/counts and projection basis;
  one-shot outcomes classify cancellation/timeout without payloads. Addon
  install/refresh logs no longer print configured URLs, labels, or response errors.
  The app installs the subscriber once (desktop stderr, Android `Nova` logcat,
  including job-only startup). `NOVA_SYNC_DEBUG` was removed in the separate
  tracing follow-up commit; user-visible status/persistence errors are unchanged.
- [ ] **H18:** measure retained-connection traffic and battery cost on Android.
  Choose a foreground/background connection-retention policy from measurements;
  do not increase timeouts or disable keepalives blindly.
- [ ] Add behavioral tests to PR validation; **not enabled per user request** (tests run locally). Compilation alone misses these
  defects. Update the matching project-structure sections after each shipped
  storage, API, lifecycle, or protocol change.

### B–G implementation contracts and limits

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

Remaining release work: Android target/device handoff, job cancellation, Doze,
network/process-death fault injection, and retained-connection power measurements.
The retention policy has deliberately not been changed without measurements.
Full concurrent lifecycle barrier coverage and the complete fault-injection matrix
remain validation work, not claims made by the host tests below. PR tests were
explicitly declined by the user; `.github/workflows/build-release.yml` is unchanged.

Local verification for B–G: 77 sync unit tests plus `store_persistence` and
`local_mutations` pass; 131 app library tests and `settings_sync_overflow`,
`settings_language`, `settings_playback_speed` pass. New coverage includes injected
commit failure/retry, idempotent quarantine/UTF-8/future-clock reload, GC from a
nonempty domain, offline baseline edits/deletions with unseen remote additions,
pending projection reload, three-peer progress convergence, EOF/truncated/trailing
completion failures, and controlled revocation before durable apply. The isolated
app regression exercises fresh/default choice provenance, real tombstone
projection, desired addon order/stale flags/generations, and unsupported local
and remote grouped setting enums through actual persistence paths.

`cargo check --workspace --locked` passed. Full workspace tests (retried with
`-j 2` after a compiler SIGKILL) again stop at the pre-existing
`tests/home_carousel_flick.rs:139` inertia assertion; that file is unchanged.
Android and Windows target checks fail with E0463 (target `core`/`std` libraries
not installed). `git diff --check` passes. Host tests do not validate Android
lifecycle behavior or battery cost. Existing duplicate-test and `tmp_snap`
warnings are unrelated and retained.

## 4. Regression and fault-injection matrix

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

Run the narrowest relevant tests first, then broader validation:

```sh
cargo test -p nova-sync --lib
cargo test -p nova-sync --test store_persistence
cargo test --lib app::sync::tests
# Run new headless UI regressions individually as they are added.
cargo test --test settings_sync_overflow
cargo check --workspace --locked
cargo test --workspace --locked
cargo check --workspace --locked --target x86_64-pc-windows-gnu
```

Android changes also require the configured Android toolchain, a target check
using `--no-default-features --features android`, and device tests for job/activity
handoff, Wi-Fi/cellular transitions, process death, and Doze. Host tests alone
cannot certify Android lifecycle behavior.

## 5. Compatibility, migration, and implementation checkpoints

- Start with internal fixes without changing current ALPNs. Adding/reordering
  postcard fields requires a version bump of the affected protocol and explicit
  mixed-version behavior. Local JSON evolution still needs downgrade tests.
- Before C/E/F changes, record the selected mutation, provenance, action-clock,
  and pairing contracts here. Any new KV keys, files, domains, modules, or UI
  callbacks must also be documented in `project-structure.md` in that change.
- Migration must preserve identity, membership/tombstones, settings, library,
  progress, addon order, and quarantine evidence. Test interruption and retry.
  Back up test fixtures; do not repair production data by deleting the database.
- Legacy installs cannot reveal whether a default-valued setting was deliberately
  selected. Make that ambiguity explicit; adopt a conservative policy rather
  than claiming to recover intent from values alone.
- Keep caches/projections rebuildable. Do not acknowledge undurable changes or
  introduce a rollback path that revives stale app snapshots.
- Preserve translation conventions and the three-step Slint callback wiring.
- Do not broaden this work into mDNS, rendezvous servers, selective sync,
  permanent mobile services, Merkle trees, or a wholesale CRDT rewrite.

Suggested delivery order: **A → B → C → D/E → F → G → H**. Independent narrow
fixes can land earlier with tests; dependencies above still apply. Each delivery
should update its checklist, record actual verification, and link any deliberately
deferred issue. Do not mark a timing risk resolved without a controlled test.

### Decision log (complete during implementation)

- [ ] Durable mutation owner and transaction/replay contract selected.
- [ ] First-sync/default provenance and legacy migration policy selected.
- [ ] Progress intent/version model and future-clock recovery policy selected.
- [ ] Pairing completion/revocation contract and protocol compatibility selected.
- [ ] Android ownership, cancellation, and one-shot pass budget selected.
- [ ] Connection-retention policy justified by measured mobile traffic/power.

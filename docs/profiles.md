# Profiles

More → Profiles creates, renames, selects and deletes personal profiles without
a PIN. Existing installations migrate into **Default**, retaining their library,
history, settings, addon configuration and record versions. A new profile starts
with empty personal data and normal application defaults.

**Open last profile on startup** defaults to enabled, including upgrades where
the preference is absent. Disabling it shows the profile picker on startup;
navigation cannot bypass that picker. The selection and toggle are saved only
on this device at `profiles:device:v1`. Choosing the last-used profile still
works when it is already marked Current. Tracking and Stremio workers wait for
selection before starting when the startup picker is required.
Paired-device playback also waits for selection and exposes no remembered
profile playback state while the picker is required.

Personal state includes Library, playback progress and viewing history, Home
removals, language/display/player settings, title playback preferences, addon
membership/order/configuration, desired addon package versions, search history,
last-stream choices, and tracking/Stremio state. Device pairing and Sync settings
remain account-wide. Downloads and their files/timings, torrent storage/settings,
window geometry, interface mode, updates and onboarding remain device-local and
shared between profiles. Metadata/artwork caches and installed package archives
can be reused; the selected profile controls enabled addons and their settings.

Profile snapshots and record domains use `profile:{id}:{logical-name}`. The
account domains `profiles` and `profile_deletions` carry profile names and
permanent retirement markers. The active selection never filters record
exchange: every paired device synchronizes every profile. Inactive profile
records materialize into durable snapshots without changing the screen or local
selection. `nova/sync/4` and `nova/pair/3` isolate these semantics from older
clients; all paired devices need an updated Nova. The postcard schema is unchanged.

The deterministic legacy ID is `default`. Its untouched name has a neutral
version so a newly paired installation cannot overwrite an explicit rename.
Migration moves snapshots, domains, incremental baselines and pending projections
atomically, preserving existing versions and progress action clocks. New IDs
combine the account device ID with a durable local sequence.
Android background sync performs the same migration before opening an upgraded
sync session, even if the Activity has not launched yet.

Each asynchronous operation captures a profile ID and activation epoch. Writes
remain owned by that profile; stale UI publications are discarded after a
switch. Long-running personal service reads use `Bridge::personal_state`: it
locks the live state before validating the captured activation, and snapshots
inputs before releasing that lock. Activation changes the epoch before replacing
the state under the same lock, so an outgoing worker cannot read the incoming
profile's library or history. Settings autosave captures ownership when scheduled.
Provider settings
backends and session hosts are replaced on activation; generation guards reject
old requests and configuration writes. Tracking and Stremio use separate service
instances per activation and profile-scoped storage. Global queues retain their
cleanup callbacks even when an old profile's publication is cancelled.
Selection persistence is prepared before outgoing services stop, followed by an
infallible runtime selection commit. A failed selection write keeps the outgoing
profile and playback usable. Incoming load failures retain the selection picker
for retry rather than marking the session loaded.

Deleting a profile removes its personal snapshots, baselines and last-stream
cache files, and tombstones its record values. A permanent deletion marker
prevents stale offline peers and delayed workers from restoring it. Late live
records are tombstoned again for convergence and cleanup. At least one profile
must remain locally; concurrent deletion of all profiles creates a fresh profile
identity rather than resurrecting a retired one. Deleting the active profile on
another device returns this device to profile selection. Shared download files
are removed through their existing download actions, independently of profiles.

Implementation: `src/app/{profiles,profile_actions,profile_net}.rs`,
`src/app/profiles/storage.rs`, `src/app/{io,persistence,sync,last_stream}.rs`,
`crates/sync/src/profiles.rs`, and `crates/ui/{more,profiles}.slint`.

## Ownership plan

The next internal refactor should make ownership explicit without changing the
storage namespaces, all-profile sync, migration, or shared-download behavior.
The current thread context and guarded reads contain cross-profile publication
and snapshot hazards; they do not provide a complete service commit fence for
rapid A → B → A activation.

| Owner | Lifetime | State |
| --- | --- | --- |
| `AccountRuntime` | Account/process | Sync identity, pairing, peers, profile directory, permanent deletion markers and projection for every profile |
| `DeviceRuntime` | Device/process | Downloads/torrents and files, startup preference, window/interface state, shared caches/archives and the physical player |
| `ProfileStore` | Durable profile identity | Explicit profile ID, personal snapshots, sync transactions, retirement checks, tracking/Stremio credentials and journals |
| `ProfileSession` | One activation | Personal/view state, provider host, tracking/Stremio service handles, cancellation, activation ID and UI publication |

The physical player stays device-owned. Its playback target, progress owner,
party/remote participation and pending callbacks belong to the session.

Implement this in stages:

1. Give each `ProfileSession` its own state allocation. Workers capture that
   allocation or immutable inputs at launch; switching publishes a different
   session rather than overwriting state retained by old work. UI callbacks
   resolve the current session when invoked, then pass it explicitly.
2. Prepare the incoming session and validate its state/provider configuration
   before outgoing teardown. Flush accepted edits/checkpoints, prepare the local
   selection write, then publish the prepared session on the UI thread. Errors
   before publication leave the outgoing session usable; partial transitions
   have an explicit recovery state.
3. Replace ambient persistence ownership incrementally with `ProfileStore`.
   Start with scoped storage, `TrackingStorage`, settings autosave and Stremio
   transactions. Durable writes belong to a profile, not its activation epoch:
   switching does not cancel an accepted edit, while retirement rejects writes
   in the same serialized transaction. Keep TLS only as an audited adapter.
4. Put service handles on the session and keep a small draining-worker registry.
   Accessors must not create services. For A → B → A, defer a new mutable service
   for A until its old instance drains, or fence commits with a service lease.
   A check immediately before a write alone is insufficient. Already-issued
   external acknowledgements remain attributable to their original owner.
5. Separate completion/resource release from UI publication. Completion and
   queue-permit cleanup always run, including cancellation and failed event-loop
   dispatch; `session.publish(...)` alone checks the current activation.

Use controlled barriers to test metadata completion across A → B and A → B → A,
and fault injection for selection writes and incoming-session preparation.
Assert that histories never cross profiles, only one service owns each mutable
credential/outbox, old UI results are discarded, and every shared queue permit is
released. Retain migration/version, inactive projection, retirement, autosave
ownership and startup-picker regressions. This is an app-module refactor; it
does not require a new crate, actor framework or sync wire-schema change.

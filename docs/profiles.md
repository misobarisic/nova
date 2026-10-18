# Profiles

More → Profiles creates, renames, selects and deletes personal profiles without
a PIN. Existing installations migrate into **Default**, retaining their library,
history, settings, addon configuration and record versions. A new profile starts
with empty personal data and normal application defaults.

The picker uses compact, full-row profile buttons with a circular first-letter
avatar and a marker for the current profile. **Add profile** opens the name
editor; **Manage profiles** contains Rename/Delete and returns to the picker
with Back or Done. Management and name editors cannot bypass required startup
selection. Keyboard and TV navigation reveal the focused action while scrolling.

Avatar initials follow the displayed name (including translated Default and
Unicode names). Foreground/background pairs vary by a stable hash of the profile
ID, using contrasting colors. Renaming, reordering, restarting or syncing to
another device preserves the pair; no avatar files or additional stored/synced
fields are needed. The picker respects the current theme and accent color.

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

Each asynchronous operation captures its originating `ProfileSession`, including
the profile ID, activation epoch and stable personal-state allocation. Switching
publishes a different allocation; old continuations can finish without accessing
the replacement profile. Live actor reads additionally use `Bridge::personal_state`
to reject cancellation. Durable `ProfileStore` adapters retain only the profile
identity, so snapshot jobs, credentials, journals and acknowledgements survive
switching without keeping a UI session alive. Retired identities reject writes.

Incoming snapshots, recoverable settings and provider configuration are prepared
before outgoing teardown or the device selection write. Preparation failures leave
the outgoing selection and playback usable. The prepared session is published on
the UI thread after outgoing checkpoints flush and service shutdown is requested.
Tracking and Stremio handles belong to the session; a new activation of the same
profile waits for its previous actors to drain. Already-issued external results
checkpoint the original durable owner, while cancellation prevents fresh requests.

Provider settings and hosts retain their durable owner. Ordinary metadata work
captures that authority and an immutable package/configuration/metadata inventory
from its own session; a queued request cannot execute another profile's addon code.
Stale fallback chains complete with cancellation instead
of starting requests. Shared resolver downloads retain their package, configuration
and settings capability independently of the active session. Switching away and
back does not revoke them; actual settings/addon/package edits and retirement do.
Completion and resource cleanup always run, while stale UI publication is dropped.
The persistent snapshot and last-stream writers and physical download workers
retain durable inputs rather than the session that launched them.

Deleting a profile removes its personal snapshots, baselines and last-stream
cache files, and tombstones its record values. A permanent deletion marker
prevents stale offline peers and delayed workers from restoring it. Late live
records are tombstoned again for convergence and cleanup. At least one profile
must remain locally; concurrent deletion of all profiles creates a fresh profile
identity rather than resurrecting a retired one. Deleting the active profile on
another device returns this device to profile selection. Shared download files
are removed through their existing download actions, independently of profiles.

Implementation: `src/app/{profiles,profile_session,profile_actions,profile_net}.rs`,
`src/app/profiles/{store,storage}.rs`, `src/app/{io,persistence,sync,last_stream}.rs`,
`crates/sync/src/profiles.rs`, and `crates/ui/{more,profiles}.slint`.

## Ownership boundaries

The runtime separates activation lifetime from durable ownership without changing
storage namespaces, all-profile sync, migration, or shared-download behavior.

| Owner | Lifetime | State |
| --- | --- | --- |
| Account services / `AccountRuntime` | Account/process | `nova-sync` owns identity, peers and all-profile records; app runtime owns sync startup/recovery controls. Profile directory and retirement are independent of the selected session. |
| `DeviceRuntime` | Device/process | Downloads, physical player, poster generations/pipeline, usage scans, last-stream dispatcher and session directory. Device preferences, torrents and shared caches remain in their existing device stores/services. |
| `ProfileStore` | Durable profile identity | Explicit profile ID, personal snapshots, sync transactions, retirement checks, tracking/Stremio credentials and journals |
| `ProfileSession` | One activation | Personal/view state, provider host, tracking/Stremio service handles, cancellation, activation ID and UI publication |

The physical player stays device-owned. Its playback target, progress owner,
party/remote participation and pending callbacks belong to the session.

Implementation rules:

1. Give each `ProfileSession` its own state allocation. Workers capture that
   allocation or immutable inputs at launch; switching publishes a different
   session rather than overwriting state retained by old work. UI callbacks
   resolve the current session when invoked, then pass it explicitly.
2. Prepare the incoming session and validate its state/provider configuration
   before outgoing teardown. Flush accepted edits/checkpoints, prepare the local
   selection write, then publish the prepared session on the UI thread. Errors
   before publication leave the outgoing session usable.
3. Use explicit `ProfileStore` adapters for scoped storage, snapshot jobs,
   `TrackingStorage` and Stremio transactions. Durable writes belong to a profile,
   not its activation epoch:
   switching does not cancel an accepted edit, while retirement rejects writes
   in the same serialized transaction. Keep TLS only as an audited adapter.
4. Put service handles on the session and keep a small draining-worker registry.
   Accessors must not create services. For A → B → A, defer a new mutable service
   for A until its old instance drains.
   A check immediately before a write alone is insufficient. Already-issued
   external acknowledgements remain attributable to their original owner.
5. Separate completion/resource release from UI publication. Completion and
   queue-permit cleanup always run, including cancellation and failed event-loop
   dispatch; `profiles::invoke_from_event_loop` checks the current activation
   before UI publication. `invoke_account` remains independent of selection.

Use controlled barriers to test metadata completion across A → B and A → B → A,
and fault injection for selection writes and incoming-session preparation.
Assert that histories never cross profiles, only one service owns each mutable
credential/outbox, old UI results are discarded, and every shared queue permit is
released. Retain migration/version, inactive projection, retirement, autosave
ownership and startup-picker regressions. This is an app-module refactor; it
does not require a new crate, actor framework or sync wire-schema change.

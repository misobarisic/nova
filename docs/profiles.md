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
switch. Settings autosave captures ownership when scheduled. Provider settings
backends and session hosts are replaced on activation; generation guards reject
old requests and configuration writes. Tracking and Stremio use separate service
instances per activation and profile-scoped storage. Global queues retain their
cleanup callbacks even when an old profile's publication is cancelled.

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

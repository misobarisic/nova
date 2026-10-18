# Simkl and MDBList sync

Settings → Tracking includes separate Simkl and MDBList account cards. Each
account belongs to the selected Nova profile. Connecting verifies the remote
account and saves its sign-in; it does not enable a whole-library upload.

The current Nova interface accepts an MDBList API key from
[account preferences](https://mdblist.com/preferences/). Simkl currently requires
a registered public application client ID and its access token, available through
[Simkl application settings](https://simkl.com/settings/developer/). Nova does not
ship a fabricated application registration. The API crate also implements
Simkl's device authorization and refresh endpoints; browser/device-code sign-in
and automatic token refresh are not yet wired into the account cards. An expired
or rejected credential requires reconnecting. Tokens are never included in
statuses or diagnostics.

## Choosing scope

Open a saved title's Tracking page and choose Simkl or MDBList. **Start syncing
this title** enables that exact native title for that verified account. The
**Include watched episodes** checkbox is unchecked by default. Without it, Nova
checkpoints the local semantic watch journal; subsequent local watched actions
can upload, including a later unwatch/rewatch of an old episode. Old watched
history, position-only updates and paired-device history imports do not gain
upload permission from a timestamp change.

**Stop syncing this title** stores an explicit exclusion. The exclusion also
wins when full-library mode is enabled. It does not remove the title or its
history from the remote service. Nova retains the title's proven catalog
identity with that choice, so removing its native library entry or cached
metadata cannot re-import the same remote title under a canonical ID.

In account settings, check **Sync the full library**, then choose **Start
full-library sync**. This explicitly authorizes importing and exporting all
supported saved titles and watched history for that account. **Use selected
titles only** returns to the account's per-title choices. A selected-title pass
never imports unrelated remote titles or history. Retained links belong to the
verified account and remain available when reconnecting the same account;
a different account cannot use them. Full-library and automatic-sync
preferences also belong to the verified account and are restored on reconnect.

Automatic sync polls in the foreground approximately every 60 seconds. Manual
**Sync now** remains available while automatic sync is off. **Cancel** stops new
requests and pauses the worker until a manual sync resumes it. A request already
sent can finish. Profile switches and disconnect invalidate captured work.

## Merge policy and identity

Sync is additive: saved membership and positive watched history are combined.
Removing a title or marking it unwatched does not erase the other service's
history. Nova uses targeted official watchlist/history writes and preserves
existing remote ratings, custom lists and unrelated status fields. A missing
remote title is added as Planning; existing service statuses are retained.
Custom-list editing and rating sync are not part of this integration.

Title identity requires a current unambiguous typed IMDb or TMDB ID. Contradictory
shared IDs cannot authorize a match. Full-library imports use a canonical IMDb
identity, or a typed TMDB identity when IMDb is absent. Source-specific titles
keep their native identities. Series episode progress requires an exact
`<IMDb ID>:<season>:<episode>` native episode ID or a current confirmed canonical
episode alias. Displayed season/episode numbers alone never authorize a mapping.
Unresolved identities and uncovered episodes remain pending rather than being
marked synchronized or uploaded by guesswork.

Unfinished playback can export a paused scrobble checkpoint when Nova has a real
finite duration and a proven episode identity. Only the most recently updated
local resume candidate is considered for each title. Matching episode positions
merge toward the greater percentage. Different remote/local episode checkpoints
remain pending instead of overwriting one another. Import converts a remote
percentage to seconds only when the matching local episode already has a real
duration; missing durations remain pending. This is checkpoint sync, without
continuous start/pause event streaming or guessed runtimes.

## Persistence and paired devices

The profile-qualified `list_services` generic peer domain contains version-1
JSON `connection:0` (Simkl) and `connection:1` (MDBList) records. Each includes the
verified account, credentials, public client registration and a fresh sign-in
epoch. Disconnect publishes a tombstone. Separate
`account:<JSON(service,account)>:automatic` and `:full` booleans carry preferences
across sign-in epochs. Existing active `session:<service>:<JSON epoch>`
preferences migrate before that credential pointer is replaced or removed.
Credential checks still require the current sign-in epoch; retaining an account
preference cannot authorize requests from an earlier sign-in.

`selected:<JSON(service,account,native type,native ID)>` records contain the
explicit enable/exclusion choice, history preference, start time and unique
grant ID. These choices synchronize with paired Nova devices. Journal counters
are device-local: each receiving device establishes its own boundary when a
grant arrives, so receipt never retroactively authorizes its old history.
`identity:<JSON(service,account,native type,native ID)>` stores the typed,
confirmed catalog IDs associated with a choice. A disabled choice blocks
implicit full-library imports using those IDs even after its native title is
gone. Re-enabling that native choice lifts the exclusion. These records do not
grant consent to a different account or change explicit bindings for other
native titles.

Device-local storage uses:

- `list_services:boundary:v1:<JSON(service,account,type,ID,grant)>`: the local
  semantic journal sequence captured for that selection grant.
- `list_services:watched:v1:<JSON(series ID,episode ID)>`: the latest local
  watched-transition sequence, written atomically alongside Nova's existing
  history journal. Remote and paired-device observations do not update it.
- `list_services:simkl_cache:v1:<JSON account>`: the last successful Simkl
  activities and library snapshot, restored only after account verification.
  It avoids repeating a full initial library pull after every restart.

The credential records are plaintext JSON in Nova's database, following the
existing tracker/Stremio storage choice. They are shared through encrypted Nova
peer sync, within the owning profile. This is not encrypted-at-rest storage.
Sign-in must verify the stored account before retained work can use credentials.
API requests attach credentials only to the service's official HTTPS endpoint.

The actor rereads durable credentials and scope before each request. The UI apply
step repeats account/scope checks under the sync-store lock and rejects stale
native progress snapshots. Remote library/history patches use the ordered
persistence worker under `ApplyingGuard` and the external-origin guard. This
preserves newer local edits and prevents imported history from becoming a fresh
local upload action. Retry delays grow from five seconds to five minutes;
server rate limits cannot be bypassed by the manual sync button.

## Where to look

- `crates/list-services/src/{api,models}.rs`: bounded official HTTPS clients,
  normalized catalog identities, library/history/checkpoint operations and
  Simkl device authorization.
- `src/app/list_services.rs`: profile actor, credentials, explicit scope,
  conservative identity projection, additive merge and durable apply checks.
- `src/app/list_services_tests.rs`: scope, history authorization, queue
  cancellation, account separation, resume validity and mocked actor regressions.
  Account-preference migration and retained identity-exclusion regressions are
  inline in `src/app/list_services.rs::policy_regressions`.
- `src/app/tracking.rs::journal_progress`: semantic watched-transition hook.
- `crates/ui/list-services.slint`: account settings and full-library opt-in.
- `crates/ui/list-services-title.slint`: selected-title opt-in and exclusion.

Fixture and headless tests do not establish live-account compatibility. No live
Simkl or MDBList account mutation has been performed during this implementation.

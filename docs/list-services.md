# Simkl, MDBList and Trakt sync

Settings → Integrations includes separate Simkl, MDBList and Trakt account cards. Each
account belongs to the selected Nova profile. Connecting verifies the remote
account and saves its sign-in; it does not enable a whole-library upload.

The current Nova interface accepts an MDBList API key from
[account preferences](https://mdblist.com/preferences/). Simkl currently requires
a registered public application client ID and its access token, available through
[Simkl application settings](https://simkl.com/settings/developer/). Nova does not
ship a fabricated application registration. For AUTH V2 browser login, a public
mobile/desktop registration requires a redirect URI and no client secret;
device-only registrations omit redirect URIs. See the [official setup guide](https://api.simkl.org/api-reference/auth).
The API crate contains preliminary device/refresh helpers; AUTH V2 device PKCE
and browser/device-code sign-in still need completion and UI wiring. Automatic
token refresh is not yet wired into the account cards. An expired
or rejected credential requires reconnecting. Tokens are never included in
statuses or diagnostics.

Trakt uses browser device-code sign-in with your registered public application
client ID from [Trakt developer apps](https://developer.trakt.tv/apps). Register
`urn:ietf:wg:oauth:2.0:oob` as its redirect URI; Nova uses that URI when renewing
the device grant. No client secret is needed or embedded. The code and official
activation URL appear in the account status, and the browser opens automatically.
Polling honors the service interval, slowdown responses, expiry and cancellation.
Android keeps the shared authorization service alive while sign-in is pending.
Refresh tokens rotate atomically before subsequent network reads. A rejected
shared refresh waits with bounded backoff for newer paired-device credentials;
it never disconnects the account or overwrites a newer connection/tombstone.
See the [current official Trakt authentication guide](https://developer.trakt.tv/docs/authentication-oauth)
and [OpenAPI schema](https://developer.trakt.tv/openapi.json).

## Choosing scope

Open a saved title's Tracking page and choose Simkl, MDBList or Trakt. **Start syncing
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

In account settings, choose **Full library sync** in **Sync mode**, then choose
**Start full-library sync**. The selector is only a draft until that explicit
confirmation. This authorizes importing and exporting all supported saved titles
and watched history for that account. Choosing **Selected titles** returns to
the account's retained per-title choices and exclusions. A selected-title pass
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
marked synchronized or uploaded by guesswork. A rejected individual media
mutation (including a successful HTTP response with unresolved IDs) leaves that
title pending and continues with later titles. Authentication, rate limits,
transport failures, cancellation and local-save failures still stop the pass.

Unfinished playback can export a paused scrobble checkpoint when Nova has a real
finite duration and a proven episode identity. Only the most recently updated
local resume candidate is considered for each title. Matching episode positions
merge toward the greater percentage. Different remote/local episode checkpoints
remain pending instead of overwriting one another. Import converts a remote
percentage to seconds only when the matching local episode already has a real
duration; missing durations remain pending. Simkl/MDBList use checkpoint sync without continuous player event streaming.
Trakt additionally receives real player start, unpause, pause and stop events,
subject to the same selected/full scope, exclusions, automatic toggle and
verified account. Loading never emits a scrobble. Live playback skips competing
bulk watched/checkpoint writes, and repeated stop acknowledgments do not add
duplicate plays. Trakt records a watch above its documented 80% stop threshold;
shorter stops retain resume progress. Episode scrobbles resolve the exact
episode ID against the confirmed parent, never the show ID or display numbering.
These observations and pending device codes are process-local and profile-owned.
A bounded 16-event queue retains transiently failed player transitions, including
terminal stops, until retry. Real frames update the live snapshot even without
a state change, so finishing verification or enabling consent mid-play can
deliver Start once. Successfully sent session/action pairs suppress repeat
notifications; profile/account generations invalidate stale queued events.
Player observations use their own queue and never cancel library HTTP requests.
Native EOF sends Stop while the final frame/end menu remains open; later pause
ticks and close cannot send it again. Seeking back or replaying rearms a new
viewing cycle on the same native load. Invalid or unsupported remote items are
quarantined for that viewing cycle so another title can proceed; transport and
rate-limit failures keep their bounded retry behavior. Movie scrobbles use a
canonical Trakt summary whose IDs match the confirmed source and include the
required title/year/IDs. A 64-entry, one-day public metadata cache avoids
refetching that summary for every start/pause/stop.

## Persistence and paired devices

The profile-qualified `list_services` generic peer domain contains version-1
JSON `connection:0` (Simkl), `connection:1` (MDBList) and `connection:2` (Trakt) records. Trakt records optionally include its refresh token and access expiry; older records without these fields remain readable. Each includes the
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

- `crates/list-services/src/{api,models,trakt}.rs`: bounded official HTTPS clients,
  normalized catalog identities, library/history/checkpoint operations and
  Simkl device authorization and current Trakt auth/pagination/scrobble protocol.
- `src/app/list_services_trakt.rs`: Trakt browser device flow, shared refresh
  rotation, stale peer guards and scoped real player observations.
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
Simkl, MDBList or Trakt account mutation has been performed during this implementation.

# nova-list-services

Official Simkl and MDBList account, list, watched-history and playback APIs,
independent of Slint, storage and the app's sync engine. `Client` accepts an
`Arc<dyn Transport>`; production uses `HttpsTransport`, tests inject responses.
Requests use HTTPS to only the two official API hosts, disable redirects, time
out after 30 seconds, and bound each JSON body to 16 MiB. Library requests stop
at 100 pages / 50,000 titles. Failures never contain URLs, credentials or bodies.
`Secret` zeroizes strings on drop and redacts Debug output. Credentials remain
sensitive when serialized; the caller owns persistence and account isolation.

## App contract

- `Credentials { service, client_id, token }`: Simkl requires a real registered
  public app client ID and user access token. MDBList accepts a personal API key
  as `token` and an empty `client_id`. No borrowed app IDs or client secrets.
- `verify` validates credentials and returns a stable account ID/name.
- `begin_device`, `poll_device`, `refresh` implement Simkl AUTH V2's public
  device flow. The app must respect the returned polling interval and expiry;
  `SlowDown` adds five seconds. Request scopes are `media:read media:write`.
  Each device should have its own authorization: refreshing invalidates the old
  access token. Persist the returned token pair atomically.
- Retain one `Client` through a signed-in session. `library` checks Simkl
  `/sync/activities` first, reuses unchanged snapshots, sends `date_from` on
  subsequent pulls, and reconciles removed titles using an IDs-only snapshot.
  Commit the new checkpoint only after all requests succeed. The initial pulls
  run sequentially per media type. Changing credentials clears the cache.
  Save `simkl_checkpoint` device-locally and call `restore_simkl_checkpoint`
  after verifying the same account on restart, so relaunches use deltas too.
- MDBList `library` returns watchlist + collection + watched history + paused
  playback. It follows cursors (or offset/`X-Has-More` fallback) to completion.
  There is no claimed automatic custom-list membership synchronization;
  `lists`, `list_items`, `set_list_item` expose that separately.
- `RemoteTitle.episodes` contains exact watched episodes. Neither aggregate
  episode counts nor collected episodes are expanded into watched history.
  `progress` is a percentage and `playback_episode` identifies its exact target;
  the app must obtain a real duration before converting it into seconds.
- The caller gates all imports and writes on its selected-title/full-library
  policy. Merely linking an account does not authorize a whole-library import.
  The API client never mutates Nova state or chooses synchronization scope.
- Treat an incomplete/failed fetch as an error, never an empty authoritative
  library. Successful HTTP mutations with `not_found` or `errors` are rejected.

## Capability boundaries

Simkl supports five list statuses, watched/unwatched movies or exact episodes,
and playback start/pause/stop. Whole-title list removal is intentionally
`Unsupported`; unwatching is a separate supported action. Simkl custom lists
are currently read-only beta with paid-account restrictions and are not
exposed here. Series completion never guesses individual watched episodes.

MDBList supports watchlist add/remove, collected-library add/remove, exact
watched history add/remove, start/pause/stop playback, and owned static list
item add/remove. MDBList has no equivalent to Simkl's Watching/OnHold list
statuses in this interface; unsupported status changes fail before networking.
Dynamic/foreign lists are read-only, and API quotas apply to all account apps.
The current official API supports progress and history; old API summaries that
claim MDBList is only a ratings/list reader are outdated.

## Sources inspected

- [Simkl current reference](https://api.simkl.org/) and
  [required incremental sync workflow](https://api.simkl.org/guides/sync).
- [Simkl AUTH V2 device flow](https://api.simkl.org/api-reference/oauth2-device).
- [MDBList official OpenAPI schema](https://api.mdblist.com/schema/?format=json)
  and [authentication guide](https://api.mdblist.com/docs/authentication/).
- [Nuvio Mobile Simkl and MDBList integrations](https://github.com/NuvioMedia/NuvioMobile/tree/cmp-rewrite/composeApp/src/commonMain/kotlin/com/nuvio/app/features),
  used to verify current endpoint distinctions and episode payload shapes.

No live account calls are needed for unit tests. Tests cover wire shapes,
exact episode writes, rejected partial successes, auth boundaries, pagination
failure, unchanged Simkl activity snapshots and service-specific scrobbles.

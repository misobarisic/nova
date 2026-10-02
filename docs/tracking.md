# Anime tracking

Nova can send progress for explicitly linked anime releases to MyAnimeList and
AniList. Open **Settings → Tracking**, connect a service, then open a title and
choose its **Tracking** action. Connection alone does not link any titles.

## Connecting

Sign-in is saved, shared with paired devices through Nova sync, and restored
after restarting Nova. Saved tokens
are verified against the stored account before retained updates can resume.
Expired MAL access tokens refresh automatically; renewed access and refresh tokens
are saved immediately. Offline restoration retries without deleting saved sign-in.
Only one account is active per service. Switching accounts retains the previous
account's links but does not send its work as the new account.

Nova includes its public application IDs and redirect URLs as defaults.
Existing custom registrations remain available in the connection form. Never
enter a client secret. MAL uses a public/native application, plain PKCE, and a
loopback redirect such as `http://127.0.0.1:53926/callback`. The port must be available locally.
AniList defaults to manual token sign-in using its registered PIN redirect
`https://anilist.co/api/v2/oauth/pin`. Nova opens the minimal authorization URL
with only `client_id` and `response_type=token`; AniList uses the redirect in its
application settings. Approve Nova, copy the displayed token, paste it into the
**AniList token** field, and select **Finish sign-in**. Custom registrations can
still use a loopback implicit callback with state checking. A loopback return can
also be pasted as its full URL if automatic capture fails. Returned identity is verified before
any link can use that session. Sign-in attempts expire after five minutes.

The automatic-tracking checkbox applies to the service's linked releases.
Pausing it holds queued automatic work and checkpoints new history without
uploading it. Manual tracker edits remain available. Resuming allows retained
automatic work to continue; it does not replay history from the paused period.
Disconnect stops future requests and removes shared saved sign-in and in-memory
credentials. The removal propagates to paired devices on their next sync.
Links and queued edits remain. An already sent request may finish.

## Credential storage and future protection

**Current storage is plaintext**, by explicit product choice. Versioned JSON lives
in the local `nova.redb` database under `tracking:credentials:mal:v1` and
`tracking:credentials:anilist:v1`, separate from public client registrations and
the tracking-state envelope. Each record contains the verified account, public
client registration used for refresh, access token, optional refresh token, and
access-token expiry. Client secrets are not required or stored.

`crates/tracking/src/credentials.rs` owns bounded decoding, temporary secret
zeroization, identity verification, and refresh rotation. `Store::save_with_connection`
commits account selection and the credential record atomically. The application
actor restores only the saved active account for each service, obeys service
cooldowns, and checks sign-in generations before activating a restored session.
Disconnect deletes that service's record and active choice in one transaction;
Reset local tracking deletes both credential records without copying them into
recovery backups. Unreadable credential records remain available for deliberate
reconnection and are never printed in errors.

The `tracking` peer domain shares these records with explicitly paired Nova
devices, including public refresh registration, tokens, expiry and account choice.
They never enter addon configuration or the ordinary settings domain. Tracker
API requests send bearer credentials only to the official tracker endpoints.
This is not encrypted-at-rest storage; a copy of the database can expose tokens.

**Future addition:** replace plaintext token persistence with the desktop OS
credential store and Android Keystore-backed encryption. Preserve account/client
binding, immediate refresh-token rotation, atomic activation, disconnect/reset
cleanup, and offline retry behavior. Migrate existing plaintext records only after
protected storage confirms a successful write, then delete the old token values.
Protect both credential rows and credential-bearing sync rows/baselines at rest.
Keep tokens out of ordinary settings, logs, and recovery backups;
report protected-store failures rather than silently falling back to plaintext.
Restart, refresh-rotation, paired-device propagation, wrong-account, deletion, and failed-save regressions
must continue to pass with an injected credential backend.

## Paired-device tracking sync

With Nova sync enabled and devices paired, the `tracking` domain carries version-1
JSON records: one link per typed source/account/release identity, one paused
preference per service, and `credentials:mal` / `credentials:anilist` sign-in
records. Start tracking, alignment repair, unlink, pause/resume, refresh-token
rotation and Disconnect all propagate. Local-only tracking state remains a
schema-3 envelope; the generic sync wire structure and ALPN do not change.

Receiving a link preserves stable episode IDs but uses the receiving device's
account generation, mapping revision and current history checkpoint. Tokens are
verified against their stored account before requests can resume. Received
credentials and links commit together. Existing watched flags are checkpointed;
receipt does not queue a history upload. Pending API operations are never copied
between devices. Ordinary remote apply uses `ApplyingGuard` and does not echo.
Concurrent conflicting links are disabled across the mesh for explicit alignment
repair; invalid or unsupported records remain pending instead of being erased.

Unlinks and shared sign-in removals use sync tombstones, so offline peers cannot
restore an old link merely by returning. Projection acknowledges the captured
domain digest after the local commit; a newer concurrent merge remains pending.
Durable local changes that have not reached the sync store are retried before
applying remote records and after restart. Reset local tracking also publishes
removals for the shared configuration; recovery backups retain local state and
journal records, rather than live tokens.

Android browser sign-in starts a separate `NovaAuthService` foreground service
before opening the browser and waits for foreground promotion. Its notification
returns to Nova; a bounded wake lock protects callback reception, token exchange,
account verification and credential persistence while the Activity is backgrounded.
The service uses Android's `dataSync` type for account data exchange, independently
of download service ownership. Each pending login owns a lease; finishing,
cancelling, failing, expiring, disconnecting or shutting down releases that lease.
Overlapping MAL/AniList logins keep the service until the last lease ends.

Approval expires after five minutes. Callback failures and expiry report a
reconnect message; stale callbacks cannot cancel a newer login. Replacement
logins briefly wait for a cancelled receiver to release the registered port. A renewed
370-second service watchdog also bounds a stalled actor, allowing the approval
window plus two 30-second API calls. Android's `onTimeout` stops the service.
Pending OAuth state remains in memory: force-stopping or killing the process
requires a fresh login. Saved completed sign-in survives restarts and syncs with
paired devices. Device validation should approve MAL in the external browser,
return to Nova, verify Connected, then restart Nova and verify restoration; also
check cancellation and concurrent downloads. Only Cargo tests were run locally.

Android can also receive an existing desktop sign-in through pairing without
repeating browser approval. See [Android foreground-service types](https://developer.android.com/develop/background-work/services/fgs/service-types)
for the platform service contract.

## Linking and alignment

Unlinked titles open a **Review setup** for the whole library title. Nova selects
the first ranked tracker search result by default and explores its official
prequel/sequel relationships. Existing confirmed links remain the anchor when
adding coverage. This selection prepares a draft; only **Start tracking** enables
it. Use **Choose another release** to see alternatives and search by title, ID or
official URL if the default does not match. Switching the service rebuilds that
service's independent proposal.

The review groups releases by library season and shows readable episode ranges.
For example, Demon Slayer's merged second season maps episodes 1–7 to the Mugen
Train TV entry and 8–18 to Entertainment District episodes 1–11. Mushoku Tensei's
23-episode first season maps to 11- and 12-episode parts; its episode-zero TV
special can connect related main releases without consuming a regular episode.
MAL's partial release dates (year or year/month) are supported, including upcoming
Mushoku Tensei and The Apothecary Diaries entries.

Known episode totals, structured numbering and air dates guide the proposed
splits. An exact count fit without full dates is marked **Check this split** for
your review. Unknown-count ongoing releases only propose identifiable aired
episodes; forecast episodes remain unassigned. Numbering gaps, ambiguous branches,
conflicting existing links and unknown boundaries are left unresolved. Specials
and episode zero are excluded from automatic coverage. Discovery stops once the
regular episodes are covered, with a maximum of 32 release-detail lookups per
proposal; metadata is reused from the bounded local cache. There is no external
mapping dataset or silent linking.

Use **Adjust** to review or change an individual range. The range fields are
prefilled from the proposal; **Edit individual episodes** reveals stable-ID
assignments when numbering needs correction. Unmapped episodes never update a
tracker. Back from an adjustment returns to the review.

**Start tracking** confirms all proposed links in one local transaction. The
**Include watched episodes** checkbox is unchecked by default; selecting it shows
per-release progress previews and queues existing watched progress together with
the links. Leaving it unchecked checkpoints history and tracks future watched
events only. Fresh metadata/list reads and account checks precede the commit; a
failure cannot leave half a merged season linked. Remote delivery starts afterward
and preserves higher remote progress and unrelated fields.

Linked titles open a compact overview of coverage, progress and delivery state.
Use **Add missing releases** to propose additional coverage without silently
replacing existing assignments. Existing authorized queued work is preserved when
coverage is extended. **More** reveals entry edits, alignment repair, history
upload, retry, service links, automatic-status restoration and unlinking. Repair
and unlink still require confirmation; remaps discard old unsent work.

Several sources can contribute to the same release without counting an ordinal
twice. Films use one source row. Existing links, pending work and tracker account
separation retain the previous local schema. Newly discovered episodes and changed
coverage require another review; reloading suggestions captures fresh source
metadata. Source changes invalidate an open review even if the episode IDs stay
the same.

## Progress and edits

A new watched transition uses the highest accepted mapped target ordinal. Sparse
history can therefore advance progress past an unwatched gap. Automatic updates
preserve higher remote progress. Local unwatch does not lower the tracker; use an
explicit progress edit to decrease it. Decreases establish a new queue revision
and checkpoint old history so stale retries cannot raise the value again.

Internal playback starts can move a planning entry to Watching and fill a missing
start date where supported. A watched event at a known, finished release's final
ordinal can set Completed and a missing finish date. Held, dropped, completed,
repeating, and manually pinned statuses are preserved. Progress-only writes
preserve scores, notes, privacy, custom lists, and unrelated dates. External-player
launches have no reliable watched feedback and do not count as playback starts.

The entry editor supports progress, status, and service-specific scores. AniList
scores follow the verified account preference, including decimal scoring. AniList
dates accept complete or partial dates; a checked empty date clears that field.
MAL's documented API exposes dates for reading but does not support writing them,
so those controls are disabled. Manually changed status/dates are pinned; **Restore
automatic status and dates** releases those pins for future eligible events.

## Delivery and recovery

Queued work survives restarts. Nova reads the current remote entry before each
mutation, serializes writes per target, respects service-wide cooldowns, and retries
transient failures with backoff. Authentication failures require reconnecting the
same verified account. Restarted in-flight requests are uncertain and are reread
before retry. The tracking sheet shows pending, sending, retry, authentication,
rejected, inactive-account, and alignment states. **Refresh / retry** cannot bypass
a server cooldown. A score-preference change requires correcting an older queued
score rather than silently converting it.

If refreshed source metadata removes an assigned episode ID, tracking pauses for
alignment review. **Edit alignment** requires another preview and discards unsent
work based on the old mapping. It checkpoints history again. Newly discovered,
unassigned episodes remain untracked until included explicitly. **Unlink** removes
that source binding; unsent work is discarded when no other enabled source uses
the target. Remote entries and local history remain unchanged.

Unreadable tracking records retain their original data and a deterministic
`tracking:quarantine:*` backup. Tracking stays unavailable until repaired or reset.
**Reset local tracking** requires confirmation, backs up the old state and journal,
and atomically resets local links/queues while holding the history writer lock.
It does not reset Nova history or remote tracker lists.

Tracking links, account choice, access/refresh tokens and automatic-tracking
preferences are shared through the `tracking` sync domain. Resolver caches,
progress projections, checkpoints, pending requests and attempt leases stay local.
Paired-device watched changes update checkpoints but do not authorize uploads.
Remote tracker values never import library membership or mark Nova episodes watched.
Live sign-in and real account mutations still require interactive verification
with the registered applications; fixture and headless UI tests do not substitute
for that final validation.

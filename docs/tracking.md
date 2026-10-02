# Anime tracking

Nova can send progress for explicitly linked anime releases to MyAnimeList and
AniList. Open **Settings → Tracking**, connect a service, then open a title and
choose its **Tracking** action. Connection alone does not link any titles.

## Connecting

Sign-in is saved on this device and restored after restarting Nova. Saved tokens
are verified against the stored account before retained updates can resume.
Expired MAL access tokens refresh automatically; renewed access and refresh tokens
are saved immediately. Offline restoration retries without deleting saved sign-in.
Only one account is active per service. Switching accounts retains the previous
account's links but does not send its work as the new account.

Nova includes its public application IDs and redirect URLs as defaults.
Existing custom registrations remain available in the connection form. Never
enter a client secret. MAL uses a public/native application, plain PKCE, and a
loopback redirect such as `http://127.0.0.1:53926/callback`. The port must be available locally.
AniList supports a loopback implicit callback or its documented PIN redirect
`https://anilist.co/api/v2/oauth/pin`. For PIN sign-in, paste the browser's token
into the password field and finish sign-in. A loopback return can also be pasted
as its full URL if automatic capture fails. Returned identity is verified before
any link can use that session. Sign-in attempts expire after five minutes.

The automatic-tracking checkbox applies to the service's linked releases.
Pausing it holds queued automatic work and checkpoints new history without
uploading it. Manual tracker edits remain available. Resuming allows retained
automatic work to continue; it does not replay history from the paused period.
Disconnect stops future requests and removes saved and in-memory credentials.
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

These records remain device-local: they are excluded from peer sync and never
sent to addons. Bearer credentials are sent only to the official tracker endpoints.
This is not encrypted-at-rest storage; a copy of the database can expose tokens.

**Future addition:** replace plaintext token persistence with the desktop OS
credential store and Android Keystore-backed encryption. Preserve account/client
binding, immediate refresh-token rotation, atomic activation, disconnect/reset
cleanup, and offline retry behavior. Migrate existing plaintext records only after
protected storage confirms a successful write, then delete the old token values.
Keep tokens out of ordinary settings, sync records, logs, and recovery backups;
report protected-store failures rather than silently falling back to plaintext.
Restart, refresh-rotation, wrong-account, deletion, and failed-save regressions
must continue to pass with an injected credential backend.

## Linking and alignment

Unlinked titles open a **Review setup** for the whole library title. Nova finds a
starting release from a unique service ID/cross-reference or a matching official
title/alias and year, then explores official prequel/sequel relationships. If the
starting match is ambiguous, choose a release or search by title, ID or official URL.
Switching the service rebuilds that service's independent proposal.

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

Tracking records, credentials, cached metadata, and pending work are device-local.
Paired-device watched changes update checkpoints but do not authorize uploads.
Remote tracker values never import library membership or mark Nova episodes watched.
Live sign-in and real account mutations still require interactive verification
with the registered applications; fixture and headless UI tests do not substitute
for that final validation.

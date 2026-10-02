# Anime tracking

Nova can send progress for explicitly linked anime releases to MyAnimeList and
AniList. Open **Settings → Tracking**, connect a service, then open a title and
choose its **Tracking** action. Connection alone does not link any titles.

## Connecting

Connections currently last for the app session. Access and refresh tokens stay
in memory and never enter addons, peer sync, ordinary settings, or the local
tracking database. Reconnect after restarting Nova; links and queued edits remain.
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
Disconnect stops future requests and discards in-memory credentials. An already
sent request may finish.

## Linking and alignment

Opening Tracking automatically suggests releases for a connected service (MAL
first when both are connected). Switching the service reloads suggestions;
**Suggest releases** reloads them without a typed query, reusing unexpired
metadata from the bounded cache. Already linked active
releases are omitted from suggestions, while explicit manual searches can still
find them for repair. Each candidate explains its source ID, cross-reference, or
title/year evidence. Suggestions do not select a release, create a link, or upload
history; selection and confirmed alignment are still required.

Search suggestions prefer an explicitly supplied ID for the selected service,
then an official MAL/AniList cross-reference. Otherwise Nova searches the source
title and bounded aliases, ranking release format, year, and episode-count hints.
IMDb/TMDB and provider-only sources use this same title/manual path. No third-party
mapping database is downloaded: the reviewed mapping projects did not provide a
clear redistribution license. A title or cross-reference identifies a candidate;
it never proves episode numbering or activates tracking by itself.

You can enter a title, a numeric ID for the selected service, an explicit
`mal:anime:123` / `anilist:anime:123` reference, or an official anime page URL.
Choose a release, select the first/last source row and target starting episode,
then preview. Rows use stable addon episode IDs; displayed canonical season and
episode labels help you review them without changing playback identity. Edit
individual target ordinals or leave a row empty to exclude it. Confirm the preview
to save the link. Conflicting active coverage and out-of-range ordinals are rejected.

A combined source can link different ranges to several tracker releases. Several
sources can also contribute to the same release without counting an ordinal twice.
MAL and AniList mappings are independent. Films use one source row. Unknown final
counts and ongoing releases do not automatically complete from the source count.

Linking preserves the remote entry and checkpoints existing Nova history; it does
not upload that history. **Apply Nova history** first shows the proposed value and
requires a separate confirmation. It only considers confirmed assignments.

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

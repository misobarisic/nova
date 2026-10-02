# Nova: MAL and AniList tracking implementation plan

Date: 2026-10-02\
Target repository: [misobarisic/nova](https://github.com/misobarisic/nova)\
Reviewed snapshot: [b014b98c233f29814ced15401ca0a00cff052918](https://github.com/misobarisic/nova/tree/b014b98c233f29814ced15401ca0a00cff052918)\
Status: implementation started; identity, mapping/projection domain, and local persistence implemented; account connection and tracking delivery remain pending.

### Implementation progress (2026-10-02)

The first implementation slice adds `crates/providers/src/ids.rs` as a shared identity layer. It models MAL/AniList anime and manga, IMDb, and TMDB movie/TV references, parses explicit prefixes and catalog URLs, and exposes missing/unique/conflicting destination lookup without requiring IMDb. Stremio normalization retains AniList and conflicting claims alongside backward-compatible fields; source IDs are unchanged. Provider matching rejects conflicting identity evidence, and the AniKoto bridge forwards AniList metadata.

The next slice adds `nova-tracking` with account-scoped targets, explicit stable-ID episode assignments, conflict/coverage validation, and a highest-accepted-ordinal progress projection. Link-time watched checkpoints suppress unapproved saved history; manual replacements establish a new revision and reject old acknowledgments. Versioned `tracking:state:v1` JSON stores accounts, targets, bindings, and projections locally; corrupt snapshots retain their primary record and receive a deterministic quarantine backup. The app loads this store on a startup worker and retains failures without blocking playback.

This is part of Phase 1, not completion of its gate. Provenance timestamps, intent/outbox models, mapping edit/revision handling, and UI/event wiring remain pending. The Phase 0 authorization/client registration and platform credential-storage gates remain open. No tracker API calls or remote list mutations are implemented.

## 1. Goal and agreed scope

Add Mihon-style tracking for anime watched in Nova. Users connect their own MyAnimeList and/or AniList account, explicitly link a Nova title or a subset of its episodes to the appropriate tracker entry, and let Nova send progress updates while they watch. Manual tracker edits and offline delivery are part of the feature.

Direct tracker-list integration with My Library is deferred. This version does not import a user's MAL/AniList lists, create local library entries from remote lists, mirror library membership, or rewrite Nova's watched history from tracker progress. Reading an existing linked tracker entry to display its values and preserve its progress is still necessary; that is different from importing a library.

The feature must work with addons that identify media using MAL, AniList, IMDb, TMDB, or provider-specific IDs. IMDb must not be a mandatory intermediate step. The same resolver must understand that different databases can describe different amounts of content under one ID.

### Included

- Account connection and disconnection for both services.
- Explicit per-title linking, with multiple tracker entries per Nova title when needed.
- Suggestions based on supplied IDs, known cross-service mappings, and metadata.
- Search, ID/URL entry, and manual episode alignment.
- Automatic updates from Nova's existing watched events.
- Manual progress, status, score, and start/finish date edits in a tracking panel.
- Durable offline updates, retries, token-expiry handling, and visible delivery state.
- Preservation of source IDs, local playback history, and confirmed user mappings.
- Mapping support for split seasons, merged seasons, parts, cours, and continuous numbering.
- Separate MAL and AniList bindings so one service's interpretation does not dictate the other's.
- A tested path on Nova's desktop and Android builds.

### Deferred

- Importing whole MAL/AniList lists into My Library.
- Two-way library membership or local watched-history synchronization with trackers.
- A unified franchise library, replacement metadata catalog, or new anime discovery system.
- Automatic bulk linking of every library entry.
- Full MAL-to-AniList account mirroring: Nova sends its own accepted changes to each linked service independently.
- Tracker support for live-action TV and movies that have no matching anime entry.
- Manga/chapter tracking until Nova has an applicable reading workflow.
- Automatic tracking of external-player activity without reliable playback feedback.
- A central Nova account or mandatory always-on Nova backend.
- Automatic repeat-count inference from Nova's playback-open count.

## 2. Reference behavior: what to borrow from Mihon

[Mihon's tracking guide](https://mihon.app/docs/guides/tracking) describes ordinary tracking as one-way from Mihon to a tracker. Users connect an account and add tracking to individual series. Read events update progress; offline changes are delivered later. Users can edit tracker fields and search with another title or an explicit ID.

Apply that interaction model to anime playback in Nova:

1. Connect a service under Settings → Tracking.
2. Open a title and choose Tracking.
3. Select Add tracking for MAL or AniList.
4. Confirm the remote entry and its episode coverage.
5. Watch normally or mark episodes watched.
6. See updated progress and delivery state in the tracking panel.

Nova needs additional mapping behavior because anime seasons and cours often have different boundaries across sources and trackers. A simple source-title → tracker-title association is insufficient.

This plan proposes Nova's behavior. Features such as the progress policy, account handling, and mapping editor below should not be read as claims about Mihon's internal implementation.

## 3. Existing Nova foundation and integration points

The reviewed repository already provides the following useful pieces.

| Existing component | Relevant behavior | Planned use |
| --- | --- | --- |
| `src/app.rs` | `EpisodeProgress`, `LibraryEntry`, `WatchStatus`, shared application state, and the 90% watched threshold | Reuse progress semantics; add tracking state separately |
| `src/app/playback.rs` | Progress persistence during playback and on close; watched transitions | Emit/reconcile tracking work when a meaningful watched or playback-start event occurs |
| `src/app/detail.rs` | Single-episode, season, and bulk watched actions | Route manual watched changes through the same tracking projection |
| `src/app/episodes.rs` | Progress keys, release-date checks, ordering, and derived episode state | Reuse helpers where their semantics fit tracker releases |
| `src/app/library.rs` | Bookmark membership, local status pins, and library display | Preserve existing behavior; library membership does not become tracker membership |
| `crates/providers/src/models.rs` | Provider/source identities, aliases, external IDs, and contextual stream lookup | Extend typed identity handling and supply mapping evidence |
| `crates/providers/src/matching.rs` | Conservative matching by explicit IDs or normalized title/alias; ambiguity is retained | Reuse matching principles; do not turn a title match into proof of episode alignment |
| `crates/providers/src/stremio.rs` | Adaptation of addon metadata and extraction of IMDb/TMDB/MAL fields | Add AniList and typed TMDB handling; retain supplied IDs |
| `crates/providers/src/anikoto.rs` and its plugin | Confirmed Cinemeta episode mappings and `novaStreamIds` aliases while native IDs remain stable | Reuse evidence and tested mapping rules where applicable |
| `crates/storage` and `src/app/io.rs` | Existing durable storage and persistence paths | Persist links, projections, and pending delivery work |
| `src/app/sync.rs` and `crates/sync` | Existing Nova-to-Nova sync and progress merges | Keep credential state local; assess non-secret mapping sync separately |
| `crates/ui/*.slint`, `types.slint`, `appwindow.slint`, and `src/app/run.rs` | UI components, exposed properties/callbacks, and application wiring | Add settings and a detail-page tracking sheet using the established pattern |

### Important current limitations

- `ExternalIds` has explicit IMDb, TMDB, and MAL fields, plus an open `other` map. It does not currently model AniList as a first-class field.
- TMDB IDs currently lack a typed movie/TV distinction in that model.
- The existing title/ID matcher compares items of the same media type. Anime tracker formats need an intentional conversion to Nova's media types; `ANIME` is not automatically the same string as `series`.
- App library/progress identity still depends on existing addon IDs. The new feature must not rename those IDs or rewrite progress keys.
- `MetaHeader` is a limited display snapshot. Required tracking identity cannot depend only on fields that disappear when the original addon response is no longer in memory.
- Library Completed means all known episodes of the source item are watched. Tracker completion must instead refer to a particular mapped release.
- External-player launch paths do not supply the same progress observations as Nova's internal player. Launching an external player cannot imply that an episode was watched.
- A native episode's displayed season/number may already reflect a canonical mapping. Tracking must use stable episode IDs and explicit alignment, not infer the native playback number from a displayed label.

## 4. Core design invariants

1. **Source identity stays stable.** Addon IDs remain the identity for existing playback, history, downloads, and library entries.
2. **External IDs are typed.** Numeric values require a namespace and media kind where applicable.
3. **IMDb is optional.** A MAL-only or AniList-only addon can link to that tracker without first resolving IMDb.
4. **Identity and coverage are separate.** Knowing a MAL ID does not automatically prove how a combined source's episodes align with that entry.
5. **Tracking is explicitly enabled.** Account login and metadata search do not create or change remote list entries.
6. **One source can cover multiple releases.** One source card may have separate tracker bindings for seasons, parts, or specials.
7. **Several sources can cover one release.** Accepted progress from their mappings is combined without counting a target episode twice.
8. **A relation is not equivalence.** Sequel, adaptation, remake, summary, and side-story relationships must not be treated as the same release.
9. **Ambiguity remains visible.** Missing or contradictory evidence leads to manual linking, not an arbitrary match.
10. **Automatic progress never lowers accepted remote progress.** Deliberate lower values require an explicit tracker edit.
11. **Writes preserve unrelated fields.** A progress event does not reset scores, notes, dates, privacy, or custom lists.
12. **Offline delivery is durable.** Pending work survives a restart and is scoped to the correct service and account.
13. **Credentials belong to the application.** Addon scripts never receive tracker tokens.
14. **Remote reads do not rewrite local history.** Displaying progress 8 on MAL does not mark Nova episodes 1–8 watched.
15. **Mappings survive ordinary refreshes.** Persist confirmed links; invalidate or review coverage when relevant evidence actually changes.

## 5. Identity model and ID normalization

### 5.1 Three identities

Keep three concepts separate:

| Identity | Example | Meaning |
| --- | --- | --- |
| Local source identity | Existing Nova item ID plus retained source/provider context | The source object that Nova opens and whose episodes it plays |
| External media identity | `mal:anime:100001`, `anilist:anime:200001`, `imdb:tt0000001`, `tmdb:tv:300001` | A typed reference in an external catalog |
| Tracker list identity | Service + authenticated account ID + remote media ID, with a remote list-entry ID when available | The user's list record that Nova may update |

The numeric examples are illustrative. They are not verified references to real titles.

### 5.2 Typed external IDs

Introduce a reusable representation for known namespaces:

- MAL anime and MAL manga.
- AniList anime and AniList manga.
- IMDb title IDs.
- TMDB TV and TMDB movie IDs.
- Optional registered namespaces, such as AniDB or TVDB, when a mapping source needs them.
- Provider-specific source IDs, which remain namespaced source identity rather than global catalog identity.

Prefer an enum or another type-safe representation for known namespaces. Preserve unknown namespace/value pairs as opaque evidence; do not claim support for resolving an unknown namespace automatically.

### 5.3 Normalize at the addon boundary

Support recognized forms such as explicitly prefixed IDs, separate metadata fields, and official media URLs. Document an unambiguous internal format even when legacy addon formats vary.

Normalization requirements:

- Recognize known MAL and AniList field aliases and preserve all valid supplied IDs.
- Parse IMDb IDs only when the namespace and syntax establish that meaning.
- Distinguish TMDB movies from TMDB TV series using explicit type information.
- Reject zero, negative, malformed, overflowed, or unexpectedly large numeric values.
- Do not interpret a bare `12345` as MAL merely because the content looks like anime.
- Retain unsupported fields for later compatibility instead of discarding them.
- If two fields claim conflicting IDs in the same namespace, retain the conflict and suspend automatic selection.
- Record the supplying addon/provider and retrieval time for provenance.
- Bound the size and count of accepted IDs/aliases from untrusted addon metadata.

Example outcomes:

| Addon input | Expected behavior |
| --- | --- |
| Explicit MAL anime ID | Fetch that MAL entry directly; optionally resolve AniList independently |
| Explicit AniList anime ID | Fetch that AniList entry directly; use its MAL cross-reference when available |
| IMDb series ID | Resolve candidate anime releases with season/episode context |
| TMDB TV ID | Preserve TV kind and resolve the relevant season or release |
| Provider slug only | Search metadata candidates and provide manual linking |
| Bare numeric ID with no declared namespace | Keep it as source identity; require namespace evidence or manual entry |
| Conflicting MAL IDs from metadata fields | Show a conflict; do not quietly pick the first field |

## 6. Resolution strategy

### 6.1 Resolve toward the requested destination

Use one resolver interface with a requested destination and coverage context. A request to link MAL should prefer an existing MAL ID; a request to link AniList should prefer an AniList ID. Resolving a title must not require filling every possible catalog ID first.

Conceptual request:

    resolve(source_identity, supplied_ids, metadata, requested_service, coverage)

Return an explicit result:

- Resolved candidate with evidence and applicable coverage.
- Several candidates requiring selection.
- A conflict between supplied or previously verified IDs.
- No known mapping, with manual search/ID entry available.
- Temporarily unavailable because a dependency is offline or rate-limited.
- Unsupported media kind or namespace.

Distinguish a verified media identity from a verified episode alignment. A result may resolve the title but still require alignment.

### 6.2 Lookup order

1. Reuse an applicable user-confirmed mapping.
2. Validate a directly supplied ID for the requested service.
3. Resolve through explicit service cross-references, such as AniList's `idMal` when present.
4. Consult a known cross-catalog mapping source with the correct release/season scope.
5. Search with title aliases and discriminating metadata.
6. Ask the user to choose or enter the exact ID and coverage.

Do not assume MAL and AniList numeric IDs are equal. Do not assume a cross-reference is always present or establishes a universal one-to-one correspondence.

### 6.3 Candidate validation

Assess titles and aliases together with media kind, format, start year/date, episode count, season/part/cour indicators, and existing identity evidence.

- English, romanized, and native titles may refer to the same release.
- A missing year is unknown; it is not automatically a conflicting year.
- A different year can reflect a later season or cour and requires coverage-aware examination.
- Counts help prove alignment only when their source and completeness are known.
- An ongoing release may have an unknown final count.
- Anime TV, film, OVA, ONA, and special formats must not be merged merely because names match.
- Related-media edges may suggest the next candidate but cannot prove equivalence or episode numbering.
- Fuzzy title matching ranks search results; it does not silently establish a writable tracker binding.

### 6.4 Mapping sources and caching

Evaluate [Fribb/anime-lists](https://github.com/Fribb/anime-lists) and [Anime-Lists/anime-lists](https://github.com/Anime-Lists/anime-lists) as possible cross-catalog evidence. Assess coverage, licensing, update cadence, data format, and the exact meaning of offsets before choosing a distribution method.

Design the mapping-source interface so the first tracker implementation can work without downloading a whole external database. Start with explicit IDs, direct cross-references, and manual linking; add curated mappings as an optional improvement.

Persist user-confirmed mappings separately from expiring search and metadata caches. Cache positive lookups longer than unsuccessful searches, apply bounded storage limits, and retain the data-source/version provenance. A network failure must not be cached as proof that no mapping exists.

Do not scrape service HTML as the primary integration path. Keep service-specific reads and writes in API adapters.

### 6.5 Meaning of “resolving should always work”

Every supported ID namespace must enter the same resolution workflow. MAL-only metadata must not fail because there is no IMDb ID. However, no resolver can guarantee a correct automatic match when data is absent, contradictory, or describes different releases.

The product guarantee is a usable resolution path: automatic where evidence establishes the mapping, otherwise explicit manual linking that is remembered. Incomplete resolution should not block ordinary Nova playback.

## 7. Episode and release coverage model

### 7.1 Separate media identity from episode alignment

A binding needs both:

- The tracker media/list identity.
- The source episodes or range that contribute to that tracker release.

Use stable source episode IDs as the final lookup keys. Displayed season and episode numbers are useful labels and mapping evidence, but cannot replace stable IDs.

Represent mappings as explicit episode assignments, with optional range rules for editing convenience. A simple offset is an optimization for a proven contiguous sequence, not the universal model.

### 7.2 Required mapping forms

- One source season maps directly to one tracker release.
- A source range maps to a tracker release with an offset.
- Several source ranges or source titles contribute to one tracker release.
- Individual source episodes have explicit assignments when numbering is irregular.
- Specials remain unassigned or map to their own tracker entries.
- Known unresolved episodes are retained as unresolved instead of guessed.

A single source episode ordinarily maps to one episode within one release per service. Do not automatically count one combined video as several tracker episodes. If an addon packages multiple episodes in one video, defer automatic tracking for that case or require an explicit, documented multi-episode coverage rule.

### 7.3 Merged season example

Suppose an addon has a 24-episode season while the tracker has two 12-episode releases.

| Source coverage | Tracker release | Target episode calculation |
| --- | --- | --- |
| Source S1E1–E12 | Part 1 | Source number |
| Source S1E13–E24 | Part 2 | Source number minus 12 |
| Source S2E1–E12 | Next season | Source number |
| Source specials | Separate entry, or unresolved | Explicit assignment |

Watching source S1E15 updates Part 2 to episode 3. It does not update Part 1 to 15 or mark the whole franchise complete.

### 7.4 Split source example

An addon might provide Part A with episodes 1–12 and Part B with episodes 1–12, while a tracker has one 24-episode entry.

- Part A episode 1 maps to target episode 1.
- Part B episode 1 maps to target episode 13.
- Both bindings contribute to the same service/account/media target.
- Duplicate observations of target episode 13 do not increment a count twice.
- A score belongs to the shared tracker target, not separately to each source contribution.

### 7.5 Independent MAL/AniList alignment

Store coverage per service target. If MAL uses two releases and AniList uses one, use two MAL targets and one AniList target. Cross-service IDs help find candidates; they do not force identical release boundaries.

### 7.6 Mapping evidence and validation

Reuse AniKoto's confirmed canonical aliases as evidence when available, but keep tracker mappings distinct from `novaStreamIds`. Stream routing and tracker progress have different destinations and coverage rules.

Validate before activating automatic tracking:

- No impossible target episode number or negative/overflowed offset.
- No accidental overlap between different targets for the same source episode within one service.
- Duplicate source contributions to the same target episode are identified and deduplicated.
- No assumption that every cour has 12 or 13 episodes.
- No automatic conversion of season zero into the main TV release.
- Numbering gaps, recaps, irregular specials, and conflicting episode titles remain visible.
- Incomplete source metadata does not imply the final release is complete.
- A later metadata refresh can resolve newly added episodes without invalidating unchanged confirmed assignments.

### 7.7 Manual alignment UI

Provide a normal linking flow for common one-to-one cases and an expandable alignment editor for harder cases.

The editor should allow the user to:

1. Select source season/range or individual episodes.
2. Select the remote release.
3. Choose the target starting episode, or edit explicit assignments.
4. Preview source title/episode → tracker release/episode.
5. See gaps, overlaps, and the expected progress projection.
6. Save and confirm tracking.

Show first and last assignments and several intermediate examples for a range. Changes to an active mapping require a preview; they must not immediately send historical watched episodes to a new release.

## 8. Proposed tracking domain model

Keep the following records separate rather than putting account tokens or tracker-specific fields into `LibraryEntry`.

| Record | Purpose | Important fields |
| --- | --- | --- |
| Account connection | Local account/session configuration | Service, remote user ID, display name, credential reference, authentication state |
| Identity evidence | Typed external IDs and how they were established | Source reference, namespace/kind/value, provenance, confidence category, observed time |
| Tracking binding | Permission to track a source's specified coverage | Binding ID, source reference, target key, enabled state, mapping revision |
| Episode assignment | Source-to-target alignment | Source episode ID, target episode ordinal, assignment evidence |
| Target state | Shared projection for all bindings pointing to one list entry | Service/account/media key, remote entry ID, observed remote values, accepted progress, local revision |
| Pending patch | Durable remote work | Target key, account generation, binding/mapping revisions, changed fields, intent, retry state |
| Projection checkpoint | Reconcile progress after restarts without replaying old history | Watched-state observations and semantic change/revision checkpoints |

The target key should include service, media kind, remote account identity, and remote media ID. AniList's remote list-entry ID should be retained separately from its media ID. Tokens and refresh tokens are credentials, not part of these ordinary records.

### Suggested module boundary

Create `crates/tracking` / `nova-tracking` for service adapters, typed IDs, mapping validation, progress projection, queue semantics, and reusable models. Keep Slint and application-global state outside it.

Add `src/app/tracking.rs` for Bridge integration, worker scheduling, UI updates, and adapting existing progress/metadata. If the resolver later supports playback routing throughout the application, move the broadly reusable identity layer into an appropriate shared crate rather than making providers depend on tracker authentication.

Proposed internal modules:

    crates/tracking/src/
      lib.rs
      models.rs
      ids.rs
      resolver.rs
      mapping.rs
      projection.rs
      outbox.rs
      service.rs
      anilist.rs
      myanimelist.rs
      auth.rs

These are proposed paths. Keep the final split proportional to implemented responsibilities; do not add empty abstractions merely to match the list.

## 9. Tracker service contract

Expose a service-neutral interface with capabilities rather than assuming every tracker has identical fields.

Required operations:

- Begin and finish the supported authorization flow.
- Verify the authenticated user and obtain a stable account ID.
- Search anime metadata with pagination and bounded result counts.
- Fetch media metadata by the service's typed ID.
- Fetch the current user's list entry for one linked title.
- Add or update that list entry with a field-specific patch.
- Refresh credentials where supported, or request reauthentication.
- Report supported score formats, dates, statuses, and privacy fields.

A whole-user-list endpoint is not required for the initial release. An adapter may obtain needed account preferences without importing the list.

Return structured errors such as authentication required, rate limited, unavailable, invalid mapping, unavailable remote entry, rejected field value, and unsupported capability. Preserve sufficient diagnostic information for logs while keeping tokens and sensitive URLs redacted.

### 9.1 AniList adapter

Use AniList's documented GraphQL API and authenticated requests for list access/mutations. Search/fetch only the fields required for candidate selection and mapping, such as ID, MAL cross-reference, titles, format, release dates/status, episode count, and relevant relations.

Use a user-scoped lookup for a particular list entry. AniList's docs warn that a `MediaList` lookup using only `mediaId` does not reliably select the intended user's record. Use the authenticated media-list field or an explicit correct user ID as appropriate.

Use `SaveMediaListEntry` for changes. Distinguish `mediaId` from the existing list-entry `id`, retain the returned list-entry ID, and follow the current create/update contract. Send only fields owned by the queued intent. Do not send null/zero for unrelated fields simply because the local struct has defaults.

Inspect GraphQL `errors` even when HTTP succeeds. Do not treat a partial data response as successful mutation delivery without verifying the affected entry and fields.

### 9.2 MAL adapter

Use MAL's official API v2 for anime metadata and the authenticated user's per-title list status. Implement the current documented create/update operation for anime list status and its field names, including episode progress and dates.

The official MAL reference could not be fetched in this research environment. Mihon's source confirms a deployed MAL API integration and authorization/refresh flow, but its manga implementation is not a substitute for verifying the anime contract. Before coding this adapter, check the official documentation for the exact method, fields, OAuth application type, PKCE support, and current response semantics. In particular, do not copy a manga path or choose PUT versus PATCH from an unofficial wrapper.

Retain MAL's returned list status and account identity. Classify titles that appear in search but cannot be added as rejected/unavailable rather than retrying forever.

### 9.3 Status mapping

| Nova tracking status | AniList | MAL |
| --- | --- | --- |
| Plan to Watch | `PLANNING` | `plan_to_watch` |
| Watching | `CURRENT` | `watching` |
| Completed | `COMPLETED` | `completed` |
| On Hold | `PAUSED` | `on_hold` |
| Dropped | `DROPPED` | `dropped` |
| Rewatching, if explicitly enabled | `REPEATING` | Service rewatch fields/flags; verify exact contract |

This table describes tracker state, not a replacement for Nova's current `WatchStatus` enum or library buckets.

### 9.4 Scores and dates

Present the service's scoring system. MAL normally uses its integer score scale; AniList supports user-configured scoring formats. Keep service-specific representations or a precise canonical conversion, and test rounding and clearing behavior. Updating MAL's score must not automatically overwrite AniList's score unless the user explicitly applies an edit to both.

Keep an unknown date unknown. AniList can represent partial dates; preserve any remote precision the adapter returns. Validate date clearing and partial-date behavior against MAL's current contract. Use the user's local calendar date when creating new start/finish dates, rather than silently deriving the date from UTC.

## 10. Account connection and credential handling

### 10.1 Local authorization

Register Nova's own application/client identifiers with the services. Open the system browser for sign-in and approval; do not collect account passwords in Nova.

For desktop and Android, verify the supported callback mechanisms early. Prefer a properly registered callback flow; retain a documented manual PIN/token return path when the service supports it and automated callbacks are unavailable. Tokens returned in URL fragments require explicit fragment handling; a normal HTTP server never receives the fragment itself.

Bind authorization to a pending session and validate state where supported. Prevent stale or unrelated callbacks from replacing an active account. Verify the token against the service before accepting the connection.

### 10.2 Service-specific constraints

- AniList's current published documentation describes implicit authorization for clients that cannot keep a secret, authorization-code flow for environments that can, an auth-PIN option, and no refresh tokens. Do not assume AniList offers a secretless PKCE flow unless current documentation confirms it.
- Do not ship an AniList client secret inside a desktop binary, APK, or addon. If choosing a secret-requiring flow, a secure exchange component is a separate deployment decision; the initial design should avoid making it mandatory.
- MAL authorization should use its documented native/public-client flow with a random verifier and the supported PKCE method. Verify that method against the current official contract instead of assuming S256 support.
- Expired/revoked AniList access requires reauthentication under the documented current model. MAL can use a supported refresh flow; serialize refreshes so simultaneous requests do not race token rotation.

### 10.3 Secret persistence

Use an OS credential store or platform-backed encryption with a non-exportable/platform-protected key, as appropriate for the supported targets. Assess Android Keystore and desktop credential-store availability during the first implementation phase.

- Do not place plaintext tokens in library records, addon settings, the ordinary synchronized settings map, diagnostics, or backup exports.
- Keep the ordinary tracking database's credential reference separate from the secret value.
- If persistent secure storage is unavailable, provide an honest session-only connection rather than an undocumented plaintext fallback.
- Disconnect removes credentials and pauses/cancels outgoing work for that account.
- Reconnecting the same verified account can resume its retained links after explicit activation.
- Connecting a different account never adopts the old account's pending patches.

Support one active account per service per installation initially. The internal key still includes remote account identity so switching accounts remains safe.

## 11. Linking flow and existing tracker values

### 11.1 Search and selection

When the user opens Tracking → Add tracking:

1. Identify the active service account.
2. Gather current typed IDs and source metadata.
3. Suggest the best-supported candidate or show multiple candidates.
4. Allow search by alternate title or exact ID/official URL.
5. Show title, artwork, format, year/date, and episode count for selection.
6. Establish episode coverage separately.
7. Read the existing user's tracker entry, if any.
8. Show a confirmation preview before enabling writes.

Canceling search/selection has no remote side effect. Selecting a search result alone does not add it to the user's list.

### 11.2 Initial progress policy

Avoid surprising uploads when linking an already watched source.

- If a remote entry exists, preserve its values by default.
- Offer an explicit Apply Nova history option with a preview of mapped progress and affected releases.
- If no remote entry exists, the final confirmation may explicitly add it as Plan to Watch or apply Nova history.
- If a read fails because authentication or the service is unavailable, save an inactive/pending link and defer the initial write; do not assume the remote entry is absent.

For each target, record a link-time progress checkpoint. Existing local watched flags are considered already observed when Apply Nova history is off. Only subsequent semantic events advance tracking. Otherwise a later watch of episode 2 could accidentally upload an older, unapproved local completion of episode 24.

An explicit Sync Nova history action remains available later. Its preview should show all affected targets and warn about any decrease or mapping ambiguity before applying it.

## 12. Progress projection rules

### 12.1 Use meaningful events

The tracking integration needs semantic events rather than every playback tick:

- First meaningful playback for a mapped title/release.
- Episode transitioned from unwatched to watched.
- User marked episode/season/range watched.
- User explicitly changed tracker fields.
- User approved applying saved Nova history.
- Mapping activation or correction that requires a new projection preview.

Use Nova's existing watched threshold and natural-end semantics. Do not add a second competing 80%/90% rule in the tracking crate. Position-only saves are not episode-completion writes.

Starting an episode can move a linked planning entry to Watching and set an empty start date after meaningful playback is observed. Opening a stream chooser, clicking Play without frames, or launching an external player cannot establish that event.

### 12.2 Choose and document a sparse-history policy

The trackers store aggregate episode progress, while Nova stores per-episode flags and playback positions. There is no exact conversion for arbitrary gaps.

Recommended initial policy: progress follows the highest accepted watched target episode ordinal, matching the usual sequential “up to episode N” interpretation. This is a Nova product decision; it is not proof that every preceding source episode was watched.

Example: accepted target watched episodes `{1, 2, 8}` can produce tracker progress 8. Nova's local history remains `{1, 2, 8}`. Show a concise explanation when applying sparse saved history manually. Defer an optional contiguous-only policy unless user testing establishes a need; do not silently switch between policies.

Deduplicate by target episode identity before projecting. A second provider observation of the same target episode does not add one to progress.

### 12.3 Automatic progress rule

Within an active tracking run, an automatic forward update proposes:

    progress = max(current remote baseline,
                   acknowledged progress for this run,
                   accepted mapped watched-event progress)

Use events accepted after linking, plus history explicitly approved for upload. Do not blindly scan all pre-link watched flags after every metadata refresh.

This protects an existing remote value of 20 when local history contains only episodes 1–3. Once the user explicitly sets a lower tracker value, establish a new run/revision baseline; older in-flight acknowledgments must not restore the previous higher value.

### 12.4 Local unwatch and manual decreases

Marking a local episode unwatched changes Nova history. It does not automatically lower remote tracker progress, because the tracker cannot express arbitrary holes and local rewatch preparation may be intentional.

Allow deliberate decreases through the tracking editor or an explicit history-sync preview. Classify them as an explicit replacement intent rather than applying the automatic max rule. Make the new value and affected entry visible before submitting. Preserve the edit through retries; an older queued automatic patch must not overwrite it.

### 12.5 Completion

Compute completion per tracker release, not per entire source card.

- The relevant release must have a known final regular-episode total or an explicit user completion instruction.
- Coverage must establish what target episodes the source events represent.
- Reaching the known final target episode can complete under the documented sequential policy.
- Watching all currently available episodes of an ongoing or unknown-total release is caught up, not automatically Completed.
- An unrelated sequel or special must not prevent completion of the current TV release.
- Finishing Part 1 must not mark Part 2 complete.
- A linked anime film can complete as a single tracker item when its watched event is established, even though Nova's current movie library bucket has different behavior.

Retain the remote releasing status and source completeness evidence. Do not mark Completed solely because a cached source list contains no more rows.

### 12.6 Status/date defaults

- Planning can become Watching on meaningful playback or an accepted watched event.
- Set an empty start date when the accepted start event occurs.
- Set an empty finish date when that mapped release completes.
- Preserve existing dates and scores unless the user edits them.
- On Hold/Dropped remain pinned tracker choices; automatic progression should not clear them silently. The user can resume Watching in the panel.
- Nova's local library On Hold/Dropped pins remain independent initially. Do not interpret a library category change as permission to edit every linked release.
- Treat Completed/repeating status carefully on subsequent playback. Rewatching does not automatically erase completion, dates, or score.

### 12.7 Rewatch support

Nova's `play_count` records playback openings, not full rewatches. Do not use it to increment the service repeat count.

Initially expose service repeat/rewatching fields only through an explicit supported tracker edit if implemented. Full automatic rewatch-run detection is a later feature with its own coverage and completion semantics.

## 13. Durable delivery and recovery

### 13.1 Outbox design

Maintain one ordered stream of field-specific work per service/account/media target. Coalesce redundant forward-progress updates to the newest accepted value and keep independent fields intact.

Each queued patch records:

- Target account/media identity and account generation.
- The local intent revision.
- Binding and mapping revisions relevant to its projection.
- Which fields change and their values.
- Intent type: automatic forward progress, manual field edit, or explicit replacement.
- Attempt count, next attempt time, and last classified error.
- The last acknowledged remote state used as its baseline.

Use set-style progress writes instead of “increment by 1”. Do not claim network exactly-once delivery: after a timeout the server may already have applied the request.

### 13.2 Consistency with local persistence

Once Nova accepts a progress change, it must be possible to recover the corresponding tracking intent after a crash.

Preferred approach: persist a small semantic event or projection checkpoint durably with the progress mutation, then reconcile it into the tracking outbox. If the existing transaction boundaries make a shared commit impractical, retain enough last-observed watched state and revision data for deterministic startup reconciliation.

Do not depend solely on an in-memory callback after `write_progress_map`. The plan's persistence phase must resolve the failure window between saving local history and recording pending tracker work.

Checkpoint comparisons must detect meaningful watched/unwatch transitions. Timestamp changes caused by position updates alone must not replay pre-link history or generate duplicate completion events.

### 13.3 Sending algorithm

1. Load the next eligible target patch.
2. Verify the account is still the same verified account and generation.
3. Verify the referenced binding/mapping is active and its revision still applies.
4. Read remote state when required for first delivery, reconnection, stale state, or an uncertain previous request.
5. Rebase automatic progress against the remote value without overwriting unrelated fields.
6. Send the service-specific field patch.
7. Check both transport and API-level results.
8. Persist acknowledgment for that exact revision.
9. Retain any newer patch that arrived while the request was in flight.
10. Update the tracking panel on the UI thread.

Serialize mutations for a target. Do not send an automatic progress patch and a manual decrease concurrently for the same entry.

### 13.4 Retry policy

| Condition | Behavior |
| --- | --- |
| Offline, timeout, transient server failure | Retain intent; exponential backoff with jitter and bounded request timeouts |
| HTTP/API rate limit | Respect retry/reset headers and defer service work |
| Expired token with supported refresh | Refresh once through serialized account logic and retry |
| Revoked token or reauthentication required | Pause that account; show Connect again |
| Invalid field value or rejected media entry | Stop automatic retries for that intent and show an actionable error |
| Ambiguous/stale mapping | Pause affected projection; ask for alignment review |
| Response lost after mutation may have succeeded | Read the entry before repeating; preserve newer local intent |

Schedule retries with background timers rather than blocking UI or sleeping in the playback path. Provide Retry now while still respecting an active server rate-limit window.

### 13.5 Rate limits

Use a service-wide bounded request queue with target-level serialization. Respect current limit headers and server cooldowns; do not hardcode an assumed permanent requests-per-minute allowance. AniList's documentation currently includes a degraded-limit notice in addition to its nominal limit, illustrating why adaptive behavior matters.

Candidate search should debounce input, cancel obsolete generations, cache useful metadata, and request bounded pages. Link confirmation, progress delivery, and authentication recovery should not be starved by repeated search requests.

## 14. Remote edits and conflict boundaries

One-way tracking still needs remote reads to avoid destructive writes. It does not mean Nova is the only application editing the user's tracker account.

- If remote progress is higher than Nova's automatic proposal, preserve the higher value.
- If remote score/date/privacy values changed, a progress-only patch leaves them untouched.
- Refresh the panel when opened or when the user chooses Refresh.
- Display remote progress without marking local episodes watched.
- A deliberate manual edit can replace a field, but the UI should show its current remote value when available.
- Do not turn this into unrestricted MAL↔AniList mirroring: one service changing a score does not automatically change the other.

The APIs do not necessarily offer conditional updates or atomic max-progress operations. A read followed by a write cannot fully prevent a race with another app or device. Verify available version/conditional-write support before claiming conflict guarantees. Serialize Nova's own target writes and recheck uncertain outcomes; document the remaining race instead of promising strict multi-writer consistency.

## 15. Metadata refresh, mapping edits, and source changes

Persist the established source-to-release relationship and explicit episode assignments. Refreshing a title, poster, description, or translation should not unlink tracking.

Store a mapping-evidence fingerprint over the information that matters: source episode identities, numbering/coverage, supplied external IDs, and the relevant remote release identity/count. Do not invalidate alignment merely because unrelated artwork changed.

When new source episodes appear:

- Extend a proven range only when the rule remains unambiguous and within valid target coverage.
- Keep confirmed existing assignments.
- Leave unresolved new episodes untracked and show that coverage is partial.

When an addon changes/removes IDs, renumbers content incompatibly, or contradicts a confirmed mapping, pause affected automatic projection and expose a repair flow. Do not silently redirect pending work to another media entry.

Editing a mapping increments its revision. Pending patches based on old mappings must be invalidated/reviewed. Moving watched episodes to another remote release requires an explicit history-application preview; Nova must not silently undo writes already made to the old entry.

If the user switches sources, offer to reuse the existing release binding only after validating the new source coverage. Never migrate local playback history merely because the tracking entries match.

## 16. Tracking UI

### 16.1 Settings → Tracking

Show one card/row per service with:

- Connected account name and stable account association.
- Connect, Disconnect, and Connect again actions as applicable.
- Service availability/error state and pending update count.
- Automatic tracking preference.
- A concise explanation that tracking sends progress for linked titles.

Expose the actual supported authorization flow without making users handle client secrets. Avoid implementation detail in routine user-facing screens.

### 16.2 Detail-page tracking sheet

Add a Tracking action that fits the existing detail-page controls. The sheet shows service rows and all releases linked to the current title.

Each row shows:

- Service and remote release title.
- Source coverage label, such as S1E13–24 → Part 2 E1–12.
- Progress and total when known.
- Status, score, and start/finish dates.
- Delivery state: Synced, Pending, Sign in required, Failed, or Needs alignment.
- Edit, Refresh, Change mapping, Open on service, and Unlink actions.

Use a short path for direct one-to-one links. Keep ranges and manual alignment in an expanded editor so most users do not have to understand offsets.

### 16.3 Lifecycle semantics

| Action | Expected result |
| --- | --- |
| Connect account | Enables authenticated operations; does not track every title |
| Confirm a link | Activates only the chosen service/entry/coverage and approved initial write |
| Add/remove Nova bookmark | Changes local library membership; does not add/delete a remote entry |
| Mark mapped episode watched | Queues accepted progress according to the mapping |
| Mark local episode unwatched | Changes local history; remote decrease requires an explicit tracker edit |
| Unlink tracking | Stops future automatic updates for that binding; keeps remote history |
| Disconnect service | Removes local credentials and stops outgoing work for that account |
| Refresh tracker row | Reads remote values; does not import watched history |

If several bindings share a target, unlinking one leaves the others active. Cancel unsent work that depends only on the removed binding and reproject remaining contributions.

An already transmitted mutation may finish after unlink/disconnect. A generation check prevents its response from reactivating local state, but Nova cannot retract a remote request already accepted by the server. Do not promise otherwise.

Remote deletion is unnecessary for the initial version and should not be attached to a normal Unlink action.

### 16.4 Accessibility and localization

Support touch, keyboard navigation, focus restoration, clear loading/empty states, and scrolling for titles with several linked releases. Translate Slint strings with `@tr` and Rust-formatted messages through the existing backend translation layer. Add Croatian translations alongside English-source strings.

Do not communicate link confidence or errors only through color. Show the release name and coverage before any write-producing confirmation.

## 17. Persistence, migrations, and Nova-to-Nova sync

### 17.1 Local records

Use versioned JSON records consistent with Nova's current local storage conventions. Proposed namespaces include:

- `tracking:accounts`: non-secret local account metadata and credential references.
- `tracking:bindings`: source-to-target associations.
- `tracking:assignments`: confirmed episode alignment and mapping revisions.
- `tracking:targets`: accepted projection and remote acknowledgment state.
- `tracking:outbox`: durable pending patches.
- `tracking:checkpoints`: progress-observation checkpoints.
- `tracking:resolver-cache`: bounded, disposable lookup evidence.

The final key design should follow the storage crate's transaction and recovery capabilities; the names above are illustrative. Do not quietly add these keys to the generic synchronized settings snapshot.

Use defaults for genuinely optional new fields and explicit schema versions where semantics change. Quarantine unreadable durable tracking state instead of discarding links or replaying all history. Cache corruption can be handled by rebuilding the cache.

### 17.2 Existing Nova sync

The initial reliable single-installation feature must not depend on another Nova device being online. Each installation can connect directly to the tracker service.

Confirmed non-secret identity/alignment data may later use Nova's existing paired-device sync, which is separate from direct tracker-library integration. If included during implementation, treat it as its own phase with the following rules:

- Never sync account access/refresh tokens.
- Sync source identity and mapping evidence, not a command to write someone else's account.
- Activate an account-specific binding on another device only after it connects the same verified remote account and explicitly enables tracking.
- Keep the outbox and acknowledgment state device-local.
- Apply merged progress with a known origin and a projection checkpoint so it cannot recursively echo or replay history.
- Decide whether paired progress is eligible to produce tracker work on that device; it must not happen accidentally through a generic persistence hook.
- Use `ApplyingGuard` where existing Nova remote-apply paths require it.

Nova's current sync domains are strings. Adding a JSON domain may not require changing its postcard envelope, but that must be established from the actual implementation. Altering serialized wire structs or their ordering requires the protocol/ALPN version treatment specified by `AGENTS.md`.

Cross-device simultaneous writes retain the remote API race described in section 14. If strict single-writer behavior is required later, offer an explicit designated writer or a verified coordination mechanism rather than implying the tracker provides atomic arbitration.

## 18. Application integration and threading

Keep HTTP, resolver lookups, and queue draining on background workers. No tracker call should block Slint's UI thread, the playback tick, or the local progress save.

Application wiring:

1. Initialize local tracking records and authenticate retained credentials at startup without blocking ordinary playback.
2. Add tracking state to the application bridge as a separate subsystem.
3. Route normalized metadata and stable episode references into linking/resolution.
4. Observe meaningful playback-start and watched transitions from internal playback.
5. Observe manual episode/season/range watched actions from the detail page.
6. Reconcile committed progress changes into durable tracker projection work.
7. Drain eligible work asynchronously.
8. Post UI updates through `slint::invoke_from_event_loop` or the established equivalent.
9. Reject stale search, login, and delivery responses by generation/revision.
10. Reconcile unfinished work at startup and resume eligible pending delivery on connectivity/authentication recovery.

Avoid a hook that sends work after every `write_json` or progress-position change. Enumerate all progress mutation paths and make their origin and semantics explicit. Preserve Nova's existing persistence-failure reporting rather than presenting uncommitted progress as successfully queued tracking.

Local persistence must remain available when a service is offline. A tracker outage must not prevent watching, bookmarking, or saving resume position.

## 19. Diagnostics and operational behavior

Use existing diagnostics patterns to record service, target identity, error class, queue revision, mapping revision, and retry state. Bound retained diagnostics and redact tokens, credential headers, callback URL secrets, and raw login responses.

Useful counters include pending targets, oldest pending intent, successful deliveries, authentication pauses, rate-limit deferrals, rejected mappings, and dropped stale responses. These can remain internal initially; routine user screens need clear status and recovery actions rather than debug output.

Distinguish:

- Not linked.
- Linked but source coverage is incomplete.
- Linked and pending delivery.
- Service authentication is required.
- The service rejected the selected entry/value.
- Metadata/mapping needs review.

Do not label every failure “sync failed” when an alignment correction or account reconnect is the actual remedy.

## 20. Implementation phases and acceptance gates

### Phase 0 — Contracts and platform feasibility

- [ ] Verify current AniList and MAL search, per-entry read, write, authentication, score, and date contracts.
- [ ] Register Nova application/client identifiers and choose supported callback flows.
- [ ] Validate desktop and Android callback handling with minimal prototypes.
- [ ] Validate persistent secret storage on supported targets, with a session-only fallback if needed.
- [ ] Inventory every local progress mutation path and storage transaction boundary.
- [ ] Confirm the addon ID normalization forms that need compatibility support.
- [ ] Document the chosen sparse-progress policy and initial-history defaults.
- [ ] Review the services' published API usage terms for the intended companion integration.

Gate: the plan for authentication requires no embedded confidential secret, both adapters have verified contracts, and persistence/recovery has a concrete implementation route.

### Phase 1 — Typed IDs and tracking domain

- [x] Add the tracking crate and application module.
- [ ] Implement typed IDs, source references, target/account keys, bindings, assignments, and intent models.
- [ ] Extend provider/addon normalization to retain AniList IDs and typed TMDB evidence.
- [ ] Preserve all existing library/progress/source IDs.
- [x] Add versioned local persistence for tracking records (accounts, targets, bindings, projections; outbox comes with delivery).
- [ ] Implement mapping validation and direct-ID resolution without requiring IMDb.

Gate: MAL-only and AniList-only fixtures resolve through their native service paths, old metadata still parses, and no source identity is rewritten.

### Phase 2 — First complete tracker adapter

Recommended sequence: AniList first, then MAL, with both supported by the completed feature. Reorder if platform authorization testing makes MAL the simpler first implementation.

- [ ] Implement connection, verified account identity, metadata search/fetch, per-entry reads, and field-specific writes.
- [ ] Implement one-to-one manual linking and exact ID/URL entry.
- [ ] Add Settings → Tracking and the detail tracking sheet.
- [ ] Preserve existing remote values and expose explicit initial-history application.
- [ ] Implement manual status/progress/score/date edits supported by the adapter.

Gate: a user can connect, search, link, inspect an existing entry, and apply an explicit edit without modifying My Library or local history.

### Phase 3 — Automatic progress and durable delivery

- [ ] Integrate meaningful playback-start and watched events.
- [ ] Cover manual episode, season, range, and library bulk-watch actions.
- [ ] Persist semantic checkpoints and pending patches reliably.
- [ ] Implement target serialization, coalescing, retries, and startup recovery.
- [ ] Protect initial remote baselines and unrelated remote fields.
- [ ] Implement explicit decreases as ordered replacement intents.
- [ ] Add user-visible pending/authentication/error states.

Gate: an offline watched event survives restart and updates the correct entry later; position-only changes do not flood the service; older work cannot undo a newer explicit tracker edit.

### Phase 4 — Split/merged coverage and manual repair

- [ ] Implement explicit episode assignments and proven range/offset rules.
- [ ] Support several tracker releases for one source title.
- [ ] Support several source contributions to one tracker release.
- [ ] Add the alignment preview/editor with overlap and gap validation.
- [ ] Reuse confirmed AniKoto canonical evidence without changing native IDs.
- [ ] Implement mapping revision checks and metadata-change repair behavior.
- [ ] Test completion for parts, ongoing releases, specials, and anime films.

Gate: all synthetic merged/split mapping cases in section 21 project correctly and unresolved episodes cannot update a guessed release.

### Phase 5 — Second service and independent dual tracking

- [ ] Implement the second adapter against its verified contract.
- [ ] Map statuses, scoring, and dates with service capability handling.
- [ ] Allow MAL and AniList to use different coverage boundaries.
- [ ] Isolate delivery failures and authentication per service/account.
- [ ] Keep unrelated edits independent; do not create a general tracker-to-tracker mirror.

Gate: one accepted Nova watch event can update both linked services correctly even when their release splits differ, and a failure on one does not block the other.

### Phase 6 — Broader resolution and polish

- [ ] Add a selected curated mapping source after coverage/licensing review.
- [ ] Add contextual IMDb/TMDB resolution and alias search ranking.
- [ ] Add bounded positive/negative caching with provenance.
- [ ] Finish localization, keyboard/touch flows, long-title layouts, and repair messages.
- [ ] Verify performance under many links and offline backlog.
- [ ] Update provider documentation and `docs/PROJECT_STRUCTURE.md` for implemented behavior.

Gate: common IDs can enter the same workflow, ambiguity has a useful manual path, and playback remains responsive during lookup and delivery.

### Phase 7 — Optional non-secret mapping sync between Nova devices

Implement only after the local tracking path is stable; this does not include tracker-list import.

- [ ] Specify which non-secret link/mapping records may sync and how their revisions merge.
- [ ] Require local verified account connection/activation before sending.
- [ ] Keep secret references, credentials, pending work, and acknowledgments out of peer snapshots.
- [ ] Define paired-progress origins and deduplication explicitly.
- [ ] Verify mixed-version behavior and any necessary protocol updates.
- [ ] Document the limits of simultaneous independent tracker writers.

Gate: pairing does not leak credentials, create unsolicited remote tracking, or replay old history.

## 21. Behavior-focused verification matrix

Use deterministic fixtures and mock service responses for most tests. Live tests should be manual/opt-in and use disposable test-account entries; never modify a user's real list during normal CI.

### Identity and resolution

| Case | Expected outcome |
| --- | --- |
| MAL ID only | Direct MAL lookup/link; no IMDb requirement |
| AniList ID only | Direct AniList lookup/link; MAL lookup uses an available verified cross-reference |
| Different numeric MAL/AniList IDs | Correct namespace conversion; no numeric equality assumption |
| Bare numeric source ID | No guessed catalog namespace |
| TMDB movie versus TV with same number | Different typed identities |
| Conflicting supplied IDs | Conflict exposed; no automatic write |
| Alias match with incomplete metadata | Candidate suggestion; coverage still validated |
| Same title, remake/different year | Candidates remain distinct |
| Mapping-source outage | Temporary failure/manual path; not a permanent missing-match cache |

### Episode coverage and completion

| Case | Expected outcome |
| --- | --- |
| Source S1E15 in a 24-episode merged season | Part 2 episode 3 under the confirmed 12+12 mapping |
| Two 12-episode source items form one tracker release | Target episodes 1–24; shared target state |
| Two sources report the same target episode | Deduplicated identity, not an extra episode |
| Unequal 10+14 split | Uses validated boundaries; never assumes 12 |
| Continuous source numbering | Correct explicit/range mapping |
| Special or recap among normal episodes | Explicit handling; no unintended offset shift |
| Displayed canonical episode differs from native number | Stable source episode ID remains the lookup key |
| Final known episode of Part 1 watched | Part 1 completes; Part 2 stays unchanged |
| Ongoing/unknown-total title caught up | No automatic Completed based on current cache length |
| MAL split differs from AniList split | Independent correct projections |
| Linked anime film watched | Its single tracker release can complete |
| Metadata adds future episodes | Confirmed old assignments retained; new coverage validated |

### Progress and edits

| Case | Expected outcome |
| --- | --- |
| Partial playback below watched threshold | No completed-episode progress write |
| First meaningful playback of Planning entry | Watching/start date according to policy; no false completion |
| Threshold transition and natural end | One accepted completion intent, not duplicate increments |
| Repeated position saves after watched | No service request for each save |
| Link existing remote progress 20 with local 3 | Remote progress preserved |
| Pre-link local episode 24 watched, history application off | Later watch of episode 2 does not upload 24 |
| User explicitly applies old history | Previewed targets/progress are applied |
| Sparse accepted history 1, 2, 8 | Tracker gets 8 under chosen policy; local flags remain sparse |
| Local mark-unwatched | Local history changes; no implicit tracker regression |
| Explicit tracker decrease while automatic write is in flight | Ordered newer revision wins locally; old acknowledgment cannot revive stale progress |
| Progress-only update on remotely edited score/date | Unrelated fields preserved |
| Local library removal or status-pin change | No remote deletion or hidden bulk tracker edit |
| External player launched without observations | No watched event inferred |

### Delivery, accounts, and recovery

| Case | Expected outcome |
| --- | --- |
| Offline watched event, restart, reconnect | Correct durable update eventually delivered |
| Crash between local progress and outbox projection | Checkpoint/event reconciliation recovers the intent |
| Request timeout after server applied mutation | Read/reconcile; no repeat-count increment or stale overwrite |
| Newer patch arrives during a request | Acknowledgment removes only the matching old revision |
| HTTP success with GraphQL mutation errors | Treated as failed/partial mutation, not Synced |
| Rate-limit response | Correct cooldown without busy retries |
| Token expiry/revocation | Refresh or reconnect according to service capability |
| Account switch | Old queued work never targets the new account |
| Unlink or mapping edit during delivery | Stale response cannot re-enable binding or redirect old work |
| MAL unavailable while AniList works | AniList continues independently |
| Corrupt tracking state | Quarantined with visible recovery, not silently wiped/replayed |
| Unsupported secure credential persistence | Explicit session-only behavior; no hidden plaintext storage |

### UI and platform checks

- Validate desktop and Android callback completion, cancellation, and stale-return handling.
- Exercise long titles, many linked releases, narrow screens, keyboard focus, and touch scrolling.
- Verify translated status/error strings and date/score editing formats.
- Verify visible loading and pending state during service outages.
- Confirm ordinary playback and local progress persistence remain usable throughout.

### Repository validation

Run focused tests for changed behavior, then the repository-required checks:

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo check
    cargo test -p nova-tracking
    cargo test -p nova-sync --lib
    cargo test

The `nova-tracking` test command applies once that crate exists. Run relevant headless Slint tests and desktop/Android/Windows GNU build checks appropriate to the implemented changes. Avoid repeating the full suite after it passes unless new changes or concerns justify it.

## 22. Definition of done for the initial feature

The initial feature is complete when:

- Users can connect their own MAL and AniList accounts on supported Nova targets.
- Tracking is explicitly enabled per title/release and source coverage.
- Direct MAL/AniList IDs work without IMDb.
- Stable addon IDs and existing library/progress data are preserved.
- Missing/ambiguous mappings have a usable manual link/alignment path.
- Merged/split seasons update the correct service release and target episode.
- New accepted watched events and supported manual tracker edits update the remote entry.
- Initial remote progress is protected unless the user explicitly applies/replaces it.
- Offline work survives restarts, is scoped to the correct account, and retries visibly.
- Changes to mappings, accounts, or bindings cannot reuse stale pending work silently.
- Scores, dates, privacy, notes, and unrelated fields survive progress-only writes.
- My Library membership and local watched history are not imported or mirrored from trackers.
- Tokens stay outside addons, synchronized ordinary settings, and diagnostics.
- Relevant behavior tests and platform/build checks pass.
- Documentation describes the implemented behavior and remaining limits accurately.

## 23. Decisions to keep explicit during implementation

The plan chooses defaults where possible. Resolve these implementation details through contract/platform testing before writing dependent code:

| Decision | Proposed direction | Evidence needed |
| --- | --- | --- |
| First adapter | AniList, then MAL | Callback and secure-storage prototypes may justify reordering |
| Sparse progress | Highest accepted target episode | Product explanation and behavior tests |
| Applying saved history on link | Explicit option; default preserve existing remote values | Link-preview usability |
| Automatic decreases | Disabled; explicit tracker edits only | Ordered replacement/retry tests |
| Credential persistence | Platform-protected store; session-only fallback | Supported platform behavior |
| MAL API operation details | Current official anime contract | Official reference access before implementation |
| AniList native authorization | Documented public-client flow without embedded secret | Actual callback/PIN support on desktop and Android |
| Source mapping distribution | Optional curated evidence provider | License, size, coverage, and update behavior |
| Storage consistency | Durable semantic checkpoint/event and outbox reconciliation | Existing redb/storage transaction paths |
| Paired-device mapping sync | Separate optional phase | Origin handling, schema compatibility, and writer-race tests |

None of these require direct tracker-library integration to deliver useful Mihon-style tracking.

## 24. Sources and research limits

### Nova repository evidence

- [Repository instructions](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/AGENTS.md).
- [Application state and persisted models](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/src/app.rs).
- [Playback progress](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/src/app/playback.rs).
- [Detail episode actions and canonical aliases](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/src/app/detail.rs).
- [Library behavior reference](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/docs/library.md).
- [Provider model](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/crates/providers/src/models.rs).
- [Conservative metadata matcher](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/crates/providers/src/matching.rs).
- [Provider integration and mapping behavior](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/crates/providers/README.md).
- [AniKoto canonical integration](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/crates/providers/src/anikoto.rs).
- [Application sync projection](https://github.com/misobarisic/nova/blob/b014b98c233f29814ced15401ca0a00cff052918/src/app/sync.rs).

### Primary tracker/reference documentation

- [Mihon tracking guide](https://mihon.app/docs/guides/tracking).
- [Mihon MAL adapter source](https://github.com/mihonapp/mihon/blob/main/app/src/main/java/eu/kanade/tachiyomi/data/track/myanimelist/MyAnimeListApi.kt), inspected as implementation evidence; this adapter handles manga.
- [AniList authentication guide source](https://github.com/AniList/docs/blob/master/docs/guide/auth/index.md).
- [AniList implicit authorization source](https://github.com/AniList/docs/blob/master/docs/guide/auth/implicit.md).
- [AniList authorization-code source](https://github.com/AniList/docs/blob/master/docs/guide/auth/authorization-code.md).
- [AniList media reference source](https://github.com/AniList/docs/blob/master/docs/reference/object/media.md).
- [AniList per-entry list query guide source](https://github.com/AniList/docs/blob/master/docs/guide/graphql/queries/media-list.md).
- [AniList mutation reference source](https://github.com/AniList/docs/blob/master/docs/reference/mutation.md).
- [AniList rate-limit documentation source](https://github.com/AniList/docs/blob/master/docs/guide/rate-limiting.md).
- [AniList API terms](https://github.com/AniList/docs/blob/master/docs/guide/terms-of-use.md).
- [MAL official authorization reference](https://myanimelist.net/apiconfig/references/authorization).
- [MAL official API v2 reference](https://myanimelist.net/apiconfig/references/api/v2).
- [Fribb cross-catalog mapping project](https://github.com/Fribb/anime-lists).
- [Anime-Lists mapping project](https://github.com/Anime-Lists/anime-lists).

AniList's published terms restrict competing, non-complementary tracking services and use of its API as generic storage. The intended feature is a playback companion that maintains explicitly linked AniList entries; verify the intended use against current terms before distribution rather than treating this document as authorization from AniList.

The Nova findings refer to the reviewed commit, not a promise about future repository changes. AniList documentation was read from its official source repository where rendered pages were unavailable. MAL's official references were unavailable in this environment, so unverified anime API details are deliberately implementation gates. Proposed models, policies, module paths, and phase ordering are design recommendations, not descriptions of features already present in Nova.

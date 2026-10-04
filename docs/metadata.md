# Metadata fetching and artwork reliability

This review follows catalog entries from their source through Home, Detail,
episode mapping, persistence and image decoding. The shared behavior is provider
agnostic; provider adapters supply native fields and stable IDs. The existing
Cinemeta numbering repair is a separate adapter-specific correction.

## Why the featured artwork was missing

A catalog is a preview, not a complete detail response. AniKoto's catalog
currently supplies `poster` and leaves `background` and `description` absent.
Home previously fetched only `background`, never requested detail metadata, and
marked absent/failed artwork as finished for the entire list generation. Opening
Detail could enrich the same show, but Home did not consume that result.

The same failure applied to any poster-only catalog. Home now displays a poster
when no backdrop is available, loads missing detail fields for the current and
next slide, and upgrades the image when richer artwork arrives. A failed
backdrop also falls back to the poster. Existing pixels remain visible during
upgrades; metadata and images are committed together through the existing
featured revision/crossfade path.

A live check found 40 poster rows in the public AniKoto catalog HTML; Nova's
provider returned its bounded page of 30 previews, all with posters and none
with backdrops. Nova's actual desktop image loader fetched and decoded a poster
successfully (225 × 318). Native detail supplied a description and 13 episodes;
enabling the installed Cinemeta manifest supplied a backdrop through ordinary
enrichment, retaining the requested native show ID and episode count. A
separate Python image request received HTTP 403, while Nova's transport
succeeded; that probe failure does not establish an image authentication
problem in the app. No site-specific image transport was added.

## Pipeline and ownership

| Stage | Implementation | Contract |
|---|---|---|
| Installed sources | `src/app/addon_mgr.rs`, `crates/providers/src/metadata.rs` | Enabled, available addon manifests form an immutable metadata snapshot. Its canonicalized fingerprint invalidates aliases/cache results when inventory changes. |
| Catalog | `src/app/{catalog,home}.rs`, `crates/providers/src/registry.rs`, provider JS | Catalog responses may omit optional fields. Bundled JS is converted to the same `MetaPreview` contract as ordinary addons. Home retains the originating catalog URL for opaque-ID ownership. |
| Home hydration | `src/app/home.rs` | Native/cached artwork paints immediately. Missing header fields load through ordinary meta endpoints, with at most two detail chains active and four endpoint candidates per chain. Originating providers are preferred; opaque IDs are sent only to their owner. Recognized global IDs may fall back to other accepting manifests. |
| Detail and prefetch | `src/app/{detail,catalog}.rs` | Cached headers/episodes paint first, then refresh on workers. Responses must match both requested ID and media type before being cached/applied. Discover's prefetch preference does not disable Home hydration or Library prefetch. |
| Optional enrichment | `crates/providers/src/metadata.rs`, `crates/media/src/net.rs` | Resolve explicit external IDs first, expand discovered IDs, then search remaining addon catalogs by normalized title/family. Already connected addons need no speculative title search. Candidate detail responses must preserve requested identity. |
| Episode alignment | `crates/providers/src/{sequence,matching}.rs` | Match native available episodes to external episodes using IDs, release context, sequence/count evidence and unique title anchors. Split seasons/cours and continuous numbering share one mapper. Gaps, remakes, duplicate editions and conflicting identities reject speculative aliases. |
| Field selection | `crates/providers/src/metadata.rs` | Keep native playback IDs and availability. Treat blank strings as absent. Select display fields only from accepted, nonconflicting connections. Later accepted providers may fill missing thumbnails/overview without replacing the first accepted display numbering. Duplicate external aliases retain independent provenance until conflicts are checked, then are deduplicated. |
| Cinemeta correction | `crates/providers/src/metadata/cinemeta.rs` | Its existing official-endpoint repair uses one-to-one episode-title/date evidence to correct image numbering while retaining native episode IDs. Confirmed corrections survive failed, malformed, unrelated and partial refreshes; new confirmed images take precedence. |
| UI publication | `src/app/{home,detail,posters}.rs` | Slint updates run on the event loop. Home checks list generation and requested identity; image completions also check the in-flight URL. Detail uses opening tokens and stream generations. Episode thumbnail repainting targets the currently visible list. |
| Image delivery | `crates/media/src/{net,cache}.rs`, `src/app/posters.rs` | URL-keyed decoded LRU and validated disk files are shared by desktop/Android. Display derivatives have separate size keys. Corrupt cached bytes permit refetching; cache replacements are atomic. Desktop and Android image downloads reject HTTP errors and retry transient failures. |
| Tracking | `src/app/tracking.rs`, `crates/tracking/src/{cache,resolution,mapping}.rs` | Tracking remembers source identity/aliases and resolves tracker releases separately. Artwork enrichment does not activate a tracker mapping or alter watched/native playback identities. |

## Cache layers

| Cache | Scope / policy | Failure behavior |
|---|---|---|
| `home:showcase:v1` | Local results keyed by addon URL, type, catalog and genre; five displayed titles per selection, with bounded extra candidates for deduplication | Failed catalogs keep their last successful batch. Sparse successful refreshes preserve richer known fields for the same identity; supplied nonempty values replace old ones. Hydrated headers/poster URLs persist across restarts. Opaque IDs from different owners remain separate. |
| `meta_header:{type}\x01{id}` | Local detail headers/season artwork | Empty optional responses do not erase usable cached values. Supplied nonempty backdrop/season URLs update older URLs. Home can reuse these headers without fetching Detail again. |
| `episodes:{type}\x01{id}` | Local native episode snapshots | Refreshes keep prior thumbnails by stable native episode ID when new artwork is absent; fresh nonempty artwork replaces old artwork. |
| `metadata:v2:<digest>` | Bounded shared session store; confirmed entries also use `provider_metadata_cache:v1` (128 entries / 8 MiB, 256 KiB per entry) | Confirmed enrichment is reused for 30 days. Failed discovery preserves a confirmed result for the same fingerprint. Explicit conflicts invalidate it. Missing/ambiguous results expire after 30 seconds and remain session-only. Oversized results are usable without being cached. |
| Enrichment fingerprint | Native details/sequence, caller, canonicalized inventory revision, mapping/artwork/field-selection versions | Changed availability, source identity, inventory or implementation rules cannot silently reuse obsolete confirmations. No sync wire change is needed for local cache-version changes. |
| Home request state | Per-list generation; current/next metadata and artwork | Failed images and incomplete/failed metadata have a 30-second cooldown. Rotation or returning Home retries them after the cooldown. In-flight duplicates are suppressed; absent/unavailable artwork does not stop paging. |
| Images | Local URL hash, encoded source bytes and optional display derivatives; decoded memory budget | Caching can be disabled independently of metadata persistence. Failed refreshes keep displayed pixels. Detail refresh checks compare decoded pixels before replacing the displayed image. |

## Reliability boundaries and follow-ons

“Always” must mean that missing external data does not break native browsing or
replace correct identity with a guess. No client can guarantee an image that a
remote provider does not expose, a site that is unavailable, or an unambiguous
mapping when the available evidence conflicts. Keep the native poster/metadata,
reuse confirmed cache data where the fingerprint still agrees, and retry bounded
work when the user revisits or rotates the relevant content.

The remaining architectural improvements are:

- **Shared scheduling and cancellation.** Home bounds active detail chains and
  episode thumbnails have a four-worker pump, but catalog, Detail and Library
  prefetch can still request the same title independently. Generation guards
  prevent stale UI writes; they do not cancel network work already in progress.
  A shared request coordinator would reduce duplicate network/decode work.
- **Fair optional discovery budgets.** Enrichment has a five-second overall
  deadline, 32 requests, 100 previews per search response, six detail candidates
  and a 2 MiB response cap. Resolving known IDs first avoids wasting that
  deadline on unnecessary searches. A slow early request can still consume the
  remaining deadline; per-source scheduling/budgets would improve fallbacks.
- **Broader header refresh policy.** Home hydrates sparse previews and Detail
  refreshes series episodes, but opening a movie currently starts streams
  directly. Catalog-provided movie headers and older text-only header caches
  do not have a common freshness/forced-refresh policy.
- **Richer normalized fields and diagnostics.** `MediaItem` carries core
  title/year/artwork/description/genres/IDs, not every external field such as
  cast/runtime/rating. Ordinary addon responses preserve those extensions, but
  bundled-provider enrichment does not project every one. Missing discovery
  also reports a broad status rather than a per-source explanation. Extend the
  shared contract/diagnostics deliberately rather than special-casing a show.
- **Image authentication and memory bounds.** Image fields currently carry a
  URL, not provider-specific request headers. Protected image hosts may require
  a richer artwork request contract. Featured images retain source resolution
  and per-slide buffers; many configured catalogs can retain more pixels than
  the decoded LRU budget alone suggests.
- **Legacy opaque-ID storage.** Home distinguishes opaque IDs by catalog owner,
  while Library/header/episode persistence is still fundamentally keyed by
  stable media IDs. Adapters must namespace private IDs; supporting colliding
  unnamespaced IDs across independent addons needs a storage migration.

## Regression coverage

- `src/app/home.rs::tests`: poster-only providers, backdrop/poster URL priority,
  in-flight suppression, failed-image cooldown/retry, preserving pixels during
  upgrades, catalog ownership, richer metadata retention/restart, stale
  completions, and existing deferred carousel refresh behavior.
- `src/app/detail.rs::tests`: requested metadata ID/type validation and existing
  thumbnail retention, split-source routing and stream ordering.
- `crates/providers/src/metadata.rs::tests`: ID-first lookup, blank-field
  fallback, later-provider episode artwork, conflict-safe display fields and
  duplicate-alias provenance, bounded discovery and cache invalidation.
- `crates/providers/src/metadata/cinemeta.rs::tests`: shifted numbering, special
  classification, date/title ambiguity, stable identities and partial refreshes.
- `crates/media/src/cache_tests.rs`: corrupt-cache recovery, atomic/format
  handling, sized images and an HTTP 503 → successful PNG download.
- Existing Home headless tests cover card layout, featured crossfade/rotation,
  paging gestures and carousel behavior independently of external servers.

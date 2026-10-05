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

## Android cards after library sync

Sync shares artwork URLs, not image files or decoded buffers. Blank Android
cards therefore need diagnosis after image decoding as well as during metadata
lookup. A 680 × 1000 TVDB poster exposed a display-resizing bug: fitting into
pre-rounded bounds produced 326 × 479 pixels labelled as 326 × 480. Android's
Skia renderer rejects the undersized buffer, even though the loader reports
success; smaller Metahub posters bypass the resize and display normally.

The shared display helper now fits once into the size limit and uses the
result's actual dimensions. Android started using this helper when image
compression was added (`a2660f0`); its previous loader already used actual
resized dimensions. This fix needs no ID migration, metadata remapping or
image-cache clearing. Regression coverage checks pixel storage against the
reported dimensions for portrait, landscape, narrow and already-small images.

## Pipeline and ownership

| Stage | Implementation | Contract |
|---|---|---|
| Installed sources | `src/app/addon_mgr.rs`, `crates/providers/src/metadata.rs` | Enabled, available addon manifests form an immutable metadata snapshot. Its canonicalized fingerprint invalidates aliases/cache results when inventory changes. |
| Catalog | `src/app/{catalog,home}.rs`, `crates/providers/src/registry.rs`, provider JS | Catalog responses may omit optional fields. Bundled JS is converted to the same `MetaPreview` contract as ordinary addons. Home retains the originating catalog URL for opaque-ID ownership. |
| Home hydration | `src/app/home.rs` | Native/cached artwork paints immediately. Missing header fields load through ordinary meta endpoints, with at most two detail chains active and four endpoint candidates per chain. Originating providers are preferred; opaque IDs are sent only to their owner. Recognized global IDs may fall back to other accepting manifests. |
| Detail and prefetch | `src/app/{detail,catalog}.rs` | Detail paints cached content first and refreshes metadata on workers for series and movies; movie metadata runs independently of stream discovery. Responses must match both requested ID and media type before being cached/applied. Discover browse/search reuse the last decoded Detail poster or a saved Library poster; enabled metadata prefetch also decodes newly discovered posters for missing grid art. Discover's prefetch preference does not disable Home hydration or Library prefetch. |
| Library mapping refresh | `src/app/{library,catalog,addon_mgr}.rs` | Opening Library fills missing episode/header metadata through the same enrichment broker as Detail; movies need header metadata only. Complete cached entries keep their cache. Missing/failed posters independently run mapping and retain the enriched native header/episode aliases, even if old metadata caches are populated. Displayed sort/filter order gets priority. Shared prefetch state coalesces queued requests and retries missing metadata after five minutes on success, one minute on failure, or a changed addon inventory. An open Library retries when manifests become available. Native saved/playback IDs and watch history remain intact. |
| Grid artwork repair | `src/app/{posters,catalog,run}.rs` | Missing or failed posters in Discover browse/search, My Library and Home retry enabled metadata endpoints through the same enrichment transport as Detail, independently of episode prefetch or cached episodes/header text. Episodes with an unusable poster do not stop recovery: try the next endpoint. Follow current nonconflicting broker connections to alternate IDs only at their owning enabled metadata addons, keeping the original Library/playback ID. A visited-endpoint budget (16) prevents alias cycles. Pending manifests do not spend cooldowns; when manifests become usable, only queued missing URLs or confirmed download failures retry. Unloaded rows do not trigger speculative enrichment. A distinct cached poster is tried before metadata endpoints. Library visits skip loaded posters and coalesce pending image loads. For saved native identities, also cache the enriched header/episode aliases and update the existing Continue/Upcoming publication path. Clearing image cache then failing to download a saved URL invokes this repair, including inside Detail. Persist and publish only decoded replacement URLs to matching grids and a matching Detail view. |
| Optional enrichment | `crates/providers/src/metadata.rs`, `crates/media/src/net.rs` | Resolve explicit external IDs first, expand discovered IDs, then search remaining addon catalogs by normalized title/family. Already connected addons need no speculative title search. Candidate detail responses must preserve requested identity. Tied weak family matches are evaluated together within the six-detail budget; exactly one must prove at least two available episode mappings, and every competitor must answer successfully (an empty Stremio detail response counts as no candidate; errors do not). Exact-title/remake ties remain unresolved. |
| Episode alignment | `crates/providers/src/{sequence,matching}.rs` | Match native available episodes to external episodes using IDs, release context, sequence/count evidence and unique title anchors. Split seasons/cours and continuous numbering share one mapper. A contiguous available prefix may match a larger canonical season only for an exact family, the same season and its first part, with no known later source season/part or contradictory declared total. A shorter declared first-cour total is allowed when only a contiguous aired prefix has servers; only episodes actually available at the source map. Repeated generic episode prefixes are placeholders, not title evidence. Gaps, remakes, duplicate editions and conflicting identities reject speculative aliases. |
| Field selection | `crates/providers/src/metadata.rs` | Keep native playback IDs and availability. Treat blank strings as absent. Select display fields only from accepted, nonconflicting connections. Later accepted providers may fill missing thumbnails/overview without replacing the first accepted display numbering. Duplicate external aliases retain independent provenance until conflicts are checked, then are deduplicated. |
| Cinemeta correction | `crates/providers/src/metadata/cinemeta.rs` | Its existing official-endpoint repair uses one-to-one episode-title/date evidence to correct image numbering while retaining native episode IDs. Confirmed corrections survive failed, malformed, unrelated and partial refreshes; new confirmed images take precedence. |
| UI publication | `src/app/{home,detail,posters}.rs` | Slint updates run on the event loop. Home checks list generation and requested identity; image completions also check the in-flight URL. Detail uses opening tokens and stream generations. Successful Detail poster loads publish to matching Discover browse/search and Library rows, retaining corrected URLs through scroll unloading. Grid image completions check current URLs before painting or caching pixels. Episode thumbnail repainting targets the currently visible list. |
| Image delivery | `crates/media/src/{net,cache}.rs`, `src/app/posters.rs` | URL-keyed decoded LRU and validated disk files are shared by desktop/Android. Display derivatives have separate size keys. Corrupt cached bytes permit refetching; cache replacements are atomic. Desktop and Android image downloads reject HTTP errors and retry transient failures. |
| Tracking | `src/app/tracking.rs`, `crates/tracking/src/{cache,resolution,mapping}.rs` | Tracking remembers source identity/aliases and resolves tracker releases separately. Artwork enrichment does not activate a tracker mapping or alter watched/native playback identities. |

## Cache layers

| Cache | Scope / policy | Failure behavior |
|---|---|---|
| `home:showcase:v1` | Local results keyed by addon URL, type, catalog and genre; five displayed titles per selection, with bounded extra candidates for deduplication | Failed catalogs keep their last successful batch. Sparse successful refreshes preserve richer known fields for the same identity; supplied nonempty values replace old ones. Hydrated headers/poster URLs persist across restarts. Opaque IDs from different owners remain separate. |
| `meta_header:{type}\x01{id}` | Local detail headers/season artwork and last decoded poster URL | Empty optional responses do not erase usable cached values. Supplied nonempty backdrop/season URLs update older URLs. Home can reuse these headers without fetching Detail again. Discover reuses `poster_url`, written only after successful decoding, so an unverified URL cannot displace working cached artwork; older JSON defaults it to empty. |
| `episodes:{type}\x01{id}` | Local native episode snapshots | Refreshes keep prior thumbnails by stable native episode ID when new artwork is absent; fresh nonempty artwork replaces old artwork. |
| `metadata:v2:<digest>` | Bounded shared session store; confirmed entries also use `provider_metadata_cache:v1` (128 entries / 8 MiB, 256 KiB per entry) | Complete episode enrichment is reused for 30 days; confirmed identities with unmapped episodes, placeholder titles or missing image URLs are retried on the next fetch after five minutes. Confirmed mappings stay durable through that shorter refresh cycle. Failed discovery preserves a confirmed result for the same fingerprint. Explicit conflicts invalidate it. Missing/ambiguous results expire after 30 seconds and remain session-only. Oversized results are usable without being cached. |
| Enrichment fingerprint | Native details/sequence, caller, canonicalized inventory revision, mapping/artwork/field-selection versions | Changed availability, source identity, inventory or implementation rules cannot silently reuse obsolete confirmations. No sync wire change is needed for local cache-version changes. |
| Home request state | Per-list generation; current/next metadata and artwork | Failed images and incomplete/failed metadata have a 30-second cooldown. Rotation or returning Home retries them after the cooldown. In-flight duplicates are suppressed; absent/unavailable artwork does not stop paging. |
| Grid repair request state | Shared typed identity/failed-URL cooldown (60 seconds) | Browse/search/Library/Home suppress duplicate repair requests. Stale catalog generations and search targets cannot start repairs for replacement cards. |
| Metadata prefetch request state | Session-only typed identity, addon revision and next retry time | Library prefetch eligibility checks missing header/episode fields; failed artwork recovery remains independent of populated legacy caches. Discover and Library coalesce queued/fetching pairs; no retry window is spent before a matching enabled, available metadata source exists. Confirmed mappings still use the broker's persistent cache. |
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
  episode thumbnails have a four-worker pump. Catalog and Library prefetch now
  coalesce typed requests, but Home, Detail and independent artwork repair can
  still request the same title independently. Generation guards
  prevent stale UI writes; they do not cancel network work already in progress.
  A shared request coordinator would reduce duplicate network/decode work.
- **Fair optional discovery budgets.** Enrichment has a five-second overall
  deadline, 32 requests, 100 previews per search response, six detail candidates
  and a 2 MiB response cap. Resolving known IDs first avoids wasting that
  deadline on unnecessary searches. A slow early request can still consume the
  remaining deadline; per-source scheduling/budgets would improve fallbacks.
- **Broader header refresh policy.** Home hydrates sparse previews and Detail
  refreshes both series and movies behind cached content. Catalog-provided
  headers and older text-only header caches do not have a common age-based
  freshness policy.
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

- `src/app/posters.rs::discover_artwork_tests`: missing artwork with episode prefetch disabled, shared recovery cooldowns, stale identity/URL rejection, and a headless HTTP fixture proving that cached episodes and an unusable first-source image do not prevent posters from reaching all grids without opening Detail. Its Library case keeps a healthy cached entry free of metadata requests, then clears the image cache to expose an expired saved URL and uses the real enrichment broker to publish alternate artwork and persist current episode aliases without opening Detail. Additional cases prove unloaded rows do not start repair, repeated Library visits coalesce pending image downloads, a decoded cached alternative avoids metadata requests, and confirmed failures retry once manifests become usable. A separate movie case proves that entering Detail refreshes otherwise healthy cached metadata/artwork; typed Detail-only failed-URL guards reject stale completions.
- `src/app/catalog.rs::tests`: typed prefetch coalescing, elapsed retry windows and changed addon inventories.

- `src/app/home.rs::tests`: poster-only providers, backdrop/poster URL priority,
  in-flight suppression, failed-image cooldown/retry, preserving pixels during
  upgrades, catalog ownership, richer metadata retention/restart, stale
  completions, and existing deferred carousel refresh behavior.
- `src/app/detail.rs::tests`: requested metadata ID/type validation and existing
  thumbnail retention, split-source routing and stream ordering.
- `crates/providers/src/metadata.rs::tests`: ID-first lookup, blank-field
  fallback, later-provider episode artwork, conflict-safe display fields and
  duplicate-alias provenance, weak-family search ties and failed competitors, ongoing-season metadata without fabricated availability, short refresh lifetimes for incomplete episode fields, bounded discovery and cache invalidation.
- `crates/providers/src/sequence.rs::tests`: available prefixes, duplicate placeholders, holes, split parts, wrong seasons/years and conflicting titles.
- `crates/providers/src/metadata/cinemeta.rs::tests`: shifted numbering, special
  classification, date/title ambiguity, stable identities and partial refreshes.
- `crates/media/src/cache_tests.rs`: corrupt-cache recovery, atomic/format
  handling, valid resized pixel-buffer dimensions (including 680 × 1000 TVDB
  posters), sized images and an HTTP 503 → successful PNG download.
- Existing Home headless tests cover card layout, featured crossfade/rotation,
  paging gestures and carousel behavior independently of external servers.

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

## Catalog presentation evidence

`crates/providers/src/catalog_grouping.rs` keeps catalog grouping separate from
Library/progress equivalence and tracker identities. Owned previews retain their
original IDs, addon installation and order. Confirmed parent relationships can
form Series groups; Season groups require canonical season proof. Complete
mapped episode partitions can create reversible virtual season cards while
retaining the native episode IDs. Relative source season numbers, title text,
and ordinary external-ID aliases alone do not establish these relationships.

The source-shaped fixtures in `test-fixtures/catalog-grouping` document
AIOMetadata's canonical IMDb episode IDs/triples. Native-only responses without
parent evidence remain ungrouped under the default confirmed-only policy. The optional `novaCatalogGrouping` extension
is distinguished from those existing addon fields. Conflicts, ambiguous
episodes and incomplete partitions do not produce guessed season groups. Independently verified broker parent proof remains usable for Series grouping when the exact native inventory contains specials; the whole source remains reachable in Season mode.

Settings → Display's **Infer series grouping** is an independent profile boolean
(`catalog_grouping_infer_series`, false by default), synced with ordinary device
overrides. It applies only when a catalog uses Series mode. The optional bounded
route combines explicit source-owned prequel/season relationships, consecutive
season labels and a matching title family with an independently confirmed
canonical series anchor. Titles or relationship links alone remain insufficient;
conflicts, branches, cycles and mismatched owners must reject the inference.
`GroupingProof::InferredSeasonChain` records this weaker origin separately, and
`GroupingEvidence::is_inferred()` identifies it for presentation and policy
checks. The result provides only a display parent with unknown season and no
partitions: it cannot mint episode aliases, renumber native episodes, change
Library/progress identity, establish tracker coverage or create virtual seasons.
Original and Season modes retain confirmed-only behavior; disabling inference
removes its display memberships without changing the underlying native items.

A separate local `catalog_grouping_evidence:v1` cache stores envelope version 5
evidence, capped at 128 entries / 512 KiB, with seven-day positive and 30-second
negative TTLs. Source ownership, metadata inventory revision and the confirmed/inferred policy
participate in its separate key namespaces. Confirmed proof takes precedence;
a negative lookup while inference is disabled cannot suppress an enabled lookup.
Inferred proof retains a minimal native parent preview and provenance for display,
without duplicating its episode inventory or exporting episode aliases. The exact-ID resolver permits two concurrent lookups, at most two
accepting endpoints per request, a two-second total timeout and 2 MiB body cap.
When the exact owner returns a matching globally identified native inventory, the resolver can reuse bounded current confirmed broker cache results to recover parent-only proof without running full enrichment. Legacy opaque identities are never transplanted between installations; changed inventories, revisions and conflicting parent claims reject recovery. Original mode does not initiate catalog-grouping network requests. Derived
evidence is not synced; only the grouping preferences use settings sync. Immutable
read snapshots remain available while background writers serialize and persist a
replacement.

`src/app/catalog_grouping.rs` resolves Settings → Display's global and per-catalog
Home/Discover preferences, projects raw previews and registers opaque display
actions. Original remains the default. Each genre inherits its catalog's choice;
settings use the existing profile sync and device override policies. Missing
addons retain editable and removable overrides, and their configured URLs never
enter labels.
The UI chooser retains its outgoing snapshot through close animations. Groups
prefer a confirmed parent already supplied by a compatible catalog/metadata
owner; otherwise each original member remains reachable through the chooser.

Discover keeps raw responses separate from visible cards. Its pagination cursor
advances by fetched raw counts, even if all new seasons collapse into existing
cards. Empty/repeated/failed pages and automatic backfill-budget pauses remain
distinct. A paused catalog offers Continue rather than pretending it is exhausted.
Focus, viewport anchors and poster deliveries carry display identity in addition
to native IDs. Verified poster repairs update raw candidates only while the
captured catalog generation, exact source owner, native identity and previous URL
still match, so regrouping preserves repaired artwork without accepting stale
callbacks. Home projects each source before featured merging or rail limits,
retains bounded raw batches and may backfill at most three pages / 120 raw
candidates per source when grouping is enabled and the catalog supports skip.
Neither settings reprojection nor switching back to Original refetches catalogs.

Virtual season opening context lives in `src/app/catalog_detail.rs`. It binds the
exact source owner and generation, current profile/open token, metadata revision,
semantic mapping fingerprint and complete ordered native episode inventory.
Corroborating provider provenance remains recorded separately and does not make an
otherwise identical mapping stale. Display/view
filtering does not truncate `modal.videos`, stored metadata or stream identities.
Scoped Watch Now, watch-state and downloads use confirmed native episode IDs;
ordinary next-episode traversal and tracking still see the full native inventory.
New, missing or conflicting metadata invalidates the scoped view and leaves an
explicit All seasons escape rather than silently choosing a different episode.
Shared reference-counted source/evidence objects keep virtual cards from copying
a long combined show's metadata once per season.

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

## Image processing and cache contract

The cache key combines the source URL with the resolved processing recipe.
Detail poster, detail backdrop, episode thumbnail and player episode image
uses each have an optional longest-side limit (defaults: 480, 960, 360 and
Original). Settings offer Original and 240, 360, 480, 720, 960, 1024, 1280,
1920 and 2560 pixel limits. Equal recipes share decoded entries even when the
consumer names differ. The decoded LRU is byte-budgeted.

Each disk variant is one atomic `.variant` file containing `NVIMAGE2` magic, a
length-prefixed JSON header and processed encoded bytes. Its versioned header
records the source URL, registered uses, dimensions, resolved recipe and an
encoded-payload checksum. There are no separate metadata/use files or
original-image backup. Downloaded source pixels are transient and are resized
before reaching Slint. Compression writes JPEG or WebP at the selected quality,
falling back to PNG when JPEG cannot retain alpha; resizing without compression
writes PNG, while Original with compression off keeps source bytes. Disabling
disk caching makes the cache memory-only. Concurrent requests
for the same recipe coalesce. A fresh result is encoded and decoded at the
selected quality before publication so visible pixels match the disk variant.
Writes replace variants atomically and preserve the old valid file on failure.
Changing a recipe fetches its source again, so a smaller stored variant is
never upscaled. Per-URL locks serialize disk changes. Legacy `.img`/`.cfg`
files are invalidated only after a successful new write, with no startup sweep.
Clear advances a cache generation, and loads capture the image-settings
revision, so stale work cannot repopulate cleared cache state or publish pixels
for superseded settings.

The resolution controls autosave immediately and work independently of disk
caching and compression. The re-encode command snapshots settings, fetches
each registered source URL once, and reconstructs its registered uses under
current recipes; it supports cancellation, and any source may need to download
again. Per-entry failures do not discard the previous valid variant. Clear
cache removes every image variant file and its usage metadata. Home retains
its fixed image behavior: original-size showcase artwork and logos, and 480px
poster cards.

## Synced series opening whole-series streams

A separate cold-cache issue affected saved Kitsu entries. AIOMetadata accepts
`kitsu:1415` for Code Geass but returns `tt0994314`, explicitly declaring
`_kitsuId: "1415"`. The strict response-ID guard rejected those valid episode
lists. Desktop's local episode cache hid the failure; a synced Android install
without that cache fell back to streams for the whole series.

The shared Stremio transport now retains the requested ID when the response's
explicit, nonconflicting catalog aliases prove both identities and its media
type matches. This applies to Kitsu, IMDb, MAL, AniList and typed TMDB aliases,
including common underscored fields. It preserves canonical catalog evidence
in the existing `novaExternalIds` extension and leaves episode IDs, saved IDs
and watch progress unchanged. Opaque unrelated IDs, conflicting aliases and
wrong media types still fail validation. Normalization also runs inside nested
broker requests before the recursion guard skips optional enrichment.

An empty or seasonless episode cache is treated as missing when opening Detail,
so fresh metadata gets its first episode-picker population rather than keeping
an accidental stream fallback. No library migration or cache clearing is needed.

## Standalone season entries with relative numbering

AIOMetadata's `kitsu:50024` entry is titled `Black Clover Season 2`, but its
26 episode rows use season 1. AniKoto has a separately labelled Season 2 entry
with its first episode available. The source search found that entry; the
shared mapper rejected its available prefix because it compared the metadata's
relative season number directly with the source's explicit season label.

The mapper now uses an explicit later-season title as alignment context when
regular episode counts prove a single-season entry and its absolute numbering
starts at episode 1. Source family, season, part, release context and ambiguity
checks still apply. Only available source episodes map; saved IDs and displayed
metadata numbering remain unchanged. The same rule serves forward stream
lookup and reverse metadata enrichment. Matcher fingerprints and AniKoto's
session lookup cache are versioned to discard older confirmations. A live
lookup of the unchanged Season 2 episode-1 request returned three source options
after the correction.

## Pipeline and ownership

| Stage | Implementation | Contract |
|---|---|---|
| Installed sources | `src/app/addon_mgr.rs`, `crates/providers/src/metadata.rs` | Enabled, available addon manifests form an immutable metadata snapshot. Its canonicalized fingerprint invalidates aliases/cache results when inventory changes. |
| Catalog | `src/app/{catalog,home}.rs`, `crates/providers/src/registry.rs`, provider JS | Catalog responses may omit optional fields. Bundled JS is converted to the same `MetaPreview` contract as ordinary addons. Home retains the originating catalog URL for opaque-ID ownership. |
| Home hydration | `src/app/home.rs` | Native/cached artwork paints immediately. Missing header fields load through ordinary meta endpoints, with at most two detail chains active and four endpoint candidates per chain. Originating providers are preferred; opaque IDs are sent only to their owner. Recognized global IDs may fall back to other accepting manifests. |
| Detail and prefetch | `src/app/{detail,catalog}.rs` | Detail paints cached content first and refreshes metadata on workers for series and movies; movie metadata runs independently of stream discovery. Responses must match both requested ID and media type before being cached/applied; transport normalization accepts a returned canonical ID only when explicit, nonconflicting aliases prove the requested identity. Empty or seasonless caches do not count as a visible episode picker. Discover browse/search reuse the last decoded Detail poster or a saved Library poster; enabled metadata prefetch also decodes newly discovered posters for missing grid art. Discover's prefetch preference does not disable Home hydration or Library prefetch. |
| Library mapping refresh | `src/app/{library,catalog,addon_mgr}.rs` | Opening Library fills missing episode/header metadata through the same enrichment broker as Detail; movies need header metadata only. Complete cached entries keep their cache. Missing/failed posters independently run mapping and retain the enriched native header/episode aliases, even if old metadata caches are populated. Displayed sort/filter order gets priority. Shared prefetch state coalesces queued requests and retries missing metadata after five minutes on success, one minute on failure, or a changed addon inventory. An open Library retries when manifests become available. Native saved/playback IDs and watch history remain intact. |
| Grid artwork repair | `src/app/{posters,catalog,run}.rs` | Missing or failed posters in Discover browse/search, My Library and Home retry enabled metadata endpoints through the same enrichment transport as Detail, independently of episode prefetch or cached episodes/header text. Episodes with an unusable poster do not stop recovery: try the next endpoint. Follow current nonconflicting broker connections to alternate IDs only at their owning enabled metadata addons, keeping the original Library/playback ID. A visited-endpoint budget (16) prevents alias cycles. Pending manifests do not spend cooldowns; when manifests become usable, only queued missing URLs or confirmed download failures retry. Unloaded rows do not trigger speculative enrichment. A distinct cached poster is tried before metadata endpoints. Library visits skip loaded posters and coalesce pending image loads. For saved native identities, also cache the enriched header/episode aliases and update the existing Continue/Upcoming publication path. Clearing image cache then failing to download a saved URL invokes this repair, including inside Detail. Persist and publish only decoded replacement URLs to matching grids and a matching Detail view. |
| Optional enrichment | `crates/providers/src/metadata.rs`, `crates/media/src/net.rs` | Resolve explicit external IDs first, expand discovered IDs, then search remaining addon catalogs by normalized title/family. Already connected addons need no speculative title search. Candidate detail responses must preserve requested identity. Tied weak family matches are evaluated together within the six-detail budget; exactly one must prove at least two available episode mappings, and every competitor must answer successfully (an empty Stremio detail response counts as no candidate; errors do not). Exact-title/remake ties remain unresolved. |
| Episode alignment | `crates/providers/src/{sequence,matching}.rs` | Match native available episodes to external episodes using IDs, release context, sequence/count evidence and unique title anchors. Split seasons/cours and continuous numbering share one mapper. A contiguous available prefix may match a larger canonical season only for an exact family, the same season and its first part, with no known later source season/part or contradictory declared total. A shorter declared first-cour total is allowed when only a contiguous aired prefix has servers; only episodes actually available at the source map. Repeated generic episode prefixes are placeholders, not title evidence. Gaps, remakes, duplicate editions and conflicting identities reject speculative aliases. |
| Field selection | `crates/providers/src/metadata.rs` | Keep native playback IDs and availability. Treat blank strings as absent. Select display fields only from accepted, nonconflicting connections. Later accepted providers may fill missing thumbnails/overview without replacing the first accepted display numbering. Duplicate external aliases retain independent provenance until conflicts are checked, then are deduplicated. |
| Cinemeta correction | `crates/providers/src/metadata/cinemeta.rs` | Its existing official-endpoint repair uses one-to-one episode-title/date evidence to correct image numbering while retaining native episode IDs. Confirmed corrections survive failed, malformed, unrelated and partial refreshes; new confirmed images take precedence. |
| UI publication | `src/app/{home,detail,posters}.rs` | Slint updates run on the event loop. Home workers retain image identity/load/failure state; Slint owns live, current and crossfade pixels, and reacquires evicted images with generation, identity and revision guards. Detail has its own recipe-scoped requests and cannot overwrite a different grid use. Detail uses opening tokens and stream generations. Successful Detail poster loads publish to matching Discover browse/search and Library rows, retaining corrected URLs through scroll unloading. Grid image completions check current URLs before painting or caching pixels. Home featured backdrop/logo and Detail episode/season completions deliver their pixels directly to the current Slint model without requiring decoded-cache residency. Home parks logo completion pixels only for the explicit pending slide; matching current/pending typed identities survive LRU eviction without a duplicate fetch and cannot paint a different title. Inactive prefetches keep metadata only. A fallback completion paints directly and still pursues a preferred background introduced during loading; unchanged failed-background fallbacks do not restart the fetch loop. Remote image-recipe edits rebuild shown page identities and requeue current Detail artwork once after the sync batch; unchanged revisions start no extra work. Detail constructs and requests only the current filtered 50-episode page; stable requested/displayed identities preserve live pixels across model refreshes. Episode/season jobs share a four-worker pool, opening-token and current-URL/recipe/revision guards, and job-ID cancellation that cannot unregister another request. |
| Calendar/Downloads artwork | `src/app/{calendar,downloads_page,page_artwork}.rs`, `crates/ui/{calendar,downloads-page,appwindow}.slint` | Measured card/header visibility preloads one viewport above/below, including nested Calendar day scrolling. A shared four-slot queue deduplicates URL/resolved-recipe requests and holds each slot until all completion subscribers paint or discard their shared pixel buffer, including dispatch failure; cancelled and cooling failures never block later visible candidates. View generations and active/current-identity guards reject stale completions. Direct delivery paints successful pixels even with zero decoded-cache budget; offscreen/inactive model images are released. Modal/player overlays and submenu returns deactivate jobs even if destination flags remain set; overlay exit reactivates existing views for the recreated delegates. No persistent state or extra retained pixel cache. |
| Image delivery | `crates/media/src/{net,cache}.rs`, `src/app/posters.rs` | A byte-budgeted decoded LRU and processed disk variants use URL + resolved recipe keys; matching recipes share entries and same-recipe work coalesces. Each `.variant` atomically contains `NVIMAGE2`, a length-prefixed versioned JSON header (URL, registered use vector, dimensions, resolved recipe, payload checksum), and encoded bytes. No sidecar metadata/use files or original backup are retained. Per-URL locks, clear-generation and settings-revision guards protect concurrent changes. Transient per-recipe publication tickets order successful loads, forced refreshes and maintenance regardless of decoded-LRU residency; maintenance registration snapshots also reject uses rebuilt since capture, including settings changes away and back. These guards change no disk envelope or storage schema. Source pixels are transient and resized before Slint. JPEG/WebP compression falls back to PNG for alpha; resizing without compression writes PNG, while Original/no-compression keeps source bytes. Disk-off is memory-only. Desktop and Android downloads reject HTTP errors and retry transient failures. The root app retains poster priority scheduling and at most four episode-thumbnail workers; the removed app-level PosterStore is not a cache owner. |
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
| Page artwork request state | Calendar/Downloads transient URL/recipe identity, view generation, retained geometry and shared four-worker queue | Failed URLs cool for 30 seconds; retained delegates retry without blocking later rows. Page/overlay deactivation cancels pending delivery and releases model images; reactivation reschedules the existing model without requiring a data rebuild. |
| Images | URL + resolved recipe; processed `.variant` envelope plus byte-budgeted decoded memory entries. Versioned JSON metadata and registered use vector share an atomic file with encoded bytes; no metadata/use sidecars or original backup exist. | Disk caching can be disabled independently of metadata persistence, leaving a memory-only cache. Failures keep the previous valid file and displayed pixels. Recipe changes redownload source bytes and never upscale a processed variant; successful fresh encodes are decoded back at selected quality before display. Per-URL locks, clear-generation/settings-revision guards, publication tickets and maintenance registration watches reject stale changes. Publication tickets retain newer pixels only while older loads or refreshes need them; queued maintenance retains ordering metadata only; successful visible delivery does not depend on an LRU hit. Legacy cache invalidation happens on successful access/replacement only, not at startup. |

## Reliability boundaries and follow-ons

“Always” must mean that missing external data does not break native browsing or
replace correct identity with a guess. No client can guarantee an image that a
remote provider does not expose, a site that is unavailable, or an unambiguous
mapping when the available evidence conflicts. Keep the native poster/metadata,
reuse confirmed cache data where the fingerprint still agrees, and retry bounded
work when the user revisits or rotates the relevant content.

The remaining architectural improvements are:

- **Shared metadata scheduling and cancellation.** Home bounds active detail chains and
  Detail episode/season thumbnails share a four-worker pump, and
  Calendar/Downloads share a separate four-worker visibility queue. Catalog and
  Library prefetch now
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
  a richer artwork request contract. Featured images can retain source resolution
  in live/crossfade Slint images;
  these UI images, transient worker buffers and GPU allocations sit outside the
  decoded LRU budget. Home request bookkeeping holds identities, not a retained
  per-slide pixel cache.
- **Legacy opaque-ID storage.** Home distinguishes opaque IDs by catalog owner,
  while Library/header/episode persistence is still fundamentally keyed by
  stable media IDs. Adapters must namespace private IDs; supporting colliding
  unnamespaced IDs across independent addons needs a storage migration.

## Regression coverage

- `src/app/detail.rs::series_open_tests`: Discover and Library opens with missing, empty and populated caches; an explicitly aliased canonical response shows episodes, preserves watched progress and fetches streams only after an episode is selected.
- `crates/providers/src/stremio.rs::identity_tests`: proven Kitsu/MAL/AniList/TMDB response aliases preserve native requests and episode IDs; unproven/conflicting aliases, unrelated IDs and wrong media types are rejected.

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
- `crates/providers/src/sequence.rs::tests`: available prefixes, duplicate placeholders, standalone later seasons numbered as season 1, shared forward/reverse alignment, holes, split parts, wrong seasons/years and conflicting titles.
- `crates/providers/src/registry_tests.rs`: the complete AniKoto lookup path resolves a standalone Season 2 entry's S1 E1 to the available native episode, preserves the caller's identity and rejects the unavailable next episode.
- `crates/providers/src/metadata/cinemeta.rs::tests`: shifted numbering, special
  classification, date/title ambiguity, stable identities and partial refreshes.
- `crates/media/src/cache_tests.rs`: corrupt-cache recovery, atomic/format
  handling, valid resized pixel-buffer dimensions (including 680 × 1000 TVDB
  posters), sized images and an HTTP 503 → successful PNG download.
- `src/app/page_artwork.rs::tests` and `src/app/downloads/page.rs::tests`: later rows beyond 80, URL/recipe/view revision rejection, inactive/offscreen release, queue deduplication/four-slot transport-and-delivery cap/shared-buffer fanout/cooldowns and replacement of nonempty cached group artwork. `tests/downloads/page_artwork_visibility.rs` checks real native scrolling through 150 Calendar/Downloads rows at phone/desktop widths and Detail/player/More overlay exit.
- `src/app/home.rs::tests::featured_logo_pending_and_current_pixels_survive_cache_eviction`: an isolated cache-hit fixture parks the pending logo, evicts decoded pixels before commit, retains the matching current logo without refetching, and releases obsolete target/recipe ownership.
- Existing Home headless tests cover card layout, featured crossfade/rotation,
  paging gestures and carousel behavior independently of external servers.

## Episode coverage across main-series refreshes

The shared broker supplements regular episodes only when another enabled addon
explicitly identifies the same IMDb or TMDB TV parent. Coverage reconciliation
uses IDs, confirmed aliases and unambiguous episode alignment, rather than
assuming an absent season label means new content. A 24-episode first season
and two 12-episode seasons describe the same coverage; the second layout must
not add duplicate cards. An incomplete existing season can gain missing episodes
once its common prefix is confirmed. Donor seasons require contiguous numbering
from episode one; standalone season/cour entries, bundled native lists and
specials are not expanded. Conflicting identity claims are excluded.

`metadata/coverage.rs` also reconciles fresh `MetaItem` data against the existing
app episode cache. Saved episode IDs and display numbering anchor watch progress,
while fresh native IDs become confirmed stream aliases with source ownership.
Shorter/empty responses retain known regular episodes instead of deleting them;
meaningful text, artwork and release dates still refresh. Ambiguous additions
are withheld, and stale routing aliases are stripped after inventory changes.
No progress records or sync schemas are rewritten.

Detail, Home hydration and prefetch use the same cache writer and merged list.
Successful Detail refreshes also update the open episode inventory for ended
shows. An open stream view or a restored later season keeps its navigation
while receiving the refreshed list, so visible episode rows and Library
completion use the same coverage. Metadata corrections do not change watched
flags.
The existing episode cache provides continuity when a metadata response changes
its fingerprint or when a primary provider starts supplying a season previously
added from another provider. Confirmed supplemental mappings remain in the
bounded local metadata cache with a five-minute refresh lifetime; partial donor
refreshes merge episode by episode. The enrichment version invalidates older
merge results without clearing episode lists or watch history. An ongoing donor
also prevents an older provider's ended status from freezing the combined list.
